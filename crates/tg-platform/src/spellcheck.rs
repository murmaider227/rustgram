//! Native Windows spellchecking via `ISpellChecker` / `ISpellCheckerFactory`.
//!
//! Port of `Telegram/lib_spellcheck/spellcheck/platform/win/spellcheck_win.cpp`.

use crate::error::Result;

/// Default maximum number of suggestions returned (`kMaxSuggestions` in tdesktop).
pub const DEFAULT_MAX_SUGGESTIONS: usize = 5;

/// Chunk size in UTF-16 code units used to avoid Windows 10 `std::regex_error::error_complexity`
/// on long texts (`kChunk` in `spellcheck_win.cpp`).
pub const CHECK_TEXT_CHUNK_UTF16_LEN: usize = 5000;

/// Maximum word size used when backtracking to a word boundary during chunking.
pub const MAX_WORD_SIZE: usize = 64;

/// A misspelled word span returned by [`SpellChecker::check_text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MisspelledRange {
    /// Start offset in UTF-16 code units (matching Windows `ISpellingError::StartIndex`).
    pub utf16_start: usize,
    /// Length in UTF-16 code units (matching Windows `ISpellingError::Length`).
    pub utf16_len: usize,
    /// Start offset in UTF-8 bytes inside the input `&str`.
    pub byte_start: usize,
    /// Length in UTF-8 bytes inside the input `&str`.
    pub byte_len: usize,
}

/// Replace Unicode paragraph separator (U+2029) with ASCII space to avoid a native
/// Windows spellchecker crash (`spellcheck_win.cpp` lines 481-484).
pub fn sanitize_for_spellcheck(text: &str) -> String {
    text.replace('\u{2029}', " ")
}

/// Split `text` into chunks of at most `max_utf16` UTF-16 code units, breaking on
/// non-alphanumeric boundaries within `max_word_size` characters of the chunk edge.
///
/// Mirrors `WindowsSpellChecker::chunkedCheckSpellingText` in `spellcheck_win.cpp`.
pub(crate) fn split_text_chunks(
    text: &str,
    max_utf16: usize,
    max_word_size: usize,
) -> Vec<(usize, usize, &str)> {
    let mut chunks = Vec::new();
    let mut byte_pos = 0usize;
    let mut utf16_pos = 0usize;

    while byte_pos < text.len() {
        let rest = &text[byte_pos..];
        let mut count_utf16 = 0usize;
        let mut provisional_end_byte = 0usize;
        let mut boundaries: Vec<(usize, usize, bool)> = Vec::new();

        for (rel_byte, ch) in rest.char_indices() {
            let u16_len = ch.len_utf16();
            if count_utf16 + u16_len > max_utf16 && count_utf16 > 0 {
                break;
            }
            count_utf16 += u16_len;
            provisional_end_byte = rel_byte + ch.len_utf8();
            boundaries.push((provisional_end_byte, count_utf16, ch.is_alphanumeric()));
        }

        let (chosen_byte, chosen_utf16) = if byte_pos + provisional_end_byte >= text.len() {
            (provisional_end_byte, count_utf16)
        } else {
            let until_utf16 = count_utf16.saturating_sub(max_word_size);
            boundaries
                .iter()
                .rev()
                .take_while(|&&(_, u16_off, _)| u16_off > until_utf16)
                .find(|&&(_, _, is_alnum)| !is_alnum)
                .map(|&(b, u16_off, _)| (b, u16_off))
                .unwrap_or((provisional_end_byte, count_utf16))
        };

        let slice = &rest[..chosen_byte];
        chunks.push((utf16_pos, byte_pos, slice));
        byte_pos += chosen_byte;
        utf16_pos += chosen_utf16;
    }

    chunks
}

