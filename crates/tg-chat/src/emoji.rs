//! Recent emoji with usage weights and decay, skin tone variant preferences,
//! multi-language emoji keywords search with diff application, legacy `:shortcode`
//! autocomplete, and emoji suggestions typing trigger/replacement.
//!
//! Port of:
//! - `Telegram/SourceFiles/core/core_settings.cpp` (recent emoji & variants)
//! - `Telegram/SourceFiles/chat_helpers/emoji_keywords.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_suggestions_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_list_widget.cpp` (`fillRecent`)
//! - `Telegram/lib_ui/emoji_suggestions/emoji_suggestions.cpp`
//! - `Telegram/lib_ui/ui/emoji_config.cpp` (`GetDefaultRecent`, `ColorIndexFromCode`)

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use tg_data::DocumentId;

/// Maximum number of displayed/stored recent emoji, matching `Core::kRecentEmojiLimit = 54`.
pub const RECENT_EMOJI_LIMIT: usize = 54;

/// Rating threshold above which all recent emoji ratings decay by half,
/// matching `i->rating > 0x8000` in `core/core_settings.cpp`.
pub const RECENT_EMOJI_DECAY_THRESHOLD: u16 = 0x8000;

/// Number of unused language packs kept warm in memory,
/// matching `kKeepNotUsedLangPacksCount = 4` in `chat_helpers/emoji_keywords.cpp`.
pub const KEEP_NOT_USED_LANG_PACKS_COUNT: usize = 4;

/// Number of recent input language lists retained,
/// matching `kKeepNotUsedInputLanguagesCount = 4` in `chat_helpers/emoji_keywords.cpp`.
pub const KEEP_NOT_USED_INPUT_LANGUAGES_COUNT: usize = 4;

/// Emoji presentation variation selector (`U+FE0F`), matching `Ui::Emoji::kPostfix`.
pub const EMOJI_POSTFIX: char = '\u{FE0F}';

/// Number of non-default Fitzpatrick skin tone variants (`1..=5`), matching `One::variantsCount()`.
pub const SKIN_TONE_VARIANTS_COUNT: u8 = 5;

/// Exact 34 default recent emoji from `Ui::Emoji::GetDefaultRecent()` in
/// `Telegram/lib_ui/ui/emoji_config.cpp`.
pub const DEFAULT_RECENT_EMOJI: [&str; 34] = [
    "\u{1F602}",        // 😂
    "\u{1F618}",        // 😘
    "\u{2764}\u{FE0F}", // ❤️
    "\u{1F60D}",        // 😍
    "\u{1F60A}",        // 😊
    "\u{1F601}",        // 😁
    "\u{1F44D}",        // 👍
    "\u{263A}\u{FE0F}", // ☺️
    "\u{1F614}",        // 😔
    "\u{1F604}",        // 😄
    "\u{1F62D}",        // 😭
    "\u{1F48B}",        // 💋
    "\u{1F612}",        // 😒
    "\u{1F633}",        // 😳
    "\u{1F61C}",        // 😜
    "\u{1F648}",        // 🙈
    "\u{1F609}",        // 😉
    "\u{1F603}",        // 😃
    "\u{1F622}",        // 😢
    "\u{1F61D}",        // 😝
    "\u{1F631}",        // 😱
    "\u{1F621}",        // 😡
    "\u{1F60F}",        // 😏
    "\u{1F61E}",        // 😞
    "\u{1F605}",        // 😅
    "\u{1F61A}",        // 😚
    "\u{1F64A}",        // 🙊
    "\u{1F60C}",        // 😌
    "\u{1F600}",        // 😀
    "\u{1F60B}",        // 😋
    "\u{1F606}",        // 😆
    "\u{1F44C}",        // 👌
    "\u{1F610}",        // 😐
    "\u{1F615}",        // 😕
];

/// Fitzpatrick skin tone index (`0` = default yellow, `1..=5` = light..dark).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SkinTone {
    /// Default (no skin tone modifier).
    #[default]
    Default = 0,
    /// Light skin tone (`U+1F3FB`).
    Light = 1,
    /// Medium-light skin tone (`U+1F3FC`).
    MediumLight = 2,
    /// Medium skin tone (`U+1F3FD`).
    Medium = 3,
    /// Medium-dark skin tone (`U+1F3FE`).
    MediumDark = 4,
    /// Dark skin tone (`U+1F3FF`).
    Dark = 5,
}

impl SkinTone {
    /// Converts a raw index `0..=5` into a `SkinTone`.
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Default),
            1 => Some(Self::Light),
            2 => Some(Self::MediumLight),
            3 => Some(Self::Medium),
            4 => Some(Self::MediumDark),
            5 => Some(Self::Dark),
            _ => None,
        }
    }

    /// Converts a Unicode modifier character (`U+1F3FB`..=`U+1F3FF`) to its skin tone,
    /// matching `Ui::Emoji::ColorIndexFromCode`.
    pub const fn from_modifier_char(ch: char) -> Self {
        match ch {
            '\u{1F3FB}' => Self::Light,
            '\u{1F3FC}' => Self::MediumLight,
            '\u{1F3FD}' => Self::Medium,
            '\u{1F3FE}' => Self::MediumDark,
            '\u{1F3FF}' => Self::Dark,
            _ => Self::Default,
        }
    }

    /// Returns the Fitzpatrick Unicode modifier character, or `None` for `Default`.
    pub const fn modifier_char(self) -> Option<char> {
        match self {
            Self::Default => None,
            Self::Light => Some('\u{1F3FB}'),
            Self::MediumLight => Some('\u{1F3FC}'),
            Self::Medium => Some('\u{1F3FD}'),
            Self::MediumDark => Some('\u{1F3FE}'),
            Self::Dark => Some('\u{1F3FF}'),
        }
    }

    /// Returns the numeric variant index (`0..=5`).
    pub const fn index(self) -> u8 {
        self as u8
    }
}

