//! Message search result snippet, ported from `MessageView::prepare` with `searchLowerText`
//! (`dialogs/ui/dialogs_message_view.cpp`).
//!
//! tdesktop builds the preview text with the group sender inside it
//! (`PreviewWithSender`: `"\u{200E}\u{2068}{from}\u{2069}: {message}"`, where `toPreview`
//! wraps the message in the same marks), colors the first match of every query word
//! (`HistoryView::FindSearchQueryHighlight`) and cuts the text so the earliest match starts
//! `kLeftShift = 15` characters from the left, prefixed with `kQEllipsis` (`"..."`). The
//! direction marks count in that cut, so `"You: Welcome to Parity"` (match 21 characters in)
//! leaves `"...: Welcome to Parity"`. The line is then elided at the right edge.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId};

/// `kLeftShift` in `MessageView::prepare`.
const LEFT_SHIFT: usize = 15;
/// `kQEllipsis` (`lib_ui/ui/text/text.cpp`).
const LEADING_ELLIPSIS: &str = "...";
/// Left-to-right mark `st::wrap_rtl` puts before the sender.
const LRM: char = '\u{200E}';
/// First strong isolate opening the sender name.
const FSI: char = '\u{2068}';
/// Pop directional isolate closing the sender name.
const PDI: char = '\u{2069}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// Direction mark: counts in the cut, never drawn.
    Hidden,
    /// `Ui::Text::Colorized` sender prefix (`"You:"`).
    Sender,
    /// Message text.
    Text,
}

/// One drawn run of a [`SearchSnippet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetRun {
    /// Visible text of the run.
    pub text: String,
    /// Drawn in `dialogsTextFgService` (sender prefix or a query match).
    pub service: bool,
    /// Hidden under the spoiler cover (the message text's spoilers).
    pub spoiler: bool,
}

/// A message search result line split into plain and service-coloured runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchSnippet {
    /// Runs in drawing order.
    pub runs: Vec<SnippetRun>,
}

impl SearchSnippet {
    /// Visible text of all runs.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Per drawn char, whether it is hidden under the spoiler cover; empty when nothing is.
    #[must_use]
    pub fn hidden_chars(&self) -> Vec<bool> {
        if !self.runs.iter().any(|r| r.spoiler) {
            return Vec::new();
        }
        self.runs
            .iter()
            .flat_map(|r| std::iter::repeat_n(r.spoiler, r.text.chars().count()))
            .collect()
    }

    /// Texts of the service-coloured runs.
    #[must_use]
    pub fn service_texts(&self) -> Vec<&str> {
        self.runs
            .iter()
            .filter(|r| r.service)
            .map(|r| r.text.as_str())
            .collect()
    }

    /// Single-line job elided with `…` at `max_width` (`elisionLines = 1`).
    #[must_use]
    pub fn layout_job(
        &self,
        font_id: &FontId,
        text_fg: Color32,
        service_fg: Color32,
        max_width: f32,
    ) -> LayoutJob {
        let mut job = LayoutJob {
            wrap: TextWrapping {
                max_width,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('\u{2026}'),
            },
            ..LayoutJob::default()
        };
        for run in &self.runs {
            job.append(
                &run.text,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: if run.spoiler {
                        Color32::TRANSPARENT
                    } else if run.service {
                        service_fg
                    } else {
                        text_fg
                    },
                    ..Default::default()
                },
            );
        }
        job
    }
}

/// `Ui::Text::Words`: the lowercased query split on non-word characters.
fn query_words(query: &str) -> Vec<Vec<char>> {
    query
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().collect())
        .collect()
}

/// `FindSearchQueryHighlight(text, word)` over lowercased chars.
fn find_word_highlight(inside: &[char], word: &[char]) -> Option<Range<usize>> {
    let find = |part: &[char]| -> Option<usize> {
        if part.is_empty() || part.len() > inside.len() {
            return None;
        }
        // tdesktop checks only the first occurrence of `part`.
        let from = (0..=inside.len() - part.len()).find(|&i| inside[i..].starts_with(part))?;
        (from == 0 || !inside[from - 1].is_alphanumeric()).then_some(from)
    };
    let till_end_of_word = |from: usize| -> Range<usize> {
        let till = (from + 1..inside.len())
            .find(|&i| !inside[i].is_alphanumeric())
            .unwrap_or(inside.len());
        from..till
    };
    if let Some(from) = find(word) {
        let till = from + word.len();
        if till >= inside.len() || !inside[till].is_alphanumeric() {
            return Some(from..till);
        }
    }
    // `word` is already one of `Ui::Text::Words(query)`, so tdesktop's inner word loop runs once.
    let length = word.len();
    let cut = length / 2;
    let offset = find(&word[..length - cut])?;
    for i in 0..cut {
        if let Some(from) = find(&word[..length - i]) {
            return Some(till_end_of_word(from));
        }
    }
    Some(till_end_of_word(offset))
}

/// Builds the search result line for `text` (already without the media marker), with the
/// group `sender` (`"You"`, a short name) included like `PreviewWithSender` when given.
#[must_use]
pub fn message_search_snippet(sender: Option<&str>, text: &str, query: &str) -> SearchSnippet {
    message_search_snippet_with_spoilers(sender, text, &[], query)
}