/// Convert a `(utf16_start, utf16_len)` range within `text` into a [`MisspelledRange`]
/// with accurate UTF-8 byte offsets.
pub(crate) fn utf16_range_to_misspelled(
    text: &str,
    utf16_start: usize,
    utf16_len: usize,
) -> Option<MisspelledRange> {
    let utf16_end = utf16_start.checked_add(utf16_len)?;
    let mut u16_cursor = 0usize;
    let mut byte_start = None;
    let mut byte_end = None;

    for (b_idx, ch) in text.char_indices() {
        if u16_cursor == utf16_start {
            byte_start = Some(b_idx);
        }
        u16_cursor += ch.len_utf16();
        if u16_cursor == utf16_end {
            byte_end = Some(b_idx + ch.len_utf8());
            break;
        }
    }
    if utf16_start == u16_cursor && utf16_len == 0 {
        byte_start = Some(text.len());
        byte_end = Some(text.len());
    }

    let bs = byte_start?;
    let be = byte_end?;
    Some(MisspelledRange {
        utf16_start,
        utf16_len,
        byte_start: bs,
        byte_len: be - bs,
    })
}

/// Returns `true` if the OS provides native system spellchecking (Windows 8+).
pub fn is_system_spellchecker_supported() -> bool {
    #[cfg(windows)]
    {
        crate::system_info::windows_version().is_windows_8_or_greater()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Enumerate all BCP-47 language tags supported by Windows `ISpellCheckerFactory`.
pub fn supported_languages() -> Result<Vec<String>> {
    #[cfg(windows)]
    {
        win_spell::supported_languages()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// Thread-safe wrapper around Windows `ISpellChecker` / `ISpellCheckerFactory`.
///
/// Internally runs all COM spellchecker objects on a dedicated COM MTA worker thread
/// (matching `ComThread` in `spellcheck_win.cpp`), because `ISpellChecker` is not thread-safe.
pub struct SpellChecker {
    #[cfg(windows)]
    inner: win_spell::SpellCheckerClient,
    #[cfg(not(windows))]
    added: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl std::fmt::Debug for SpellChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpellChecker")
            .field("active_languages", &self.active_languages())
            .finish()
    }
}

impl SpellChecker {
    /// Initialize a spellchecker using the system's installed spelling languages
    /// (`%APPDATA%\Microsoft\Spelling` + system UI language, matching `SystemLanguages()`).
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_spell::SpellCheckerClient::spawn(None)?,
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {
                added: std::sync::Mutex::new(std::collections::HashSet::new()),
            })
        }
    }

    /// Initialize a spellchecker with an explicit list of BCP-47 language tags (e.g. `&["en-US"]`).
    pub fn with_languages(languages: &[&str]) -> Result<Self> {
        #[cfg(windows)]
        {
            let langs: Vec<String> = languages.iter().map(|s| (*s).to_string()).collect();
            Ok(Self {
                inner: win_spell::SpellCheckerClient::spawn(Some(langs))?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = languages;
            Ok(Self {
                added: std::sync::Mutex::new(std::collections::HashSet::new()),
            })
        }
    }

    /// Return the BCP-47 language tags currently loaded by this spellchecker instance.
    pub fn active_languages(&self) -> Vec<String> {
        #[cfg(windows)]
        {
            self.inner.active_languages()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    /// Return all BCP-47 language tags supported by `ISpellCheckerFactory`.
    pub fn supported_languages(&self) -> Vec<String> {
        #[cfg(windows)]
        {
            self.inner.supported_languages()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    /// Check whether a single `word` is spelled correctly in at least one active language.
    ///
    /// Returns `true` if no active languages are loaded or if any language accepts `word`.
    pub fn check_word(&self, word: &str) -> bool {
        #[cfg(windows)]
        {
            self.inner.check_word(word)
        }
        #[cfg(not(windows))]
        {
            let _ = word;
            true
        }
    }

    /// Check a full text string and return all ranges misspelled in every active language.
    ///
    /// Automatically sanitizes U+2029 paragraph separators and chunks texts longer than
    /// [`CHECK_TEXT_CHUNK_UTF16_LEN`] UTF-16 code units.
    pub fn check_text(&self, text: &str) -> Vec<MisspelledRange> {
        #[cfg(windows)]
        {
            self.inner.check_text(text)
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Vec::new()
        }
    }

    /// Return up to `max_suggestions` replacement suggestions for a misspelled `word`.
    pub fn suggestions(&self, word: &str, max_suggestions: usize) -> Vec<String> {
        #[cfg(windows)]
        {
            self.inner.suggestions(word, max_suggestions)
        }
        #[cfg(not(windows))]
        {
            let _ = (word, max_suggestions);
            Vec::new()
        }
    }

    /// Add `word` to the user dictionary across all active languages.
    pub fn add_word(&self, word: &str) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.add_word(word)
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut set) = self.added.lock() {
                set.insert(word.to_string());
            }
            Ok(())
        }
    }

    /// Remove `word` from the user dictionary across all active languages (`ISpellChecker2::Remove`).
    pub fn remove_word(&self, word: &str) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.remove_word(word)
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut set) = self.added.lock() {
                set.remove(word);
            }
            Ok(())
        }
    }

    /// Ignore `word` for the lifetime of this spellchecker session (`ISpellChecker::Ignore`).
    pub fn ignore_word(&self, word: &str) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.ignore_word(word)
        }
        #[cfg(not(windows))]
        {
            let _ = word;
            Ok(())
        }
    }

    /// Check whether `word` was added via [`Self::add_word`] during this session.
    pub fn is_added_word(&self, word: &str) -> bool {
        #[cfg(windows)]
        {
            self.inner.is_added_word(word)
        }
        #[cfg(not(windows))]
        {
            self.added.lock().map(|s| s.contains(word)).unwrap_or(false)
        }
    }
}