/// Returns `true` if `ch` is a Fitzpatrick skin tone modifier (`U+1F3FB`..=`U+1F3FF`).
pub const fn is_skin_tone_modifier(ch: char) -> bool {
    matches!(ch, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Strips any Fitzpatrick skin tone modifier from `emoji`, returning its non-colored base string.
pub fn strip_skin_tone(emoji: &str) -> String {
    emoji
        .chars()
        .filter(|&ch| !is_skin_tone_modifier(ch))
        .collect()
}

/// Extracts the first Fitzpatrick skin tone present in `emoji`, or `SkinTone::Default`.
pub fn detect_skin_tone(emoji: &str) -> SkinTone {
    for ch in emoji.chars() {
        let tone = SkinTone::from_modifier_char(ch);
        if tone != SkinTone::Default {
            return tone;
        }
    }
    SkinTone::Default
}

/// Applies `tone` to a single-person/hand `emoji` (replacing any existing skin tone modifier
/// and stripping an immediately following `U+FE0F` on the base character when a tone is applied).
pub fn apply_skin_tone(emoji: &str, tone: SkinTone) -> String {
    let base = strip_skin_tone(emoji);
    let Some(modifier) = tone.modifier_char() else {
        return base;
    };
    let mut chars = base.chars();
    let Some(first) = chars.next() else {
        return base;
    };
    let rest: String = chars.collect();
    let rest_stripped = rest.strip_prefix(EMOJI_POSTFIX).unwrap_or(&rest);
    let mut out = String::with_capacity(base.len() + 4);
    out.push(first);
    out.push(modifier);
    out.push_str(rest_stripped);
    out
}

/// Skin tone variant preferences store.
///
/// Direct port of `Core::Settings` emoji variant methods (`lookupEmojiVariant`,
/// `hasChosenEmojiVariant`, `saveEmojiVariant`, `saveAllEmojiVariants`) in
/// `Telegram/SourceFiles/core/core_settings.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiVariants {
    /// Maps non-colored emoji ID -> skin tone index (`0..=5`).
    /// Empty string key `""` stores the global preference set by `save_all`.
    variants: BTreeMap<String, u8>,
}

impl EmojiVariants {
    /// Creates an empty preferences map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the raw underlying map (for serialization).
    pub fn raw(&self) -> &BTreeMap<String, u8> {
        &self.variants
    }

    /// Loads legacy variant preferences if the store is currently empty.
    pub fn set_legacy(&mut self, data: BTreeMap<String, u8>) {
        if !self.variants.is_empty() || data.is_empty() {
            return;
        }
        self.variants = data;
    }

    /// Saves a preferred skin tone for a specific emoji (keyed by its non-colored base).
    pub fn save_variant(&mut self, emoji: &str, tone: SkinTone) {
        let base = strip_skin_tone(emoji);
        self.variants.insert(base, tone.index());
    }

    /// Clears all per-emoji overrides and saves `tone` as the global preference (`""`),
    /// matching `Settings::saveAllEmojiVariants`.
    pub fn save_all_variants(&mut self, tone: SkinTone) {
        self.variants.clear();
        self.variants.insert(String::new(), tone.index());
    }

    /// Returns `true` if either a global preference or an override for `emoji` exists.
    pub fn has_chosen(&self, emoji: &str) -> bool {
        self.variants.contains_key("") || self.variants.contains_key(&strip_skin_tone(emoji))
    }

    /// Looks up the preferred [`SkinTone`] for `emoji` (per-emoji override first, then global `""`).
    pub fn lookup_tone(&self, emoji: &str) -> Option<SkinTone> {
        let base = strip_skin_tone(emoji);
        if let Some(&idx) = self.variants.get(&base) {
            return SkinTone::from_index(idx);
        }
        if let Some(&idx) = self.variants.get("") {
            return SkinTone::from_index(idx);
        }
        None
    }

    /// Applies the saved variant preference to `emoji` when `has_variants` is `true`,
    /// matching `Settings::lookupEmojiVariant`.
    pub fn lookup_emoji(&self, emoji: &str, has_variants: bool) -> String {
        if !has_variants {
            return emoji.to_string();
        }
        match self.lookup_tone(emoji) {
            Some(tone) => apply_skin_tone(emoji, tone),
            None => emoji.to_string(),
        }
    }
}

/// Custom emoji document reference inside recent emoji, matching `Core::RecentEmojiDocument`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecentEmojiDocument {
    /// Custom emoji document ID.
    pub id: DocumentId,
    /// Whether this document belongs to a test DC account.
    pub test: bool,
}

impl RecentEmojiDocument {
    /// Serializes as `"{id}-{0|1}"`, matching `Serialize(RecentEmojiDocument)` in `core_settings.cpp`.
    pub fn serialize(self) -> String {
        format!("{}-{}", self.id.0, if self.test { 1 } else { 0 })
    }

    /// Parses `"{id}-{0|1}"`, matching `ParseRecentEmojiDocument` in `core_settings.cpp`.
    pub fn parse(serialized: &str) -> Option<Self> {
        let (id_part, test_part) = serialized.split_once('-')?;
        if test_part.len() != 1 || test_part.contains('-') {
            return None;
        }
        let id = id_part.parse::<u64>().ok()?;
        if id == 0 {
            return None;
        }
        let test = match test_part {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        Some(Self {
            id: DocumentId(id),
            test,
        })
    }
}

/// Identifier of an entry in the recent emoji list, matching `Core::RecentEmojiId`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecentEmojiId {
    /// Standard Unicode emoji string.
    Unicode(String),
    /// Custom animated/static emoji document.
    Document(RecentEmojiDocument),
}

/// Single recent emoji entry with its usage rating, matching `Core::RecentEmoji`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEmoji {
    /// Emoji identifier.
    pub id: RecentEmojiId,
    /// Usage weight (decays by half when any entry exceeds `0x8000`).
    pub rating: u16,
}

/// Recent emoji manager with usage weights, decay, default backfill, hiding, and custom emoji support.
///
/// Direct port of `Core::Settings` recent emoji logic (`resolveRecentEmoji`, `incrementRecentEmoji`,
/// `hideRecentEmoji`, `resetRecentEmoji`) and `EmojiListWidget::fillRecent`.
#[derive(Debug, Clone, Default)]
pub struct RecentEmojiStore {
    preload: Vec<(String, u16)>,
    list: Vec<RecentEmoji>,
    skip: BTreeSet<String>,
    resolved: bool,
}

impl RecentEmojiStore {
    /// Creates a new unresolved recent emoji store (defaults populate lazily on first access).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets preloaded serialized `(id, rating)` entries before resolution.
    pub fn set_preload(&mut self, data: Vec<(String, u16)>) {
        if !self.preload.is_empty() || data.is_empty() {
            return;
        }
        self.preload = data;
    }

    /// Resolves preloaded entries and fills remaining slots up to `special_count + 54`
    /// from [`DEFAULT_RECENT_EMOJI`], skipping hidden defaults.
    pub fn resolve(&mut self) {
        if self.resolved {
            return;
        }
        self.resolved = true;

        let mut test_count = 0usize;
        let mut non_test_count = 0usize;
        let preload = std::mem::take(&mut self.preload);
        self.list.reserve(preload.len());

        for (raw_id, rating) in preload {
            if let Some(doc) = RecentEmojiDocument::parse(&raw_id) {
                let id = RecentEmojiId::Document(doc);
                if !self.list.iter().any(|e| e.id == id) {
                    self.list.push(RecentEmoji { id, rating });
                    if doc.test {
                        test_count += 1;
                    } else {
                        non_test_count += 1;
                    }
                }
            } else if !raw_id.is_empty() {
                let id = RecentEmojiId::Unicode(raw_id);
                if !self.list.iter().any(|e| e.id == id) {
                    self.list.push(RecentEmoji { id, rating });
                }
            }
        }

        let special_count = test_count.max(non_test_count);
        for &default_emoji in &DEFAULT_RECENT_EMOJI {
            if self.list.len() >= special_count + RECENT_EMOJI_LIMIT {
                break;
            }
            if self.skip.contains(default_emoji) {
                continue;
            }
            let id = RecentEmojiId::Unicode(default_emoji.to_string());
            if !self.list.iter().any(|e| e.id == id) {
                self.list.push(RecentEmoji { id, rating: 1 });
            }
        }
    }

    /// Returns the resolved list of recent emoji in descending rating order.
    pub fn list(&mut self) -> &[RecentEmoji] {
        self.resolve();
        &self.list
    }

    /// Returns already-resolved entries without mutating (empty if `resolve()` hasn't been called).
    pub fn list_if_resolved(&self) -> &[RecentEmoji] {
        &self.list
    }

