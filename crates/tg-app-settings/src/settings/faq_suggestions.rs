//! Telegram FAQ suggestions fetched via `messages.getWebPage` and ranked search matching
//! for Settings search.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/settings_faq_suggestions.h`
//! - `Telegram/SourceFiles/settings/settings_faq_suggestions.cpp`

use tg_lang::tr;

use super::backend::{FaqEntry, SettingsBackend};

/// Curated offline fallback FAQ entries used in mock/offline mode when the server FAQ webpage
/// has not returned cached blocks.
#[must_use]
pub fn default_offline_faq_entries() -> Vec<FaqEntry> {
    let base = "https://telegram.org/faq";
    vec![
        FaqEntry {
            section: "General Questions".to_owned(),
            title: "What is Telegram? What do I do here?".to_owned(),
            url: format!("{base}#q-what-is-telegram-what-do-i-do-here"),
        },
        FaqEntry {
            section: "Telegram Basics".to_owned(),
            title: "How do I create a group or channel?".to_owned(),
            url: format!("{base}#q-how-do-i-create-a-group"),
        },
        FaqEntry {
            section: "Telegram Basics".to_owned(),
            title: "How do Chat Folders work?".to_owned(),
            url: format!("{base}#q-chat-folders"),
        },
        FaqEntry {
            section: "Privacy and Security".to_owned(),
            title: "How secure is Telegram? Two-Step Verification & Passcodes".to_owned(),
            url: format!("{base}#q-how-secure-is-telegram"),
        },
        FaqEntry {
            section: "Privacy and Security".to_owned(),
            title: "Who can see my phone number and last seen time?".to_owned(),
            url: format!("{base}#q-who-can-see-my-phone-number"),
        },
        FaqEntry {
            section: "Privacy and Security".to_owned(),
            title: "How do I set up Two-Step Verification (Cloud Password)?".to_owned(),
            url: format!("{base}#q-how-does-2-step-verification-work"),
        },
        FaqEntry {
            section: "Privacy and Security".to_owned(),
            title: "How do I manage active sessions and connected devices?".to_owned(),
            url: format!("{base}#q-how-do-i-log-out"),
        },
        FaqEntry {
            section: "Your Account".to_owned(),
            title: "How do I change my phone number or username?".to_owned(),
            url: format!("{base}#q-how-do-i-change-my-phone-number"),
        },
        FaqEntry {
            section: "Your Account".to_owned(),
            title: "How do I delete my account or set auto-delete inactivity?".to_owned(),
            url: format!("{base}#q-how-do-i-delete-my-account"),
        },
        FaqEntry {
            section: "Notifications and Calls".to_owned(),
            title: "Why am I not receiving notifications or call audio?".to_owned(),
            url: format!("{base}#q-notification-problems"),
        },
        FaqEntry {
            section: "Bots and Mini Apps".to_owned(),
            title: "What are bots and how do I create or translate bot info?".to_owned(),
            url: format!("{base}#q-what-are-bots"),
        },
        FaqEntry {
            section: "Data and Storage".to_owned(),
            title: "How do I export my Telegram data and manage local cache?".to_owned(),
            url: format!("{base}#q-how-do-i-export-my-data"),
        },
    ]
}

/// Stateful cache for Settings FAQ suggestions (`Settings::FaqSuggestions`).
#[derive(Debug, Clone, Default)]
pub struct FaqSuggestionsState {
    entries: Vec<FaqEntry>,
    loaded: bool,
}

impl FaqSuggestionsState {
    /// Creates an empty, unloaded [`FaqSuggestionsState`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if FAQ suggestions have already been loaded.
    #[must_use]
    pub const fn loaded(&self) -> bool {
        self.loaded
    }

    /// Returns all cached [`FaqEntry`] items.
    #[must_use]
    pub fn entries(&self) -> &[FaqEntry] {
        &self.entries
    }

    /// Ensures FAQ entries have been fetched from `backend` (`messages.getWebPage`) once.
    pub fn ensure_loaded(&mut self, backend: &dyn SettingsBackend) {
        if self.loaded {
            return;
        }
        let raw_link = tr::lng_settings_faq_link();
        let base_url = raw_link
            .split_once('#')
            .map_or(raw_link.as_str(), |(before, _)| before);
        match backend.fetch_faq_suggestions(base_url) {
            Ok(list) if !list.is_empty() => {
                self.entries = list;
            }
            _ => {
                self.entries = default_offline_faq_entries();
            }
        }
        self.loaded = true;
    }

    /// Replaces cached entries directly (used by tests and live sync hooks).
    pub fn set_entries(&mut self, entries: Vec<FaqEntry>) {
        self.entries = entries;
        self.loaded = true;
    }

    /// Searches cached entries for `query`, returning ranked matches (highest relevance first).
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&FaqEntry> {
        search_faq_entries(&self.entries, query)
    }
}

/// Searches `entries` for `query` using case-insensitive multi-term matching and ranks results:
/// exact title match > title prefix > all terms in title > section + title match.
#[must_use]
pub fn search_faq_entries<'a>(entries: &'a [FaqEntry], query: &str) -> Vec<&'a FaqEntry> {
    let trimmed = query.trim().to_lowercase();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let terms: Vec<&str> = trimmed.split_whitespace().collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(u32, usize, &'a FaqEntry)> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        let title_low = entry.title.to_lowercase();
        let section_low = entry.section.to_lowercase();
        let combined = format!("{title_low} {section_low}");
        if !terms.iter().all(|t| combined.contains(t)) {
            continue;
        }

        let mut score = 0u32;
        if title_low == trimmed {
            score += 1000;
        } else if title_low.starts_with(&trimmed) {
            score += 500;
        } else if title_low.contains(&trimmed) {
            score += 300;
        }

        let title_term_hits = terms.iter().filter(|t| title_low.contains(*t)).count() as u32;
        score += title_term_hits * 60;
        if title_term_hits as usize == terms.len() {
            score += 120;
        }
        if section_low.contains(&trimmed) {
            score += 40;
        }
        scored.push((score, idx, entry));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faq_search_ranks_title_matches_above_section_matches() {
        let entries = default_offline_faq_entries();
        let hits = search_faq_entries(&entries, "two-step");
        assert!(!hits.is_empty());
        assert!(hits[0].title.to_lowercase().contains("two-step"));

        let privacy_hits = search_faq_entries(&entries, "privacy");
        assert!(privacy_hits.len() >= 3);
        assert!(search_faq_entries(&entries, "   ").is_empty());
    }
}