#[cfg(windows)]
mod win_spell {
    use super::*;
    use crate::app_id::win_impl::to_wide;
    use crate::error::PlatformError;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::S_OK;
    use windows::Win32::Globalization::{
        CORRECTIVE_ACTION_GET_SUGGESTIONS, CORRECTIVE_ACTION_REPLACE, ISpellChecker,
        ISpellChecker2, ISpellCheckerFactory, ISpellingError, SpellCheckerFactory,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IEnumString,
    };
    use windows::core::{Interface, PCWSTR, PWSTR};

    fn is_persian(lang: &str) -> bool {
        lang.to_ascii_lowercase().starts_with("fa")
    }

    fn discover_system_languages() -> Vec<String> {
        let mut langs = Vec::new();
        let sys_lang = crate::system_info::system_language();
        if !sys_lang.is_empty() {
            langs.push(sys_lang);
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let dir = PathBuf::from(appdata).join("Microsoft").join("Spelling");
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if !langs.iter().any(|l| l.eq_ignore_ascii_case(&name)) {
                            langs.push(name);
                        }
                    }
                }
            }
        }
        if langs.is_empty() {
            langs.push("en-US".to_string());
        }
        langs
    }

    fn enum_strings(enumerator: &IEnumString, limit: Option<usize>) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            if let Some(max) = limit
                && out.len() >= max
            {
                break;
            }
            let mut ptrs = [PWSTR::null()];
            let mut fetched: u32 = 0;
            // SAFETY: Enumerating one string at a time from COM IEnumString.
            let hr = unsafe { enumerator.Next(&mut ptrs, Some(&mut fetched)) };
            if hr.is_err() || fetched == 0 || ptrs[0].0.is_null() {
                break;
            }
            let s = unsafe { ptrs[0].to_string() }.unwrap_or_default();
            // SAFETY: Freeing string allocated by IEnumString::Next.
            unsafe { CoTaskMemFree(Some(ptrs[0].0 as *const _)) };
            if !s.is_empty() {
                out.push(s);
            }
        }
        out
    }

    struct WorkerState {
        factory: Option<ISpellCheckerFactory>,
        checkers: Vec<(String, ISpellChecker)>,
    }

    impl WorkerState {
        fn new(requested_languages: Option<Vec<String>>) -> Self {
            // SAFETY: COM is initialized as MTA on the worker thread.
            let factory: Option<ISpellCheckerFactory> = unsafe {
                CoCreateInstance(
                    &SpellCheckerFactory,
                    None,
                    CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER,
                )
            }
            .ok();

            let mut state = Self {
                factory,
                checkers: Vec::new(),
            };
            let candidates = requested_languages.unwrap_or_else(discover_system_languages);
            state.load_languages(&candidates);
            state
        }

        fn load_languages(&mut self, candidates: &[String]) {
            let Some(ref factory) = self.factory else {
                return;
            };
            for lang in candidates {
                if self
                    .checkers
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(lang))
                {
                    continue;
                }
                let lang_w = to_wide(lang);
                // SAFETY: Valid factory and null-terminated BCP-47 tag.
                let supported = unsafe { factory.IsSupported(PCWSTR(lang_w.as_ptr())) }
                    .map(|b| b.as_bool())
                    .unwrap_or(false);
                if !supported {
                    continue;
                }
                // SAFETY: Creating per-language ISpellChecker instance.
                if let Ok(checker) = unsafe { factory.CreateSpellChecker(PCWSTR(lang_w.as_ptr())) }
                {
                    self.checkers.push((lang.clone(), checker));
                }
            }
        }

        fn supported_languages(&self) -> Vec<String> {
            let Some(ref factory) = self.factory else {
                return Vec::new();
            };
            // SAFETY: Enumerating supported languages from ISpellCheckerFactory.
            match unsafe { factory.SupportedLanguages() } {
                Ok(enumerator) => enum_strings(&enumerator, None),
                Err(_) => Vec::new(),
            }
        }

        fn check_word(&self, word: &str) -> bool {
            if self.checkers.is_empty() {
                return true;
            }
            let word_w = to_wide(word);
            for (_, checker) in &self.checkers {
                // SAFETY: Valid ISpellChecker and null-terminated word.
                let Ok(errors) = (unsafe { checker.Check(PCWSTR(word_w.as_ptr())) }) else {
                    continue;
                };
                // SAFETY: Reading first spelling error, if any.
                let mut err_opt: Option<ISpellingError> = None;
                let hr = unsafe { errors.Next(&mut err_opt) };
                let has_error = if hr == S_OK {
                    if let Some(err) = err_opt {
                        let action = unsafe { err.CorrectiveAction() }.ok();
                        matches!(
                            action,
                            Some(CORRECTIVE_ACTION_GET_SUGGESTIONS | CORRECTIVE_ACTION_REPLACE)
                        )
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !has_error {
                    return true;
                }
            }
            false
        }

        fn check_chunk_utf16(&self, chunk: &str, utf16_offset: usize) -> Vec<(usize, usize)> {
            if self.checkers.is_empty() {
                return Vec::new();
            }
            let chunk_w = to_wide(chunk);
            let mut misspelled: Option<Vec<(usize, usize)>> = None;

            for (lang_tag, checker) in &self.checkers {
                // SAFETY: Persian workaround from `spellcheck_win.cpp` (use Check instead of ComprehensiveCheck).
                let errors = if is_persian(lang_tag) {
                    unsafe { checker.Check(PCWSTR(chunk_w.as_ptr())) }
                } else {
                    unsafe { checker.ComprehensiveCheck(PCWSTR(chunk_w.as_ptr())) }
                };
                let Ok(errors) = errors else {
                    continue;
                };

                let mut current = Vec::new();
                loop {
                    let mut err_opt: Option<ISpellingError> = None;
                    let hr = unsafe { errors.Next(&mut err_opt) };
                    if hr != S_OK {
                        break;
                    }
                    let Some(err) = err_opt else {
                        break;
                    };
                    let Ok(start) = (unsafe { err.StartIndex() }) else {
                        continue;
                    };
                    let Ok(len) = (unsafe { err.Length() }) else {
                        continue;
                    };
                    let Ok(action) = (unsafe { err.CorrectiveAction() }) else {
                        continue;
                    };
                    if action != CORRECTIVE_ACTION_GET_SUGGESTIONS
                        && action != CORRECTIVE_ACTION_REPLACE
                    {
                        continue;
                    }
                    let span = (start as usize + utf16_offset, len as usize);
                    if misspelled
                        .as_ref()
                        .map(|prev| prev.contains(&span))
                        .unwrap_or(true)
                    {
                        current.push(span);
                    }
                }

                if current.is_empty() {
                    return Vec::new();
                }
                misspelled = Some(current);
            }

            misspelled.unwrap_or_default()
        }

        fn check_text(&self, text: &str) -> Vec<MisspelledRange> {
            if self.checkers.is_empty() || text.is_empty() {
                return Vec::new();
            }
            let sanitized = sanitize_for_spellcheck(text);
            let mut raw_spans = Vec::new();
            for (u16_off, _b_off, chunk) in
                split_text_chunks(&sanitized, CHECK_TEXT_CHUNK_UTF16_LEN, MAX_WORD_SIZE)
            {
                raw_spans.extend(self.check_chunk_utf16(chunk, u16_off));
            }
            raw_spans
                .into_iter()
                .filter_map(|(start, len)| utf16_range_to_misspelled(text, start, len))
                .collect()
        }

        fn suggestions(&self, word: &str, max_suggestions: usize) -> Vec<String> {
            if max_suggestions == 0 {
                return Vec::new();
            }
            let word_w = to_wide(word);
            let mut out = Vec::new();
            for (lang_tag, checker) in &self.checkers {
                if is_persian(lang_tag) {
                    continue;
                }
                // SAFETY: Requesting replacement suggestions from ISpellChecker.
                let Ok(enumerator) = (unsafe { checker.Suggest(PCWSTR(word_w.as_ptr())) }) else {
                    continue;
                };
                let remaining = max_suggestions.saturating_sub(out.len());
                if remaining == 0 {
                    break;
                }
                for guess in enum_strings(&enumerator, Some(remaining)) {
                    if !out.contains(&guess) {
                        out.push(guess);
                        if out.len() >= max_suggestions {
                            return out;
                        }
                    }
                }
            }
            out
        }

        fn add_word(&self, word: &str) -> Result<()> {
            let word_w = to_wide(word);
            for (_, checker) in &self.checkers {
                // SAFETY: Adding word to user dictionary.
                unsafe { checker.Add(PCWSTR(word_w.as_ptr())) }
                    .map_err(|e| PlatformError::from_win("ISpellChecker::Add", e))?;
            }
            Ok(())
        }

        fn remove_word(&self, word: &str) -> Result<()> {
            let word_w = to_wide(word);
            for (_, checker) in &self.checkers {
                if let Ok(checker2) = checker.cast::<ISpellChecker2>() {
                    // SAFETY: Removing word from user dictionary via ISpellChecker2.
                    let _ = unsafe { checker2.Remove(PCWSTR(word_w.as_ptr())) };
                }
            }
            Ok(())
        }

        fn ignore_word(&self, word: &str) -> Result<()> {
            let word_w = to_wide(word);
            for (_, checker) in &self.checkers {
                // SAFETY: Ignoring word for the lifetime of this ISpellChecker.
                unsafe { checker.Ignore(PCWSTR(word_w.as_ptr())) }
                    .map_err(|e| PlatformError::from_win("ISpellChecker::Ignore", e))?;
            }
            Ok(())
        }
    }

    enum Cmd {
        ActiveLanguages(mpsc::SyncSender<Vec<String>>),
        SupportedLanguages(mpsc::SyncSender<Vec<String>>),
        CheckWord(String, mpsc::SyncSender<bool>),
        CheckText(String, mpsc::SyncSender<Vec<MisspelledRange>>),
        Suggestions(String, usize, mpsc::SyncSender<Vec<String>>),
        AddWord(String, mpsc::SyncSender<Result<()>>),
        RemoveWord(String, mpsc::SyncSender<Result<()>>),
        IgnoreWord(String, mpsc::SyncSender<Result<()>>),
        Shutdown,
    }

    pub(crate) struct SpellCheckerClient {
        tx: Sender<Cmd>,
        added: Arc<Mutex<HashSet<String>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl SpellCheckerClient {
        pub(crate) fn spawn(requested_languages: Option<Vec<String>>) -> Result<Self> {
            let (tx, rx) = mpsc::channel::<Cmd>();
            let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(1);
            let thread = std::thread::Builder::new()
                .name("tg-spellcheck-com".into())
                .spawn(move || {
                    // SAFETY: Dedicated MTA COM apartment thread for ISpellChecker objects.
                    let hr = unsafe {
                        CoInitializeEx(None, COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE)
                    };
                    let state = WorkerState::new(requested_languages);
                    let _ = ready_tx.send(());

                    while let Ok(cmd) = rx.recv() {
                        match cmd {
                            Cmd::ActiveLanguages(r) => {
                                let _ =
                                    r.send(state.checkers.iter().map(|(l, _)| l.clone()).collect());
                            }
                            Cmd::SupportedLanguages(r) => {
                                let _ = r.send(state.supported_languages());
                            }
                            Cmd::CheckWord(w, r) => {
                                let _ = r.send(state.check_word(&w));
                            }
                            Cmd::CheckText(t, r) => {
                                let _ = r.send(state.check_text(&t));
                            }
                            Cmd::Suggestions(w, max, r) => {
                                let _ = r.send(state.suggestions(&w, max));
                            }
                            Cmd::AddWord(w, r) => {
                                let _ = r.send(state.add_word(&w));
                            }
                            Cmd::RemoveWord(w, r) => {
                                let _ = r.send(state.remove_word(&w));
                            }
                            Cmd::IgnoreWord(w, r) => {
                                let _ = r.send(state.ignore_word(&w));
                            }
                            Cmd::Shutdown => break,
                        }
                    }

                    drop(state);
                    if hr.is_ok() {
                        // SAFETY: Paired with CoInitializeEx on this worker thread.
                        unsafe { CoUninitialize() };
                    }
                })?;

            let _ = ready_rx.recv();
            Ok(Self {
                tx,
                added: Arc::new(Mutex::new(HashSet::new())),
                thread: Some(thread),
            })
        }

        pub(crate) fn active_languages(&self) -> Vec<String> {
            let (tx, rx) = mpsc::sync_channel(1);
            if self.tx.send(Cmd::ActiveLanguages(tx)).is_ok() {
                rx.recv().unwrap_or_default()
            } else {
                Vec::new()
            }
        }

        pub(crate) fn supported_languages(&self) -> Vec<String> {
            let (tx, rx) = mpsc::sync_channel(1);
            if self.tx.send(Cmd::SupportedLanguages(tx)).is_ok() {
                rx.recv().unwrap_or_default()
            } else {
                Vec::new()
            }
        }

        pub(crate) fn check_word(&self, word: &str) -> bool {
            let (tx, rx) = mpsc::sync_channel(1);
            if self.tx.send(Cmd::CheckWord(word.to_string(), tx)).is_ok() {
                rx.recv().unwrap_or(true)
            } else {
                true
            }
        }

        pub(crate) fn check_text(&self, text: &str) -> Vec<MisspelledRange> {
            let (tx, rx) = mpsc::sync_channel(1);
            if self.tx.send(Cmd::CheckText(text.to_string(), tx)).is_ok() {
                rx.recv().unwrap_or_default()
            } else {
                Vec::new()
            }
        }

        pub(crate) fn suggestions(&self, word: &str, max_suggestions: usize) -> Vec<String> {
            let (tx, rx) = mpsc::sync_channel(1);
            if self
                .tx
                .send(Cmd::Suggestions(word.to_string(), max_suggestions, tx))
                .is_ok()
            {
                rx.recv().unwrap_or_default()
            } else {
                Vec::new()
            }
        }

        pub(crate) fn add_word(&self, word: &str) -> Result<()> {
            let (tx, rx) = mpsc::sync_channel(1);
            self.tx
                .send(Cmd::AddWord(word.to_string(), tx))
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?;
            let res = rx
                .recv()
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?;
            if res.is_ok()
                && let Ok(mut set) = self.added.lock()
            {
                set.insert(word.to_string());
            }
            res
        }

        pub(crate) fn remove_word(&self, word: &str) -> Result<()> {
            let (tx, rx) = mpsc::sync_channel(1);
            self.tx
                .send(Cmd::RemoveWord(word.to_string(), tx))
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?;
            let res = rx
                .recv()
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?;
            if res.is_ok()
                && let Ok(mut set) = self.added.lock()
            {
                set.remove(word);
            }
            res
        }

        pub(crate) fn ignore_word(&self, word: &str) -> Result<()> {
            let (tx, rx) = mpsc::sync_channel(1);
            self.tx
                .send(Cmd::IgnoreWord(word.to_string(), tx))
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?;
            rx.recv()
                .map_err(|_| PlatformError::Spellcheck("spellchecker thread stopped".into()))?
        }

        pub(crate) fn is_added_word(&self, word: &str) -> bool {
            self.added.lock().map(|s| s.contains(word)).unwrap_or(false)
        }
    }

    impl Drop for SpellCheckerClient {
        fn drop(&mut self) {
            let _ = self.tx.send(Cmd::Shutdown);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub(crate) fn supported_languages() -> Result<Vec<String>> {
        let client = SpellCheckerClient::spawn(Some(Vec::new()))?;
        Ok(client.supported_languages())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_paragraph_separator() {
        assert_eq!(sanitize_for_spellcheck("a\u{2029}b"), "a b");
    }

    #[test]
    fn text_chunking_respects_word_boundaries() {
        let text = "hello world foo bar baz";
        let chunks = split_text_chunks(text, 12, 8);
        assert!(chunks.len() >= 2);
        let rejoined: String = chunks.iter().map(|(_, _, s)| *s).collect();
        assert_eq!(rejoined, text);
        // First chunk should end on space after "hello world " or "hello ".
        assert!(!chunks[0].2.ends_with("fo"));
    }

    #[test]
    fn utf16_to_byte_conversion_handles_surrogate_pairs() {
        let text = "hi 🚀 wrld";
        // "hi " = 3 UTF-16, "🚀" = 2 UTF-16 (4 UTF-8), " " = 1 UTF-16 -> "wrld" starts at UTF-16 6
        let range = utf16_range_to_misspelled(text, 6, 4).unwrap();
        assert_eq!(
            &text[range.byte_start..range.byte_start + range.byte_len],
            "wrld"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_spellchecker_basic_check_and_suggestions() {
        assert!(is_system_spellchecker_supported());
        let supported = supported_languages().unwrap();
        assert!(
            !supported.is_empty(),
            "expected at least one Windows spelling language"
        );

        let checker = SpellChecker::with_languages(&["en-US"]).unwrap();
        if checker.active_languages().is_empty() {
            // Machine might not have en-US spelling pack installed; fall back to first supported.
            let first = supported[0].as_str();
            let _ = SpellChecker::with_languages(&[first]).unwrap();
            return;
        }

        assert!(checker.check_word("hello"));
        assert!(!checker.check_word("helooooworldxyz"));
        let sugg = checker.suggestions("helo", DEFAULT_MAX_SUGGESTIONS);
        assert!(!sugg.is_empty());

        let ranges = checker.check_text("Hello helooooworldxyz world");
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            &"Hello helooooworldxyz world"
                [ranges[0].byte_start..ranges[0].byte_start + ranges[0].byte_len],
            "helooooworldxyz"
        );

        // Ignore word (in-memory only, does not mutate user dictionary on disk).
        checker.ignore_word("rustgramuniquetoken").unwrap();
        assert!(checker.check_word("rustgramuniquetoken"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "mutates real Windows user spelling dictionary on disk"]
    fn windows_spellchecker_add_and_remove_word() {
        let checker = SpellChecker::with_languages(&["en-US"]).unwrap();
        let token = "rustgramdicttesttoken";
        checker.add_word(token).unwrap();
        assert!(checker.is_added_word(token));
        assert!(checker.check_word(token));
        checker.remove_word(token).unwrap();
        assert!(!checker.is_added_word(token));
    }
}