    /// Increments the usage weight of `id`, halving all ratings if any exceeds `0x8000`,
    /// bubbling `id` past equal or lower ratings, and enforcing the `special_count + 54` cap.
    ///
    /// Exact port of `Settings::incrementRecentEmoji` in `core/core_settings.cpp`.
    pub fn increment(&mut self, id: RecentEmojiId) {
        self.resolve();

        if let RecentEmojiId::Unicode(ref s) = id {
            self.skip.remove(s);
        }

        if let Some(mut idx) = self.list.iter().position(|e| e.id == id) {
            self.list[idx].rating = self.list[idx].rating.saturating_add(1);
            if self.list[idx].rating > RECENT_EMOJI_DECAY_THRESHOLD {
                for entry in &mut self.list {
                    if entry.rating > 1 {
                        entry.rating /= 2;
                    } else {
                        entry.rating = 1;
                    }
                }
            }
            while idx > 0 {
                if self.list[idx - 1].rating > self.list[idx].rating {
                    break;
                }
                self.list.swap(idx, idx - 1);
                idx -= 1;
            }
        } else {
            self.list.push(RecentEmoji { id, rating: 1 });
            let mut idx = self.list.len() - 1;
            while idx > 0 {
                if self.list[idx - 1].rating > self.list[idx].rating {
                    break;
                }
                self.list.swap(idx, idx - 1);
                idx -= 1;
            }

            let mut test_count = 0usize;
            let mut non_test_count = 0usize;
            for entry in &self.list {
                if let RecentEmojiId::Document(doc) = entry.id {
                    if doc.test {
                        test_count += 1;
                    } else {
                        non_test_count += 1;
                    }
                }
            }
            let special_count = test_count.max(non_test_count);
            while self.list.len() >= special_count + RECENT_EMOJI_LIMIT {
                self.list.pop();
            }
        }
    }

    /// Removes `id` from recent emoji and remembers it in the skip set if it is a default emoji.
    ///
    /// Exact port of `Settings::hideRecentEmoji` in `core/core_settings.cpp`.
    pub fn hide(&mut self, id: &RecentEmojiId) {
        self.resolve();
        self.list.retain(|e| &e.id != id);
        if let RecentEmojiId::Unicode(s) = id
            && DEFAULT_RECENT_EMOJI.contains(&s.as_str())
        {
            self.skip.insert(s.clone());
        }
    }

    /// Resets recent emoji back to defaults, clearing custom ratings and hidden skips.
    pub fn reset(&mut self) {
        self.list.clear();
        self.skip.clear();
        self.preload.clear();
        self.resolved = false;
    }

    /// Returns the visible recent emoji list for the emoji picker, filtering out custom emoji
    /// from the other DC mode (`test_mode`) or all custom emoji when `only_unicode` is true,
    /// capped at [`RECENT_EMOJI_LIMIT`] (54).
    ///
    /// Exact port of `EmojiListWidget::fillRecent` in `chat_helpers/emoji_list_widget.cpp`.
    pub fn visible_recent(&mut self, test_mode: bool, only_unicode: bool) -> Vec<RecentEmojiId> {
        self.resolve();
        let mut out = Vec::with_capacity(self.list.len().min(RECENT_EMOJI_LIMIT));
        for entry in &self.list {
            if let RecentEmojiId::Document(doc) = entry.id
                && (doc.test != test_mode || only_unicode)
            {
                continue;
            }
            out.push(entry.id.clone());
            if out.len() >= RECENT_EMOJI_LIMIT {
                break;
            }
        }
        out
    }
}

/// Returns `true` if `text` is a single trademark/copyright/registered symbol (`™`, `©`, `®`)
/// that requires appending `U+FE0F` for emoji presentation.
///
/// Direct port of `MustAddPostfix` in `chat_helpers/emoji_keywords.cpp`.
pub fn must_add_postfix(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    matches!(first, '\u{2122}' | '\u{00A9}' | '\u{00AE}')
}

/// Returns `true` if `word` should be skipped for exact-word (non-colon) emoji suggestions.
///
/// Exact port of `SkipExactKeyword` in `chat_helpers/emoji_keywords.cpp`.
pub fn skip_exact_keyword(language: &str, word: &str) -> bool {
    let char_count = word.chars().count();
    let first_char = word.chars().next();
    if char_count == 1 && first_char.is_some_and(|c| !c.is_alphabetic()) {
        return true;
    }
    if word == "10" {
        return true;
    }
    if language != "en" {
        return false;
    }
    if char_count == 1 && first_char != Some('$') && first_char != Some('€') {
        return true;
    }
    if char_count == 2 && !matches!(word, "us" | "uk" | "hi" | "ok") {
        return true;
    }
    false
}

/// Normalizes a user search query for keyword lookup (`toLower()`).
pub fn normalize_emoji_query(query: &str) -> String {
    query.to_lowercase()
}

/// Normalizes a keyword pack key (`toLower().trimmed()`).
pub fn normalize_emoji_key(key: &str) -> String {
    key.to_lowercase().trim().to_string()
}

/// Single diff operation from `messages.getEmojiKeywordsDifference` / `messages.getEmojiKeywords`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmojiKeywordDiff {
    /// `emojiKeyword`: adds emoticons under `keyword`.
    Added {
        /// Keyword string.
        keyword: String,
        /// Emoji strings associated with this keyword.
        emoticons: Vec<String>,
    },
    /// `emojiKeywordDeleted`: removes specific emoticons from `keyword`.
    Deleted {
        /// Keyword string.
        keyword: String,
        /// Emoji strings to remove from this keyword.
        emoticons: Vec<String>,
    },
}

/// Single entry inside a language pack's keyword index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangPackEmoji {
    /// Canonical emoji string (with `U+FE0F` appended for `™`, `©`, `®`).
    pub emoji: String,
    /// Raw replacement text from the server.
    pub text: String,
}

/// Result returned by [`EmojiKeywords::query`] and [`EmojiKeywords::query_mine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiKeywordResult {
    /// Resolved emoji string (may have skin tone applied after `query_mine`).
    pub emoji: String,
    /// Matched keyword or `:shortcode:` label.
    pub label: String,
    /// Replacement text.
    pub replacement: String,
}

/// Per-language emoji keyword dictionary with incremental diff support.
///
/// Direct port of `EmojiKeywords::LangPack` and `ApplyDifference` in
/// `Telegram/SourceFiles/chat_helpers/emoji_keywords.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiKeywordLangPack {
    /// Language code (e.g. `"en"`, `"ru"`, `"de"`).
    pub lang_code: String,
    /// Server-provided version for incremental `getEmojiKeywordsDifference`.
    pub version: i32,
    /// Longest keyword length in characters.
    pub max_key_length: usize,
    /// Sorted map from normalized keyword to emoji list (mirrors C++ `std::map`).
    pub emoji: BTreeMap<String, Vec<LangPackEmoji>>,
}

impl EmojiKeywordLangPack {
    /// Creates an empty language pack for `lang_code`.
    pub fn new(lang_code: impl Into<String>) -> Self {
        Self {
            lang_code: lang_code.into(),
            version: 0,
            max_key_length: 0,
            emoji: BTreeMap::new(),
        }
    }

