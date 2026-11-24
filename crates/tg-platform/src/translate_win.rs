//! Windows platform offline translation provider and trait abstraction.
//!
//! Port origin:
//! - `Telegram/SourceFiles/platform/platform_translate_provider.h`
//! - `Telegram/SourceFiles/platform/win/translate_provider_win.h`
//! - `Telegram/SourceFiles/lang/translate_provider.cpp`

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Translation request passed to a [`TranslateProvider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateRequest {
    /// UTF-8 text to translate.
    pub text: String,
    /// Optional BCP-47 or ISO-639-1 source language hint (e.g. `"es"`, `"de-DE"`).
    pub from_lang: Option<String>,
    /// Target BCP-47 or ISO-639-1 language code (e.g. `"en"`).
    pub to_lang: String,
}

impl TranslateRequest {
    /// Creates a new translation request for `text` into `to_lang`.
    #[must_use]
    pub fn new(text: impl Into<String>, to_lang: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            from_lang: None,
            to_lang: to_lang.into(),
        }
    }

    /// Attaches an explicit source language hint.
    #[must_use]
    pub fn with_from_lang(mut self, from_lang: impl Into<String>) -> Self {
        self.from_lang = Some(from_lang.into());
        self
    }
}

/// Successful translation result returned by a [`TranslateProvider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateResponse {
    /// Translated UTF-8 text.
    pub translated_text: String,
    /// Source language detected by the provider (if known).
    pub detected_source_lang: Option<String>,
    /// Human-readable provider identifier (e.g. `"winrt-offline"`).
    pub provider_name: &'static str,
}

/// Errors returned by [`TranslateProvider`] and [`WinRtTranslateBridge`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    /// Platform offline translation is not available on this OS/installation.
    #[error("Windows offline translation provider is unavailable")]
    Unavailable,
    /// The input text was empty or whitespace-only.
    #[error("cannot translate empty text")]
    EmptyInput,
    /// The requested source or target language code is invalid or unsupported.
    #[error("unsupported translation language: {lang}")]
    UnsupportedLanguage {
        /// The offending language code.
        lang: String,
    },
    /// Underlying platform or bridge failure.
    #[error("translation backend error: {0}")]
    Backend(String),
}

