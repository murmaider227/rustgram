//! Pure managed-bot DTOs and validation helpers (`boxes/managed_bot.rs` model).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::info::values::MAX_USERNAME_LENGTH;

/// Maximum length of a bot display name (`bots.setBotInfo`).
pub const MAX_BOT_NAME_LENGTH: usize = 64;
/// Maximum length of a bot short "about" bio (`bots.setBotInfo`).
pub const MAX_BOT_ABOUT_LENGTH: usize = 120;
/// Maximum length of a bot intro description (`bots.setBotInfo`).
pub const MAX_BOT_DESCRIPTION_LENGTH: usize = 512;

/// Built-in supported localization language codes for [`EditBotInfoBoxState`].
pub const BOT_INFO_LANGUAGES: [(&str, &str); 8] = [
    ("", "Default"),
    ("en", "English"),
    ("es", "Spanish"),
    ("de", "German"),
    ("fr", "French"),
    ("pt", "Portuguese"),
    ("uk", "Ukrainian"),
    ("ru", "Russian"),
];

/// Computes the shortest missing bot username ending for `value` given `endings`.
///
/// Exact port of `MissingBotUsernameEnding` in `create_managed_bot_box.cpp:52-74`:
/// shortest completion wins (e.g. with `["bot", "ai", "agent"]`, `"mya"` -> `"i"`,
/// `"myb"` -> `"ot"`, `"mybot"` -> `""`, `"my"` -> `"bot"`).
#[must_use]
pub fn missing_bot_username_ending(value: &str, endings: &[String]) -> String {
    let default_ending = endings.first().map_or("bot", String::as_str);
    let mut best: Option<(String, usize)> = None;
    for ending in endings {
        let ending_bytes = ending.as_bytes();
        let val_bytes = value.as_bytes();
        let bound = val_bytes.len().min(ending_bytes.len());
        for k in (1..=bound).rev() {
            let tail = &val_bytes[val_bytes.len() - k..];
            let head = &ending_bytes[..k];
            if !tail.eq_ignore_ascii_case(head) {
                continue;
            }
            let rest = &ending[k..];
            let is_better = match &best {
                None => true,
                Some((best_rest, best_overlap)) => {
                    rest.len() < best_rest.len()
                        || (rest.len() == best_rest.len() && k > *best_overlap)
                }
            };
            if is_better {
                best = Some((rest.to_owned(), k));
            }
            break;
        }
    }
    best.map_or_else(|| default_ending.to_owned(), |(rest, _)| rest)
}

/// Trims `raw` so `raw.len() + missing_bot_username_ending(raw, endings).len() <= MAX_USERNAME_LENGTH`.
///
/// Exact port of `enforceLength` in `create_managed_bot_box.cpp:248-259`.
#[must_use]
pub fn enforce_bot_username_length(raw: &str, endings: &[String]) -> String {
    let mut fitted = raw.to_owned();
    while !fitted.is_empty()
        && fitted.len() + missing_bot_username_ending(&fitted, endings).len() > MAX_USERNAME_LENGTH
    {
        fitted.pop();
    }
    fitted
}

/// Validates that every character of a bot username input is ASCII alphanumeric or `_`.
#[must_use]
pub fn is_valid_bot_username_chars(value: &str) -> bool {
    value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Localized bot info texts (`Data::BotInfoTexts` / `bots.botInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotInfoTexts {
    /// Bot display name.
    pub name: String,
    /// Short bio shown on profile.
    pub about: String,
    /// Intro description shown before `/start`.
    pub description: String,
}

/// Partial update payload passed to `bots.setBotInfo` (`Data::BotInfoChange`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotInfoChange {
    /// Updated name (if changed).
    pub name: Option<String>,
    /// Updated about text (if changed).
    pub about: Option<String>,
    /// Updated intro description (if changed).
    pub description: Option<String>,
}

impl BotInfoChange {
    /// Returns `true` if no fields are set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none() && self.about.is_none() && self.description.is_none()
    }
}

/// Per-language unsaved draft tracker for a managed bot (`Data::BotInfoDraft`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotInfoDraft {
    /// Original server values per `lang_code` (`""` = default).
    pub original: BTreeMap<String, BotInfoTexts>,
    /// Edited values per `lang_code`.
    pub current: BTreeMap<String, BotInfoTexts>,
}

impl BotInfoDraft {
    /// Seeds or refreshes the original value for `lang_code` without clobbering unsaved edits.
    pub fn set_loaded(&mut self, lang_code: &str, texts: BotInfoTexts) {
        self.original.insert(lang_code.to_owned(), texts.clone());
        self.current.entry(lang_code.to_owned()).or_insert(texts);
    }

    /// Returns a mutable reference to the draft entry for `lang_code`.
    pub fn current_mut(&mut self, lang_code: &str) -> &mut BotInfoTexts {
        self.current.entry(lang_code.to_owned()).or_default()
    }

    /// Computes the diff for `lang_code` against `original`.
    #[must_use]
    pub fn diff_for(&self, lang_code: &str) -> BotInfoChange {
        let Some(cur) = self.current.get(lang_code) else {
            return BotInfoChange::default();
        };
        let orig = self.original.get(lang_code).cloned().unwrap_or_default();
        BotInfoChange {
            name: (cur.name != orig.name).then(|| cur.name.clone()),
            about: (cur.about != orig.about).then(|| cur.about.clone()),
            description: (cur.description != orig.description).then(|| cur.description.clone()),
        }
    }

    /// Returns the number of languages with pending unsaved changes.
    #[must_use]
    pub fn changes_count(&self) -> usize {
        self.current
            .keys()
            .filter(|lang| !self.diff_for(lang).is_empty())
            .count()
    }

    /// Marks `lang_code` as saved after `bots.setBotInfo` succeeds.
    pub fn commit_saved(&mut self, lang_code: &str) {
        if let Some(cur) = self.current.get(lang_code).cloned() {
            self.original.insert(lang_code.to_owned(), cur);
        }
    }

    /// Serializes all current localizations to pretty JSON (`bot_info_file.cpp`).
    pub fn export_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.current)
            .map_err(|e| format!("Failed to serialize bot info JSON: {e}"))
    }

    /// Imports localization entries from JSON into the draft (`bot_info_file.cpp`).
    pub fn import_json(&mut self, json: &str) -> Result<usize, String> {
        let parsed: BTreeMap<String, BotInfoTexts> =
            serde_json::from_str(json).map_err(|e| format!("Invalid bot info JSON: {e}"))?;
        let count = parsed.len();
        for (lang, texts) in parsed {
            self.current.insert(lang, texts);
        }
        Ok(count)
    }
}

/// Availability status for a managed bot username.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotUsernameStatus {
    /// Empty input.
    Empty,
    /// Invalid characters (only `A-Za-z0-9_` allowed).
    BadSymbols,
    /// Resulting full username is shorter than 5 characters.
    TooShort,
    /// Syntax valid; ready / checked as available.
    Available(String),
    /// Server reported username is taken or invalid.
    Taken,
}