    /// Applies an `emojiKeywordsDifference` payload to this pack.
    ///
    /// Returns `true` if the pack was updated, or `false` on a language code mismatch
    /// (which resets `version = 0`) or a no-op empty diff with `self.version >= version`.
    pub fn apply_difference(
        &mut self,
        response_lang_code: &str,
        version: i32,
        diffs: &[EmojiKeywordDiff],
    ) -> bool {
        if response_lang_code != self.lang_code {
            self.version = 0;
            return false;
        }
        if diffs.is_empty() && self.version >= version {
            return false;
        }

        self.version = version;
        for diff in diffs {
            match diff {
                EmojiKeywordDiff::Added { keyword, emoticons } => {
                    let word = normalize_emoji_key(keyword);
                    if word.is_empty() {
                        continue;
                    }
                    let list = self.emoji.entry(word).or_default();
                    for raw in emoticons {
                        if raw.is_empty() {
                            continue;
                        }
                        let canonical = if must_add_postfix(raw) {
                            format!("{raw}{EMOJI_POSTFIX}")
                        } else {
                            raw.clone()
                        };
                        list.push(LangPackEmoji {
                            emoji: canonical,
                            text: raw.clone(),
                        });
                    }
                }
                EmojiKeywordDiff::Deleted { keyword, emoticons } => {
                    let word = normalize_emoji_key(keyword);
                    if word.is_empty() {
                        continue;
                    }
                    let remove_empty = if let Some(list) = self.emoji.get_mut(&word) {
                        for raw in emoticons {
                            list.retain(|e| e.text != *raw);
                        }
                        list.is_empty()
                    } else {
                        false
                    };
                    if remove_empty {
                        self.emoji.remove(&word);
                    }
                }
            }
        }

        self.max_key_length = self
            .emoji
            .keys()
            .map(|k| k.chars().count())
            .max()
            .unwrap_or(0);
        true
    }

    /// Searches this language pack for `normalized` (already lowercased), using either
    /// exact or prefix matching, deduplicating emoji across matched keys.
    pub fn query(&self, normalized: &str, exact: bool) -> Vec<EmojiKeywordResult> {
        let query_chars = normalized.chars().count();
        if query_chars > self.max_key_length
            || self.emoji.is_empty()
            || (exact && skip_exact_keyword(&self.lang_code, normalized))
        {
            return Vec::new();
        }

        let mut result: Vec<EmojiKeywordResult> = Vec::new();
        for (key, list) in self.emoji.range(normalized.to_string()..) {
            let matches = if exact {
                key == normalized
            } else {
                key.starts_with(normalized)
            };
            if !matches {
                break;
            }
            for entry in list {
                if !result.iter().any(|r| r.emoji == entry.emoji) {
                    result.push(EmojiKeywordResult {
                        emoji: entry.emoji.clone(),
                        label: key.clone(),
                        replacement: entry.text.clone(),
                    });
                }
            }
        }
        result
    }
}

/// Entry in the legacy `:shortcode:` autocomplete dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyReplacement {
    /// Emoji string (e.g. `"😀"`).
    pub emoji: String,
    /// Shortcode with leading and trailing colons (e.g. `":grinning:"`).
    pub replacement: String,
    /// Sorted list of lowercase search words derived from `replacement`.
    pub words: Vec<String>,
}

impl LegacyReplacement {
    /// Builds a [`LegacyReplacement`] and automatically splits its shortcode into sorted words
    /// (on `_`, `-`, `:`, whitespace), matching `lib_ui/emoji_suggestions` codegen.
    pub fn new(emoji: impl Into<String>, replacement: impl Into<String>) -> Self {
        let replacement = replacement.into();
        let inner = replacement.trim_matches(':');
        let mut words: Vec<String> = inner
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '-')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_ascii_lowercase())
            .collect();
        if words.is_empty() && !inner.is_empty() {
            words.push(inner.to_ascii_lowercase());
        }
        words.sort();
        Self {
            emoji: emoji.into(),
            replacement,
            words,
        }
    }
}

/// Legacy `:shortcode:` completer implementing multi-word prefix backtracking and
/// 4-pass `stable_partition` ranking.
///
/// Direct port of `Ui::Emoji::Completer` in `Telegram/lib_ui/emoji_suggestions/emoji_suggestions.cpp`.
#[derive(Debug, Clone, Default)]
pub struct LegacyEmojiCompleter {
    replacements: Vec<LegacyReplacement>,
    max_length: usize,
}

impl LegacyEmojiCompleter {
    /// Creates an empty completer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a completer pre-populated with all shortcodes from `tg-text`'s
    /// compiled `emoji_autocomplete.json` table.
    pub fn with_builtin_all() -> Self {
        let mut completer = Self::new();
        for entry in tg_text::emoji::all_suggestions() {
            completer.add(LegacyReplacement {
                emoji: entry.emoji.to_string(),
                replacement: entry.replacement.to_string(),
                words: entry.words.iter().map(|w| (*w).to_string()).collect(),
            });
        }
        completer
    }

    /// Creates a completer pre-populated with a built-in set of common Telegram shortcodes.
    pub fn with_builtin_common() -> Self {
        let pairs = [
            ("\u{1F600}", ":grinning:"),
            ("\u{1F604}", ":smile:"),
            ("\u{1F60A}", ":blush:"),
            ("\u{1F602}", ":joy:"),
            ("\u{1F60D}", ":heart_eyes:"),
            ("\u{2764}\u{FE0F}", ":heart:"),
            ("\u{1F525}", ":fire:"),
            ("\u{1F44D}", ":thumbsup:"),
            ("\u{1F44D}", ":+1:"),
            ("\u{1F44E}", ":thumbsdown:"),
            ("\u{1F44E}", ":-1:"),
            ("\u{1F4AF}", ":100:"),
            ("\u{1F389}", ":tada:"),
            ("\u{1F62D}", ":sob:"),
            ("\u{1F914}", ":thinking:"),
            ("\u{1F64F}", ":pray:"),
            ("\u{1F44F}", ":clap:"),
            ("\u{1F680}", ":rocket:"),
            ("\u{2B50}", ":star:"),
            ("\u{2705}", ":white_check_mark:"),
            ("\u{274C}", ":x:"),
            ("\u{1F440}", ":eyes:"),
            ("\u{1F60E}", ":sunglasses:"),
            ("\u{1F609}", ":wink:"),
        ];
        let mut completer = Self::new();
        for (emoji, code) in pairs {
            completer.add(LegacyReplacement::new(emoji, code));
        }
        completer
    }

    /// Adds a shortcode replacement entry.
    pub fn add(&mut self, entry: LegacyReplacement) {
        self.max_length = self.max_length.max(entry.replacement.chars().count());
        self.replacements.push(entry);
    }

    /// Returns the maximum shortcode length (matching `GetSuggestionMaxLength()`).
    pub const fn max_length(&self) -> usize {
        self.max_length
    }

    /// Normalizes a legacy query: keeps ASCII lowercase alphanumeric characters, and keeps
    /// `'-'` / `'+'` only if followed by an ASCII digit or at the end of the query.
    ///
    /// Exact port of `Completer::NormalizeQuery` in `emoji_suggestions.cpp`.
    pub fn normalize_query(query: &str) -> String {
        let bytes: Vec<char> = query.chars().collect();
        let mut out = String::with_capacity(query.len());
        for (idx, &ch) in bytes.iter().enumerate() {
            let keep = ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || ((ch == '-' || ch == '+')
                    && (idx + 1 == bytes.len() || bytes[idx + 1].is_ascii_digit()));
            if keep {
                out.push(ch);
            }
        }
        out
    }