/// Normalizes a BCP-47 language tag (e.g. `"en-US"`, `"pt_BR"`) into its lowercase base ISO code
/// (`"en"`, `"pt"`).
#[must_use]
pub fn normalize_language_code(code: &str) -> String {
    let trimmed = code.trim();
    trimmed
        .split(['-', '_'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

/// Checks whether `code` looks like a well-formed BCP-47 / ISO-639 language tag.
#[must_use]
pub fn is_valid_language_tag(code: &str) -> bool {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.len() > 35 {
        return false;
    }
    let mut parts = trimmed.split(['-', '_']);
    let Some(primary) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|sub| {
        !sub.is_empty() && sub.len() <= 8 && sub.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// Platform translation provider interface mirroring `Ui::TranslateProvider`
/// (`platform/platform_translate_provider.h`).
pub trait TranslateProvider: Send + Sync {
    /// Short identifier of this provider (e.g. `"winrt-offline"`).
    fn name(&self) -> &'static str;

    /// Whether this provider is available and ready to translate on the current host.
    fn is_available(&self) -> bool;

    /// Whether `lang_code` (ISO-639 or BCP-47) is supported by this provider.
    fn supports_language(&self, lang_code: &str) -> bool;

    /// Translates `request` synchronously (offline / local provider).
    fn translate(&self, request: &TranslateRequest) -> Result<TranslateResponse, TranslateError>;
}

/// Low-level Windows WinRT translation bridge so callers and tests can substitute a deterministic
/// fake without invoking real OS services or network APIs.
pub trait WinRtTranslateBridge: Send + Sync {
    /// Probes whether Windows offline translation packs/services are available.
    fn probe_available(&self) -> bool;

    /// Returns installed BCP-47 language tags recognized by Windows Globalization.
    fn installed_language_tags(&self) -> Vec<String>;

    /// Executes an offline translation via the underlying Windows bridge.
    fn translate_offline(
        &self,
        text: &str,
        from_lang: Option<&str>,
        to_lang: &str,
    ) -> Result<(String, Option<String>), TranslateError>;
}

/// Production Windows WinRT bridge matching `platform/win/translate_provider_win.h`.
///
/// Windows Desktop (non-Store / standard Win32) does not ship a universal offline WinRT
/// translation engine comparable to Apple's `Translation` framework, so `probe_available`
/// reports `false` by default (matching `Platform::IsTranslateProviderAvailable() { return false; }`
/// in `translate_provider_win.h`), while still using `Windows.Globalization.Language` to validate
/// BCP-47 tags and enumerate the user's configured locale when queried.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWinRtTranslateBridge;

impl WinRtTranslateBridge for SystemWinRtTranslateBridge {
    fn probe_available(&self) -> bool {
        false
    }

    fn installed_language_tags(&self) -> Vec<String> {
        #[cfg(windows)]
        {
            let mut tags = Vec::new();
            let sys = crate::system_info::system_language();
            if !sys.is_empty() && is_valid_language_tag(&sys) {
                tags.push(sys);
            }
            if tags.is_empty() {
                tags.push("en-US".to_owned());
            }
            tags
        }
        #[cfg(not(windows))]
        {
            vec!["en-US".to_owned()]
        }
    }

    fn translate_offline(
        &self,
        _text: &str,
        _from_lang: Option<&str>,
        _to_lang: &str,
    ) -> Result<(String, Option<String>), TranslateError> {
        Err(TranslateError::Unavailable)
    }
}

/// Windows translation provider backed by a pluggable [`WinRtTranslateBridge`].
#[derive(Debug)]
pub struct WinTranslateProvider<B: WinRtTranslateBridge = SystemWinRtTranslateBridge> {
    bridge: B,
}

impl WinTranslateProvider<SystemWinRtTranslateBridge> {
    /// Creates a provider backed by the default [`SystemWinRtTranslateBridge`].
    #[must_use]
    pub const fn system() -> Self {
        Self {
            bridge: SystemWinRtTranslateBridge,
        }
    }
}

impl<B: WinRtTranslateBridge> WinTranslateProvider<B> {
    /// Creates a provider backed by a custom or fake [`WinRtTranslateBridge`].
    #[must_use]
    pub const fn with_bridge(bridge: B) -> Self {
        Self { bridge }
    }

    /// Returns a reference to the underlying bridge.
    #[must_use]
    pub const fn bridge(&self) -> &B {
        &self.bridge
    }
}

impl<B: WinRtTranslateBridge> TranslateProvider for WinTranslateProvider<B> {
    fn name(&self) -> &'static str {
        "winrt-offline"
    }

    fn is_available(&self) -> bool {
        self.bridge.probe_available()
    }

    fn supports_language(&self, lang_code: &str) -> bool {
        if !is_valid_language_tag(lang_code) {
            return false;
        }
        let target = normalize_language_code(lang_code);
        self.bridge
            .installed_language_tags()
            .iter()
            .any(|tag| normalize_language_code(tag) == target)
    }

    fn translate(&self, request: &TranslateRequest) -> Result<TranslateResponse, TranslateError> {
        if request.text.trim().is_empty() {
            return Err(TranslateError::EmptyInput);
        }
        if !is_valid_language_tag(&request.to_lang) {
            return Err(TranslateError::UnsupportedLanguage {
                lang: request.to_lang.clone(),
            });
        }
        if let Some(ref from) = request.from_lang
            && !from.trim().is_empty()
            && !is_valid_language_tag(from)
        {
            return Err(TranslateError::UnsupportedLanguage { lang: from.clone() });
        }
        if !self.is_available() {
            return Err(TranslateError::Unavailable);
        }
        if !self.supports_language(&request.to_lang) {
            return Err(TranslateError::UnsupportedLanguage {
                lang: request.to_lang.clone(),
            });
        }

        let norm_to = normalize_language_code(&request.to_lang);
        let norm_from = request
            .from_lang
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(normalize_language_code);
        let (translated_text, detected_source_lang) =
            self.bridge
                .translate_offline(&request.text, norm_from.as_deref(), &norm_to)?;

        Ok(TranslateResponse {
            translated_text,
            detected_source_lang: detected_source_lang.or(norm_from),
            provider_name: self.name(),
        })
    }
}

/// Returns whether the native Windows offline translation provider is available
/// (`Platform::IsTranslateProviderAvailable()` in `translate_provider_win.h`).
#[must_use]
pub fn is_translate_provider_available() -> bool {
    WinTranslateProvider::system().is_available()
}

/// Creates the native Windows translation provider if available
/// (`Platform::CreateTranslateProvider()` in `translate_provider_win.h`).
#[must_use]
pub fn create_translate_provider() -> Option<Box<dyn TranslateProvider>> {
    let provider = WinTranslateProvider::system();
    if provider.is_available() {
        Some(Box::new(provider))
    } else {
        None
    }
}

/// Deterministic in-memory fake [`WinRtTranslateBridge`] for unit and integration tests.
#[derive(Debug, Clone)]
pub struct FakeWinTranslateBridge {
    available: bool,
    installed_languages: Vec<String>,
    canned: BTreeMap<(String, String), String>,
    fail_unknown: bool,
    recorded_calls: Arc<Mutex<Vec<TranslateRequest>>>,
}

impl Default for FakeWinTranslateBridge {
    fn default() -> Self {
        Self::new(
            true,
            vec!["en-US".to_owned(), "es-ES".to_owned(), "de-DE".to_owned()],
        )
    }
}

impl FakeWinTranslateBridge {
    /// Creates a fake bridge with explicit availability and supported BCP-47 language tags.
    #[must_use]
    pub fn new(available: bool, installed_languages: Vec<String>) -> Self {
        Self {
            available,
            installed_languages,
            canned: BTreeMap::new(),
            fail_unknown: false,
            recorded_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Registers a canned translation output for `(to_lang, source_text)`.
    pub fn insert_translation(
        &mut self,
        to_lang: impl Into<String>,
        source_text: impl Into<String>,
        translated: impl Into<String>,
    ) {
        let norm_to = normalize_language_code(&to_lang.into());
        self.canned
            .insert((norm_to, source_text.into()), translated.into());
    }

    /// Makes texts without a canned translation fail with [`TranslateError::Backend`] instead
    /// of returning a `"[lang] text"` echo (a provider that cannot translate one message).
    #[must_use]
    pub const fn with_fail_unknown(mut self, fail_unknown: bool) -> Self {
        self.fail_unknown = fail_unknown;
        self
    }

    /// Returns all translation requests recorded by this fake bridge.
    #[must_use]
    pub fn recorded_calls(&self) -> Vec<TranslateRequest> {
        self.recorded_calls
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl WinRtTranslateBridge for FakeWinTranslateBridge {
    fn probe_available(&self) -> bool {
        self.available
    }

    fn installed_language_tags(&self) -> Vec<String> {
        self.installed_languages.clone()
    }

    fn translate_offline(
        &self,
        text: &str,
        from_lang: Option<&str>,
        to_lang: &str,
    ) -> Result<(String, Option<String>), TranslateError> {
        if let Ok(mut calls) = self.recorded_calls.lock() {
            calls.push(TranslateRequest {
                text: text.to_owned(),
                from_lang: from_lang.map(str::to_owned),
                to_lang: to_lang.to_owned(),
            });
        }
        if !self.available {
            return Err(TranslateError::Unavailable);
        }
        let norm_to = normalize_language_code(to_lang);
        let key = (norm_to.clone(), text.to_owned());
        let translated = match self.canned.get(&key) {
            Some(canned) => canned.clone(),
            None if self.fail_unknown => {
                return Err(TranslateError::Backend(format!(
                    "no offline translation into {norm_to}"
                )));
            }
            None => format!("[{norm_to}] {text}"),
        };
        let detected = from_lang
            .map(normalize_language_code)
            .or_else(|| Some("auto".to_owned()));
        Ok((translated, detected))
    }
}

/// Translates `request` trying `platform` first when `use_platform_translation` is enabled,
/// falling back to `fallback` whenever the platform provider is disabled, unavailable, or does
/// not support the requested language (`lang/translate_provider.cpp`).
pub fn translate_with_fallback<F>(
    request: &TranslateRequest,
    use_platform_translation: bool,
    platform: Option<&dyn TranslateProvider>,
    fallback: F,
) -> Result<TranslateResponse, TranslateError>
where
    F: FnOnce(&TranslateRequest) -> Result<TranslateResponse, TranslateError>,
{
    if use_platform_translation
        && let Some(provider) = platform
        && provider.is_available()
    {
        match provider.translate(request) {
            Ok(resp) => return Ok(resp),
            Err(
                TranslateError::Unavailable
                | TranslateError::UnsupportedLanguage { .. }
                | TranslateError::Backend(_),
            ) => {}
            Err(err) => return Err(err),
        }
    }
    fallback(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_provider_matches_tdesktop_unavailable() {
        assert!(!is_translate_provider_available());
        assert!(create_translate_provider().is_none());
        let sys = WinTranslateProvider::system();
        assert_eq!(sys.name(), "winrt-offline");
        assert!(!sys.is_available());
        let err = sys
            .translate(&TranslateRequest::new("Hola", "en"))
            .expect_err("system bridge is unavailable");
        assert_eq!(err, TranslateError::Unavailable);
    }

    #[test]
    fn fake_bridge_translates_and_validates_languages() {
        let mut fake = FakeWinTranslateBridge::new(true, vec!["en-US".into(), "de-DE".into()]);
        fake.insert_translation("en", "Hallo Welt", "Hello World");
        let provider = WinTranslateProvider::with_bridge(fake.clone());

        assert!(provider.is_available());
        assert!(provider.supports_language("en"));
        assert!(provider.supports_language("en-US"));
        assert!(!provider.supports_language("ja"));
        assert!(!provider.supports_language("12345"));

        let resp = provider
            .translate(&TranslateRequest::new("Hallo Welt", "en-US").with_from_lang("de-DE"))
            .expect("canned translation");
        assert_eq!(resp.translated_text, "Hello World");
        assert_eq!(resp.detected_source_lang.as_deref(), Some("de"));
        assert_eq!(resp.provider_name, "winrt-offline");
        assert_eq!(fake.recorded_calls().len(), 1);

        assert_eq!(
            provider.translate(&TranslateRequest::new("   ", "en")),
            Err(TranslateError::EmptyInput)
        );
        assert_eq!(
            provider.translate(&TranslateRequest::new("Hallo", "ja")),
            Err(TranslateError::UnsupportedLanguage { lang: "ja".into() })
        );
    }

    #[test]
    fn fail_unknown_bridge_errors_and_fallback_takes_over() {
        let mut fake = FakeWinTranslateBridge::new(true, vec!["en".into()]).with_fail_unknown(true);
        fake.insert_translation("en", "Hola", "Hello");
        let provider = WinTranslateProvider::with_bridge(fake);
        assert_eq!(
            provider
                .translate(&TranslateRequest::new("Hola", "en"))
                .map(|r| r.translated_text),
            Ok("Hello".to_owned())
        );
        assert!(matches!(
            provider.translate(&TranslateRequest::new("Adios", "en")),
            Err(TranslateError::Backend(_))
        ));
        let resp = translate_with_fallback(
            &TranslateRequest::new("Adios", "en"),
            true,
            Some(&provider),
            |req| {
                Ok(TranslateResponse {
                    translated_text: format!("server:{}", req.text),
                    detected_source_lang: None,
                    provider_name: "mtproto",
                })
            },
        )
        .expect("fallback");
        assert_eq!(resp.provider_name, "mtproto");
    }

    #[test]
    fn fallback_chain_prefers_platform_then_falls_back_to_mtproto() {
        let mut fake = FakeWinTranslateBridge::new(true, vec!["en-US".into()]);
        fake.insert_translation("en", "Bonjour", "Hello");
        let provider = WinTranslateProvider::with_bridge(fake);

        let mtproto = |req: &TranslateRequest| {
            Ok(TranslateResponse {
                translated_text: format!("mtproto:{}:{}", req.to_lang, req.text),
                detected_source_lang: Some("fr".into()),
                provider_name: "mtproto",
            })
        };

        // Platform enabled + supported language -> uses WinRT fake.
        let r1 = translate_with_fallback(
            &TranslateRequest::new("Bonjour", "en"),
            true,
            Some(&provider),
            mtproto,
        )
        .expect("platform hit");
        assert_eq!(r1.provider_name, "winrt-offline");
        assert_eq!(r1.translated_text, "Hello");

        // Platform enabled + unsupported target language -> transparently falls back to MTProto.
        let r2 = translate_with_fallback(
            &TranslateRequest::new("Bonjour", "uk"),
            true,
            Some(&provider),
            mtproto,
        )
        .expect("fallback on unsupported lang");
        assert_eq!(r2.provider_name, "mtproto");
        assert_eq!(r2.translated_text, "mtproto:uk:Bonjour");

        // Platform disabled -> goes straight to MTProto.
        let r3 = translate_with_fallback(
            &TranslateRequest::new("Bonjour", "en"),
            false,
            Some(&provider),
            mtproto,
        )
        .expect("fallback when disabled");
        assert_eq!(r3.provider_name, "mtproto");
    }
}