/// [`message_search_snippet`] with the byte ranges of `text` under a spoiler kept hidden
/// (`MessageView::prepare` keeps the preview's entities).
#[must_use]
pub fn message_search_snippet_with_spoilers(
    sender: Option<&str>,
    text: &str,
    spoilers: &[Range<usize>],
    query: &str,
) -> SearchSnippet {
    let mut chars: Vec<(char, Part)> = Vec::new();
    if let Some(sender) = sender {
        chars.push((LRM, Part::Hidden));
        chars.push((FSI, Part::Hidden));
        chars.extend(sender.chars().map(|c| (c, Part::Sender)));
        chars.push((PDI, Part::Hidden));
        chars.push((':', Part::Sender));
        chars.push((' ', Part::Text));
    }
    // `HistoryItem::toPreview` wraps the message text with `st::wrap_rtl` too.
    chars.push((LRM, Part::Hidden));
    chars.push((FSI, Part::Hidden));
    let text_from = chars.len();
    let lead = text.len() - text.trim_start().len();
    let mut hidden_text = Vec::new();
    for (at, c) in text.trim().char_indices() {
        chars.push((c, Part::Text));
        let byte = lead + at;
        hidden_text.push(spoilers.iter().any(|r| r.contains(&byte)));
    }
    chars.push((PDI, Part::Hidden));
    let is_spoiler = |i: usize| {
        i.checked_sub(text_from)
            .and_then(|t| hidden_text.get(t))
            .copied()
            .unwrap_or(false)
    };

    // One lowercase char per source char keeps indices aligned (UTF-16 `toLower` does too
    // for all but a few special cases).
    let lower: Vec<char> = chars
        .iter()
        .map(|(c, _)| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut highlighted = vec![false; chars.len()];
    let mut min_from = None::<usize>;
    for word in query_words(query) {
        if let Some(range) = find_word_highlight(&lower, &word) {
            min_from = Some(min_from.map_or(range.start, |m| m.min(range.start)));
            for flag in &mut highlighted[range] {
                *flag = true;
            }
        }
    }

    let cut = match min_from {
        Some(from) if from <= chars.len() && from >= LEFT_SHIFT => from - LEFT_SHIFT,
        _ => 0,
    };

    let mut snippet = SearchSnippet::default();
    let mut push = |text: &str, service: bool, spoiler: bool| match snippet.runs.last_mut() {
        Some(last) if last.service == service && last.spoiler == spoiler => {
            last.text.push_str(text);
        }
        _ => snippet.runs.push(SnippetRun {
            text: text.to_owned(),
            service,
            spoiler,
        }),
    };
    if cut > 0 {
        push(LEADING_ELLIPSIS, false, false);
    }
    let mut buf = [0u8; 4];
    for (i, (c, part)) in chars.iter().enumerate().skip(cut) {
        if *part == Part::Hidden {
            continue;
        }
        let service = *part == Part::Sender || highlighted[i];
        push(c.encode_utf8(&mut buf), service, is_spoiler(i));
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_counts_in_the_left_cut_like_tdesktop() {
        // Stage-3 official capture: "...: Welcome to Parity Test Group! ..."
        let s = message_search_snippet(
            Some("You"),
            "Welcome to Parity Test Group! Checking group chat layout and member list.",
            "Parity",
        );
        assert!(s.text().starts_with("...: Welcome to Parity Test Group!"));
        assert_eq!(s.service_texts(), vec![":", "Parity"]);
    }

    #[test]
    fn early_match_keeps_the_sender() {
        let s = message_search_snippet(Some("You"), "Parity check", "parity");
        assert_eq!(s.text(), "You: Parity check");
        assert_eq!(s.service_texts(), vec!["You:", "Parity"]);
    }

    #[test]
    fn text_is_cut_fifteen_chars_before_the_first_match() {
        let s = message_search_snippet(
            None,
            "Hello! Checking 1:1 chat parity with text, reply, photo, file, and reactions.",
            "Parity",
        );
        assert_eq!(
            s.text(),
            "...cking 1:1 chat parity with text, reply, photo, file, and reactions."
        );
        assert_eq!(s.service_texts(), vec!["parity"]);
    }

    #[test]
    fn only_the_first_word_start_match_is_coloured() {
        let s = message_search_snippet(None, "build the nightly build", "build");
        assert_eq!(s.service_texts(), vec!["build"]);
        // A match inside a word is not a match (`FindSearchQueryHighlight`).
        let s = message_search_snippet(None, "rebuild", "build");
        assert!(s.service_texts().is_empty());
        assert_eq!(s.text(), "rebuild");
    }

    #[test]
    fn prefix_of_a_longer_word_colours_to_the_end_of_the_word() {
        let s = message_search_snippet(None, "Parity testing", "test");
        assert_eq!(s.service_texts(), vec!["testing"]);
    }

    #[test]
    fn every_query_word_is_coloured() {
        let s = message_search_snippet(None, "layout and member list", "member layout");
        assert_eq!(s.service_texts(), vec!["layout", "member"]);
    }

    #[test]
    fn found_results_header_uses_the_7_4_6_wording() {
        use crate::dialogs::strings::search_found_results;
        assert_eq!(search_found_results(3), "3 messages found");
        assert_eq!(search_found_results(1), "1 message found");
    }

    #[test]
    fn job_is_one_elided_line() {
        let s = message_search_snippet(None, "abc", "abc");
        let job = s.layout_job(
            &FontId::proportional(13.0),
            Color32::GRAY,
            Color32::BLUE,
            40.0,
        );
        assert_eq!(job.wrap.max_rows, 1);
        assert_eq!(job.wrap.overflow_character, Some('\u{2026}'));
    }
}