    /// Resolves legacy shortcode suggestions for `raw_query`.
    ///
    /// Returns results ordered by:
    /// 1. Exact match (`replacement == format!("{raw_query}:")` or `:{raw_query}:`)
    /// 2. `words_used < 3`
    /// 3. `words_used < 2`
    /// 4. First char after `:` matches first char of normalized query.
    pub fn get_suggestions(&self, raw_query: &str) -> Vec<EmojiKeywordResult> {
        let query_norm = Self::normalize_query(&raw_query.to_ascii_lowercase());
        let query_chars: Vec<char> = query_norm.chars().collect();
        if query_chars.is_empty() {
            return Vec::new();
        }
        let first_char = query_chars[0];

        struct Match<'a> {
            item: &'a LegacyReplacement,
            words_used: usize,
        }

        let mut results: Vec<Match<'_>> = Vec::new();
        for item in &self.replacements {
            // Item must have at least one word starting with first_char (matching initialList lookup).
            if !item.words.iter().any(|w| w.starts_with(first_char)) {
                continue;
            }
            let words_used = if query_chars.len() == 1 {
                Some(1)
            } else if item.words.len() < 2 {
                item.words
                    .first()
                    .filter(|w| w.starts_with(&query_norm))
                    .map(|_| 1)
            } else {
                let mut used = vec![false; item.words.len()];
                let mut count = 1usize;
                if match_tail(&query_chars, 0, &item.words, &mut used, &mut count) {
                    Some(count)
                } else {
                    None
                }
            };

            if let Some(words_used) = words_used {
                let dup_of_last = results
                    .last()
                    .is_some_and(|last| last.item.emoji == item.emoji);
                if !dup_of_last {
                    results.push(Match { item, words_used });
                } else if let Some(last) = results.last_mut() {
                    let better = if words_used < last.words_used {
                        true
                    } else {
                        let good_last = last.item.replacement.chars().nth(1) == Some(first_char);
                        let good_cur = item.replacement.chars().nth(1) == Some(first_char);
                        !good_last && good_cur
                    };
                    if better {
                        *last = Match { item, words_used };
                    }
                }
            }
        }

        // Exact 4-pass stable partition order from `Completer::prepareResult()`.
        stable_partition(&mut results, |m| {
            m.item.replacement.chars().nth(1) == Some(first_char)
        });
        stable_partition(&mut results, |m| m.words_used < 2);
        stable_partition(&mut results, |m| m.words_used < 3);

        let initial_with_colon = if raw_query.starts_with(':') {
            raw_query.to_ascii_lowercase()
        } else {
            format!(":{}", raw_query.to_ascii_lowercase())
        };
        stable_partition(&mut results, |m| {
            m.item.replacement.len() == initial_with_colon.len() + 1
                && m.item.replacement.starts_with(&initial_with_colon)
        });

        results
            .into_iter()
            .map(|m| EmojiKeywordResult {
                emoji: m.item.emoji.clone(),
                label: m.item.replacement.clone(),
                replacement: m.item.replacement.clone(),
            })
            .collect()
    }
}

fn match_tail(
    query: &[char],
    pos: usize,
    words: &[String],
    used: &mut [bool],
    words_used: &mut usize,
) -> bool {
    if pos >= query.len() {
        return true;
    }
    let ch = query[pos];
    for (idx, word) in words.iter().enumerate() {
        if used[idx] {
            continue;
        }
        let word_chars: Vec<char> = word.chars().collect();
        if word_chars.first() != Some(&ch) {
            continue;
        }
        used[idx] = true;
        *words_used += 1;
        let max_eq = (query.len() - pos).min(word_chars.len());
        let mut eq = 1;
        while eq < max_eq && word_chars[eq] == query[pos + eq] {
            eq += 1;
        }
        for check in (1..=eq).rev() {
            if match_tail(query, pos + check, words, used, words_used) {
                return true;
            }
        }
        *words_used -= 1;
        used[idx] = false;
    }
    false
}

fn stable_partition<T, F>(slice: &mut [T], mut pred: F)
where
    F: FnMut(&T) -> bool,
{
    let mut matching = Vec::with_capacity(slice.len());
    let mut rest = Vec::with_capacity(slice.len());
    // Move via indices using drain-like swap on an owned Vec via temp buffer is cleaner,
    // but we can partition in-place with O(n) temp vec by taking ownership via Option or swap.
    // Since T is small, use a simple stable index permutation:
    let flags: Vec<bool> = slice.iter().map(&mut pred).collect();
    for (i, &f) in flags.iter().enumerate() {
        if f {
            matching.push(i);
        } else {
            rest.push(i);
        }
    }
    matching.extend(rest);
    // Apply permutation in-place.
    let mut visited = vec![false; slice.len()];
    for i in 0..slice.len() {
        if visited[i] || matching[i] == i {
            continue;
        }
        let mut cur = i;
        while !visited[cur] {
            visited[cur] = true;
            let next = matching[cur];
            if !visited[next] {
                slice.swap(cur, next);
            }
            cur = next;
        }
    }
}

/// Multi-language emoji keywords manager combining server language packs, LRU caching of
/// unused packs, legacy shortcode fallback, recent emoji prioritization, and skin tone variants.
///
/// Direct port of `ChatHelpers::EmojiKeywords` in `Telegram/SourceFiles/chat_helpers/emoji_keywords.cpp`.
#[derive(Debug, Clone, Default)]
pub struct EmojiKeywords {
    active_packs: BTreeMap<String, EmojiKeywordLangPack>,
    not_used_packs: VecDeque<EmojiKeywordLangPack>,
    remote_list: Vec<String>,
    input_languages: VecDeque<Vec<String>>,
    legacy: LegacyEmojiCompleter,
}

impl EmojiKeywords {
    /// Creates an empty manager with the built-in common legacy shortcodes.
    pub fn new() -> Self {
        Self {
            active_packs: BTreeMap::new(),
            not_used_packs: VecDeque::new(),
            remote_list: Vec::new(),
            input_languages: VecDeque::new(),
            legacy: LegacyEmojiCompleter::with_builtin_common(),
        }
    }

    /// Creates a manager with a custom [`LegacyEmojiCompleter`].
    pub fn with_legacy(legacy: LegacyEmojiCompleter) -> Self {
        Self {
            legacy,
            ..Self::default()
        }
    }

    /// Returns a reference to the legacy completer.
    pub fn legacy(&self) -> &LegacyEmojiCompleter {
        &self.legacy
    }

    /// Returns the active language packs map.
    pub fn active_packs(&self) -> &BTreeMap<String, EmojiKeywordLangPack> {
        &self.active_packs
    }

    /// Records the user's current input languages (keeping at most 4 recent lists),
    /// matching `EmojiKeywords::refreshInputLanguages`.
    pub fn record_input_languages(&mut self, langs: Vec<String>) {
        if let Some(pos) = self.input_languages.iter().position(|l| l == &langs) {
            if let Some(existing) = self.input_languages.remove(pos) {
                self.input_languages.push_back(existing);
            }
        } else {
            if self.input_languages.len() >= KEEP_NOT_USED_INPUT_LANGUAGES_COUNT {
                self.input_languages.pop_front();
            }
            self.input_languages.push_back(langs);
        }
    }

    /// Updates the active remote language list, moving removed packs into the 4-slot LRU
    /// `not_used_packs` queue and restoring any returning packs from it.
    ///
    /// Exact port of `EmojiKeywords::setRemoteList` and `refreshFromRemoteList`.
    pub fn set_remote_languages(&mut self, list: Vec<String>) {
        if self.remote_list == list {
            return;
        }
        self.remote_list = list;

        let active_keys: Vec<String> = self.active_packs.keys().cloned().collect();
        for key in active_keys {
            if !self.remote_list.contains(&key)
                && let Some(pack) = self.active_packs.remove(&key)
            {
                if self.not_used_packs.len() >= KEEP_NOT_USED_LANG_PACKS_COUNT {
                    self.not_used_packs.pop_front();
                }
                self.not_used_packs.push_back(pack);
            }
        }

        for id in &self.remote_list {
            if self.active_packs.contains_key(id) {
                continue;
            }
            if let Some(pos) = self.not_used_packs.iter().position(|p| p.lang_code == *id)
                && let Some(pack) = self.not_used_packs.remove(pos)
            {
                self.active_packs.insert(id.clone(), pack);
                continue;
            }
            self.active_packs
                .insert(id.clone(), EmojiKeywordLangPack::new(id.clone()));
        }
    }

    /// Applies an `emojiKeywordsDifference` payload to the pack for `lang_code`
    /// (creating and activating it if not already present).
    pub fn apply_difference(
        &mut self,
        lang_code: &str,
        version: i32,
        diffs: &[EmojiKeywordDiff],
    ) -> bool {
        let pack = self
            .active_packs
            .entry(lang_code.to_string())
            .or_insert_with(|| EmojiKeywordLangPack::new(lang_code));
        pack.apply_difference(lang_code, version, diffs)
    }

    /// Returns the maximum query length across all active language packs.
    pub fn max_query_length(&self) -> usize {
        self.active_packs
            .values()
            .map(|p| p.max_key_length)
            .max()
            .unwrap_or(0)
    }

    /// Searches all active language packs (plus legacy shortcodes when `!exact`),
    /// deduplicating results by emoji string.
    ///
    /// Exact port of `EmojiKeywords::query` in `chat_helpers/emoji_keywords.cpp`.
    pub fn query(&self, query: &str, exact: bool) -> Vec<EmojiKeywordResult> {
        let normalized = normalize_emoji_query(query);
        if normalized.is_empty() {
            return Vec::new();
        }
        let mut result: Vec<EmojiKeywordResult> = Vec::new();
        for pack in self.active_packs.values() {
            let list = pack.query(&normalized, exact);
            for entry in list {
                if !result.iter().any(|r| r.emoji == entry.emoji) {
                    result.push(entry);
                }
            }
        }
        if !exact {
            let valid_legacy = query
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+'));
            if valid_legacy {
                for entry in self.legacy.get_suggestions(query) {
                    if !result.iter().any(|r| r.emoji == entry.emoji) {
                        result.push(entry);
                    }
                }
            }
        }
        result
    }

    /// Searches and ranks results by prioritizing recently used emoji first and applying
    /// saved skin tone preferences.
    ///
    /// Exact port of `EmojiKeywords::queryMine`, `PrioritizeRecent`, and `ApplyVariants`.
    pub fn query_mine<F>(
        &self,
        query: &str,
        exact: bool,
        recent: &[RecentEmoji],
        variants: &EmojiVariants,
        has_variants: F,
    ) -> Vec<EmojiKeywordResult>
    where
        F: Fn(&str) -> bool,
    {
        let mut list = self.query(query, exact);
        prioritize_recent(&mut list, recent);
        for item in &mut list {
            if has_variants(&item.emoji) {
                item.emoji = variants.lookup_emoji(&item.emoji, true);
            }
        }
        list
    }
}

/// Rotates recently used emoji to the front of `list` preserving recent order,
/// matching `EmojiKeywords::PrioritizeRecent` in `chat_helpers/emoji_keywords.cpp`.
pub fn prioritize_recent(list: &mut [EmojiKeywordResult], recent: &[RecentEmoji]) {
    let mut last_recent = 0usize;
    for item in recent {
        let RecentEmojiId::Unicode(ref emoji_str) = item.id else {
            continue;
        };
        let original = strip_skin_tone(emoji_str);
        if let Some(pos) = list
            .iter()
            .position(|entry| strip_skin_tone(&entry.emoji) == original)
            && pos >= last_recent
        {
            if pos > last_recent {
                list[last_recent..=pos].rotate_right(1);
            }
            last_recent += 1;
        }
    }
}

/// Parsed emoji suggestion trigger under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiSuggestionQuery {
    /// Raw matched slice in the field (includes leading `:` for colon triggers).
    pub query: String,
    /// Query text passed to [`EmojiKeywords::query_mine`] (without leading `:`).
    pub keyword: String,
    /// Whether only exact keyword matches should be queried (true for first-word triggers
    /// and short uppercase/punctuation emoticons like `:D`).
    pub exact: bool,
    /// Start byte offset of the replaceable span in the input text.
    pub start_byte: usize,
    /// End byte offset of the replaceable span in the input text.
    pub end_byte: usize,
}

/// Parses a `:word` emoji suggestion trigger directly before `cursor_byte`.
///
/// Exact port of the colon loop in `SuggestionsController::getEmojiQuery`
/// (`chat_helpers/emoji_suggestions_widget.cpp:956-971`) and `SuggestionsWidget::getRowsByQuery`
/// (`321-336`):
/// - Colon `:` must NOT be preceded by a letter or digit (`foo:bar` does not trigger).
/// - Minimal length after `:` is 1 non-whitespace character (`:` or `: ` does not trigger).
/// - Scans back at most `max_chars` characters (`max(legacy_limit, modern_limit)`).
/// - A short colon query of length `<= 2` with no lowercase letters (e.g. `:D`) uses exact matching.
pub fn parse_emoji_colon_query(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    max_chars: usize,
) -> Option<EmojiSuggestionQuery> {
    if cursor_byte > text.len()
        || fragment_start_byte > cursor_byte
        || !text.is_char_boundary(cursor_byte)
        || !text.is_char_boundary(fragment_start_byte)
    {
        return None;
    }

    let fragment = &text[fragment_start_byte..cursor_byte];
    let chars: Vec<(usize, char)> = fragment.char_indices().collect();
    let len = chars.len();
    if len == 0 {
        return None;
    }

    let mut i = len;
    while i != 0 {
        i -= 1;
        let (byte_pos, ch) = chars[i];
        if ch == ':' {
            if i > 0 {
                let prev = chars[i - 1].1;
                if prev.is_alphabetic() || prev.is_numeric() {
                    return None;
                }
            }
            if i + 1 == len || chars[i + 1].1.is_whitespace() {
                return None;
            }
            let start_byte = fragment_start_byte + byte_pos;
            let raw = &text[start_byte..cursor_byte];
            let keyword = &raw[1..];
            let raw_chars = len - i;
            let simple = raw_chars <= 2 && !raw.chars().any(char::is_lowercase);
            return Some(EmojiSuggestionQuery {
                query: raw.to_string(),
                keyword: keyword.to_string(),
                exact: simple,
                start_byte,
                end_byte: cursor_byte,
            });
        }
        if len - i > max_chars {
            return None;
        }
    }
    None
}

/// Configuration for [`parse_emoji_suggestion_query`], matching `SuggestionsController::Options`
/// and keyword limits in `chat_helpers/emoji_suggestions_widget.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiSuggestionConfig {
    /// Whether emoji suggestions are enabled in settings (`Core::Settings::suggestEmoji()`).
    pub suggest_emoji_enabled: bool,
    /// Whether an exact match on the first word without `:` triggers suggestions.
    pub suggest_exact_first_word: bool,
    /// Maximum keyword length across loaded language packs (`EmojiKeywords::maxQueryLength()`).
    pub modern_limit: usize,
    /// Maximum shortcode length in legacy autocomplete (`GetSuggestionMaxLength()`).
    pub legacy_limit: usize,
}

impl Default for EmojiSuggestionConfig {
    fn default() -> Self {
        Self {
            suggest_emoji_enabled: true,
            suggest_exact_first_word: true,
            modern_limit: 32,
            legacy_limit: 32,
        }
    }
}

/// Parses either a `:word` trigger or an exact first-word emoji suggestion trigger.
///
/// Direct port of `SuggestionsController::getEmojiQuery` in
/// `Telegram/SourceFiles/chat_helpers/emoji_suggestions_widget.cpp`.
pub fn parse_emoji_suggestion_query(
    text: &str,
    cursor_byte: usize,
    fragment_start_byte: usize,
    has_selection: bool,
    config: EmojiSuggestionConfig,
) -> Option<EmojiSuggestionQuery> {
    if !config.suggest_emoji_enabled || has_selection {
        return None;
    }
    let max_limit = config.modern_limit.max(config.legacy_limit);
    if let Some(colon) = parse_emoji_colon_query(text, cursor_byte, fragment_start_byte, max_limit)
    {
        return Some(colon);
    }

    // Exact first-word query must span the entire field from 0 to end.
    if !config.suggest_exact_first_word || fragment_start_byte != 0 || cursor_byte != text.len() {
        return None;
    }
    let char_len = text.chars().count();
    if char_len == 0
        || text.chars().next().is_some_and(char::is_whitespace)
        || char_len > config.modern_limit
    {
        return None;
    }
    Some(EmojiSuggestionQuery {
        query: text.to_string(),
        keyword: text.to_string(),
        exact: true,
        start_byte: 0,
        end_byte: text.len(),
    })
}

/// Applies an emoji suggestion replacement to `text`, replacing `query.start_byte..query.end_byte`
/// with `replacement`, returning `(new_text, new_cursor_byte)`.
pub fn apply_emoji_suggestion_replacement(
    text: &str,
    query: &EmojiSuggestionQuery,
    replacement: &str,
) -> (String, usize) {
    let start = query.start_byte.min(text.len());
    let end = query.end_byte.min(text.len());
    let mut out = String::with_capacity(text.len() - (end - start) + replacement.len());
    out.push_str(&text[..start]);
    out.push_str(replacement);
    let new_cursor = out.len();
    out.push_str(&text[end..]);
    (out, new_cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_emoji_defaults_increment_decay_hide_and_limit() {
        let mut store = RecentEmojiStore::new();
        let list = store.list();
        assert_eq!(list.len(), DEFAULT_RECENT_EMOJI.len());
        assert_eq!(list[0].id, RecentEmojiId::Unicode("\u{1F602}".into()));

        // Hide a default emoji -> removed and skipped on reload
        let laugh = RecentEmojiId::Unicode("\u{1F602}".into());
        store.hide(&laugh);
        assert!(!store.list().iter().any(|e| e.id == laugh));

        // Incrementing a hidden default un-hides it and puts it at top of rating-1 group
        store.increment(laugh.clone());
        assert_eq!(store.list()[0].id, laugh);
        assert_eq!(store.list()[0].rating, 1);

        // Incrementing again raises rating to 2 -> strictly ahead of rating-1 items
        store.increment(laugh.clone());
        assert_eq!(store.list()[0].id, laugh);
        assert_eq!(store.list()[0].rating, 2);

        // Decay when exceeding 0x8000 (32768)
        let mut decay_store = RecentEmojiStore::new();
        decay_store.set_preload(vec![
            ("\u{1F525}".into(), RECENT_EMOJI_DECAY_THRESHOLD),
            ("\u{2764}\u{FE0F}".into(), 100),
            ("\u{1F44D}".into(), 1),
        ]);
        decay_store.increment(RecentEmojiId::Unicode("\u{1F525}".into()));
        let entries = decay_store.list();
        // 32768 + 1 = 32769 > 0x8000 => 32769 / 2 = 16384; 100 / 2 = 50; 1 stays 1
        assert_eq!(entries[0].rating, 16384);
        assert_eq!(entries[1].rating, 50);
        assert_eq!(entries[2].rating, 1);

        // Custom document serialization / parsing and test_mode filtering
        let doc_prod = RecentEmojiDocument {
            id: DocumentId(123456),
            test: false,
        };
        assert_eq!(doc_prod.serialize(), "123456-0");
        assert_eq!(RecentEmojiDocument::parse("123456-0"), Some(doc_prod));
        assert_eq!(RecentEmojiDocument::parse("0-0"), None);
        assert_eq!(RecentEmojiDocument::parse("123-2"), None);

        let doc_test = RecentEmojiDocument {
            id: DocumentId(999),
            test: true,
        };
        decay_store.increment(RecentEmojiId::Document(doc_prod));
        decay_store.increment(RecentEmojiId::Document(doc_test));
        let prod_visible = decay_store.visible_recent(false, false);
        assert!(prod_visible.contains(&RecentEmojiId::Document(doc_prod)));
        assert!(!prod_visible.contains(&RecentEmojiId::Document(doc_test)));
        let unicode_only = decay_store.visible_recent(false, true);
        assert!(!unicode_only.contains(&RecentEmojiId::Document(doc_prod)));
    }

    #[test]
    fn skin_tone_variants_per_emoji_and_global() {
        let mut variants = EmojiVariants::new();
        assert!(!variants.has_chosen("\u{1F44D}"));
        assert_eq!(variants.lookup_emoji("\u{1F44D}", true), "\u{1F44D}");

        // Per-emoji preference
        variants.save_variant("\u{1F44D}", SkinTone::MediumDark);
        assert!(variants.has_chosen("\u{1F44D}"));
        assert!(!variants.has_chosen("\u{1F44B}"));
        assert_eq!(
            variants.lookup_emoji("\u{1F44D}", true),
            "\u{1F44D}\u{1F3FE}"
        );
        // Non-colorizable emoji unchanged
        assert_eq!(variants.lookup_emoji("\u{1F525}", false), "\u{1F525}");

        // Global preference clears per-emoji overrides and applies to all colorizable emoji
        variants.save_all_variants(SkinTone::Light);
        assert!(variants.has_chosen("\u{1F44B}"));
        assert_eq!(
            variants.lookup_emoji("\u{1F44D}", true),
            "\u{1F44D}\u{1F3FB}"
        );
        assert_eq!(
            variants.lookup_emoji("\u{1F44B}", true),
            "\u{1F44B}\u{1F3FB}"
        );

        // Strip and detect helpers
        assert_eq!(strip_skin_tone("\u{1F44D}\u{1F3FE}"), "\u{1F44D}");
        assert_eq!(detect_skin_tone("\u{1F44D}\u{1F3FE}"), SkinTone::MediumDark);
        assert_eq!(detect_skin_tone("\u{1F44D}"), SkinTone::Default);
    }

    #[test]
    fn emoji_keywords_diff_application_normalization_and_search() {
        let mut kw = EmojiKeywords::new();
        kw.set_remote_languages(vec!["en".into(), "ru".into()]);

        // Apply initial diff to "en"
        assert!(kw.apply_difference(
            "en",
            1,
            &[
                EmojiKeywordDiff::Added {
                    keyword: "  Heart ".into(),
                    emoticons: vec!["\u{2764}\u{FE0F}".into(), "\u{1F499}".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "tm".into(),
                    emoticons: vec!["\u{2122}".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "hi".into(),
                    emoticons: vec!["\u{1F44B}".into()],
                },
                EmojiKeywordDiff::Added {
                    keyword: "go".into(),
                    emoticons: vec!["\u{1F3C3}".into()],
                },
            ],
        ));
        // TM gets U+FE0F postfix automatically
        let tm = kw.query("tm", false);
        assert_eq!(tm[0].emoji, "\u{2122}\u{FE0F}");
        assert_eq!(tm[0].replacement, "\u{2122}");

        // Exact keyword filtering rules for English:
        // "hi" is allowed, "go" (2-letter not in us/uk/hi/ok) is skipped, "10" is skipped
        assert_eq!(kw.query("hi", true).len(), 1);
        assert!(kw.query("go", true).is_empty());
        assert!(skip_exact_keyword("en", "10"));
        assert!(skip_exact_keyword("en", "a"));
        assert!(skip_exact_keyword("en", "go"));
        assert!(!skip_exact_keyword("en", "ok"));
        assert!(!skip_exact_keyword("ru", "да"));

        // Apply deletion diff
        assert!(kw.apply_difference(
            "en",
            2,
            &[EmojiKeywordDiff::Deleted {
                keyword: "heart".into(),
                emoticons: vec!["\u{1F499}".into()],
            }],
        ));
        let hearts = kw.query("hea", false);
        assert_eq!(hearts[0].emoji, "\u{2764}\u{FE0F}");
        assert!(!hearts.iter().any(|r| r.emoji == "\u{1F499}"));

        // No-op empty diff with lower/equal version returns false
        assert!(!kw.apply_difference("en", 2, &[]));

        // Prioritize recent + apply skin tone variants
        kw.apply_difference(
            "en",
            3,
            &[EmojiKeywordDiff::Added {
                keyword: "hand".into(),
                emoticons: vec!["\u{1F44B}".into(), "\u{1F44D}".into()],
            }],
        );
        let recent = vec![RecentEmoji {
            id: RecentEmojiId::Unicode("\u{1F44D}\u{1F3FD}".into()),
            rating: 5,
        }];
        let mut variants = EmojiVariants::new();
        variants.save_variant("\u{1F44D}", SkinTone::Dark);
        let mine = kw.query_mine("hand", false, &recent, &variants, |e| {
            matches!(strip_skin_tone(e).as_str(), "\u{1F44D}" | "\u{1F44B}")
        });
        // 👍 was recent (even with different skin tone), so it rotates to index 0 and gets Dark tone!
        assert_eq!(mine[0].emoji, "\u{1F44D}\u{1F3FF}");
        assert_eq!(mine[1].emoji, "\u{1F44B}");
    }

    #[test]
    fn emoji_suggestion_triggers_and_replacement() {
        let cfg = EmojiSuggestionConfig::default();

        // Basic :word trigger
        let q = parse_emoji_suggestion_query("hello :fir", 10, 0, false, cfg)
            .expect("should match :fir");
        assert_eq!(q.query, ":fir");
        assert_eq!(q.keyword, "fir");
        assert!(!q.exact);
        assert_eq!((q.start_byte, q.end_byte), (6, 10));

        let (replaced, cursor) = apply_emoji_suggestion_replacement("hello :fir", &q, "\u{1F525}");
        assert_eq!(replaced, "hello \u{1F525}");
        assert_eq!(cursor, replaced.len());

        // Minimal length: ":" alone or ": " must NOT trigger
        assert!(parse_emoji_colon_query(":", 1, 0, 32).is_none());
        assert!(parse_emoji_colon_query(": ", 2, 0, 32).is_none());
        // ":f" (length 2, 1 char after colon) triggers
        assert!(parse_emoji_colon_query(":f", 2, 0, 32).is_some());

        // Preceded by letter or digit (e.g. "http://", "12:30") must NOT trigger
        assert!(parse_emoji_colon_query("http:foo", 8, 0, 32).is_none());
        assert!(parse_emoji_colon_query("12:30", 5, 0, 32).is_none());

        // Short uppercase/punctuation emoticon like ":D" forces exact match
        let q_d = parse_emoji_colon_query(":D", 2, 0, 32).unwrap();
        assert!(q_d.exact);
        assert_eq!(q_d.keyword, "D");

        // Exact first-word trigger when cursor is at end of full input
        let q_first = parse_emoji_suggestion_query("fire", 4, 0, false, cfg).unwrap();
        assert!(q_first.exact);
        assert_eq!(q_first.keyword, "fire");

        // First-word trigger does NOT fire if cursor is not at end or text starts with space
        assert!(parse_emoji_suggestion_query("fire!", 4, 0, false, cfg).is_none());
        assert!(parse_emoji_suggestion_query(" fire", 5, 0, false, cfg).is_none());

        // Disabled setting suppresses all suggestions
        let disabled = EmojiSuggestionConfig {
            suggest_emoji_enabled: false,
            ..cfg
        };
        assert!(parse_emoji_suggestion_query(":fir", 4, 0, false, disabled).is_none());
    }

    #[test]
    fn emoji_keywords_lru_pack_eviction_and_restore() {
        let mut kw = EmojiKeywords::new();
        kw.set_remote_languages(vec!["en".into()]);
        kw.apply_difference(
            "en",
            7,
            &[EmojiKeywordDiff::Added {
                keyword: "rocket".into(),
                emoticons: vec!["\u{1F680}".into()],
            }],
        );
        // Switch away from "en" -> moves to not_used_packs, retaining version 7
        kw.set_remote_languages(vec!["de".into()]);
        assert!(!kw.active_packs().contains_key("en"));
        // Switch back to "en" -> restored from LRU cache with version 7 intact!
        kw.set_remote_languages(vec!["en".into()]);
        assert_eq!(kw.active_packs()["en"].version, 7);
        assert_eq!(kw.query("rock", false)[0].emoji, "\u{1F680}");
    }

    #[test]
    fn legacy_completer_normalization_and_ordering() {
        assert_eq!(
            LegacyEmojiCompleter::normalize_query("thumbs_up"),
            "thumbsup"
        );
        assert_eq!(LegacyEmojiCompleter::normalize_query("+1"), "+1");
        assert_eq!(LegacyEmojiCompleter::normalize_query("foo-bar"), "foobar");
        assert_eq!(LegacyEmojiCompleter::normalize_query("foo-"), "foo-");

        let completer = LegacyEmojiCompleter::with_builtin_common();
        let res = completer.get_suggestions("heart");
        // Exact match ":heart:" must come before ":heart_eyes:"
        assert_eq!(res[0].label, ":heart:");
        assert_eq!(res[1].label, ":heart_eyes:");

        // Full tg-text compiled emoji_autocomplete.json table has >1000 entries and finds exact matches
        let full = LegacyEmojiCompleter::with_builtin_all();
        assert!(full.max_length() > 0);
        let full_heart = full.get_suggestions("heart");
        assert_eq!(full_heart[0].label, ":heart:");
        assert!(!full.get_suggestions("avocado").is_empty());
    }
}
