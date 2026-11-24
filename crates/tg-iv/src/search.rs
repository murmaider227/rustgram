//! Port of `Telegram/SourceFiles/iv/iv_search_controller.h` and `iv_search_controller.cpp`.
//!
//! Provides [`IvSearchController`], a headless in-page search engine with case-insensitive
//! Unicode/UTF-8 match indexing, overlapping and multi-block match discovery, next/prev
//! wrap-around cycling, collapsed `<details>` expansion, query caching, and HTML-safe
//! snippet highlighting.

use std::collections::HashMap;

use crate::model::{ListItemContent, Page, PageBlock, RichText};
use crate::prepare::escape_html;
use crate::rich_page::{RichBlock, RichBlockKind, RichPage};

/// A single searchable text segment extracted from an Instant View document
/// (`Iv::Markdown::MarkdownArticleSearchSource`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchSource {
    /// Visible searchable text in this block/segment.
    pub text: String,
    /// Optional searchable text hidden inside a collapsed `<details>` block.
    pub hidden_text: String,
    /// Anchor ID of the collapsed `<details>` block owning `hidden_text`.
    pub details_anchor_id: String,
}

impl SearchSource {
    /// Creates a visible [`SearchSource`] with no hidden details payload.
    #[must_use]
    pub fn visible(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            hidden_text: String::new(),
            details_anchor_id: String::new(),
        }
    }

    /// Creates a [`SearchSource`] representing a collapsible `<details>` block whose body
    /// is currently collapsed.
    #[must_use]
    pub fn collapsible(
        summary: impl Into<String>,
        hidden_body: impl Into<String>,
        details_anchor_id: impl Into<String>,
    ) -> Self {
        Self {
            text: summary.into(),
            hidden_text: hidden_body.into(),
            details_anchor_id: details_anchor_id.into(),
        }
    }
}

/// A single search hit located in a [`SearchSource`] (`Iv::SearchController::SearchEntry` /
/// `MarkdownArticleSearchMatch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Index of the matching [`SearchSource`] segment.
    pub segment: usize,
    /// Start UTF-8 byte offset within `source.text` (or `source.hidden_text` when hidden).
    pub byte_start: usize,
    /// Exclusive end UTF-8 byte offset within `source.text` (or `source.hidden_text` when hidden).
    pub byte_end: usize,
    /// Present when this match resides in a collapsed `<details>` section that must be
    /// expanded before scrolling/highlighting.
    pub hidden_details_id: Option<String>,
}

/// Highlighted context snippet around a single [`SearchMatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSnippet {
    /// Segment index owning the match.
    pub segment: usize,
    /// Plain-text context preceding the match (prefixed with `"…"` if truncated).
    pub prefix: String,
    /// Exact matched text slice from the original source.
    pub matched: String,
    /// Plain-text context following the match (suffixed with `"…"` if truncated).
    pub suffix: String,
    /// HTML-escaped snippet with `<mark class="iv-search-hit">` around the match.
    pub highlighted_html: String,
}

/// Configuration for [`IvSearchController`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    /// When `false` (default), performs Unicode case-insensitive matching.
    pub case_sensitive: bool,
    /// When `true` (default), finds overlapping occurrences (e.g. `"ana"` twice in `"banana"`).
    pub allow_overlapping: bool,
    /// Number of leading/trailing Unicode scalar values included in snippets.
    pub snippet_context_chars: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            allow_overlapping: true,
            snippet_context_chars: 32,
        }
    }
}

/// Headless Instant View in-page search controller (`Iv::SearchController`).
#[derive(Debug, Clone)]
pub struct IvSearchController {
    shown: bool,
    query: String,
    sources: Vec<SearchSource>,
    matches: Vec<SearchMatch>,
    current_index: Option<usize>,
    cache: HashMap<String, Vec<SearchMatch>>,
    generation: u64,
    options: SearchOptions,
}

impl Default for IvSearchController {
    fn default() -> Self {
        Self::new()
    }
}

impl IvSearchController {
    /// Creates an empty [`IvSearchController`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            shown: false,
            query: String::new(),
            sources: Vec::new(),
            matches: Vec::new(),
            current_index: None,
            cache: HashMap::new(),
            generation: 0,
            options: SearchOptions::default(),
        }
    }

    /// Creates a controller preloaded with explicit [`SearchSource`] segments.
    #[must_use]
    pub fn from_sources(sources: Vec<SearchSource>) -> Self {
        Self {
            sources,
            ..Self::new()
        }
    }

    /// Creates a controller indexed over all text blocks of a [`RichPage`].
    #[must_use]
    pub fn from_rich_page(page: &RichPage) -> Self {
        Self::from_sources(extract_search_sources_from_rich_page(page))
    }

    /// Creates a controller indexed over all text blocks of a [`Page`].
    #[must_use]
    pub fn from_page(page: &Page) -> Self {
        Self::from_sources(extract_search_sources_from_page(page))
    }

    /// Toggles search bar visibility, returning the new visibility state.
    pub fn toggle(&mut self) -> bool {
        if self.shown {
            self.hide();
        } else {
            self.show();
        }
        self.shown
    }

    /// Shows the search bar and refreshes active matches.
    pub fn show(&mut self) {
        self.shown = true;
        self.refresh();
    }

    /// Hides the search bar, clears matches, and invalidates the query cache.
    pub fn hide(&mut self) {
        self.shown = false;
        self.invalidate_session();
        self.matches.clear();
        self.current_index = None;
    }

    /// Returns `true` when the search UI is visible.
    #[must_use]
    pub const fn is_shown(&self) -> bool {
        self.shown
    }

    /// Returns the current search options.
    #[must_use]
    pub const fn options(&self) -> SearchOptions {
        self.options
    }

    /// Updates [`SearchOptions`], invalidates the cache, and recomputes matches.
    pub fn set_options(&mut self, options: SearchOptions) {
        if self.options == options {
            return;
        }
        self.options = options;
        self.invalidate_session();
        if !self.query.is_empty() {
            self.rebuild_results(0);
        }
    }

    /// Replaces the indexed [`SearchSource`] segments and refreshes active search results.
    pub fn set_sources(&mut self, sources: Vec<SearchSource>) {
        self.sources = sources;
        self.invalidate_session();
        if !self.query.is_empty() {
            let preferred = self.current_index.unwrap_or(0);
            self.rebuild_results(preferred);
        } else {
            self.matches.clear();
            self.current_index = None;
        }
    }

    /// Returns the indexed search sources.
    #[must_use]
    pub fn sources(&self) -> &[SearchSource] {
        &self.sources
    }

    /// Returns the current cache invalidation generation counter.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Invalidates cached query results and rebuilds matches for the current query.
    pub fn refresh(&mut self) {
        self.invalidate_session();
        if !self.query.is_empty() {
            let preferred = self.current_index.unwrap_or(0);
            self.rebuild_results(preferred);
        }
    }

    /// Updates the active query string, recomputes (or retrieves cached) matches,
    /// selects the first match if any exist, and returns the total match count.
    pub fn set_query(&mut self, query: impl Into<String>) -> usize {
        self.query = query.into();
        self.rebuild_results(0);
        self.matches.len()
    }

    /// Returns the active search query string.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns all indexed matches for the active query.
    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    /// Returns the total number of active matches.
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Returns the 0-based index of the currently selected match.
    #[must_use]
    pub const fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    /// Returns the currently selected [`SearchMatch`], if any.
    #[must_use]
    pub fn current_match(&self) -> Option<&SearchMatch> {
        self.current_index.and_then(|idx| self.matches.get(idx))
    }

    /// Returns `(1-based current, total)` suitable for rendering `"3 / 12"` in a search bar
    /// (`_searchBar->setResults(total ? current + 1 : 0, total)`).
    #[must_use]
    pub fn display_position(&self) -> (usize, usize) {
        let total = self.matches.len();
        match self.current_index {
            Some(idx) if idx < total => (idx + 1, total),
            _ => (0, total),
        }
    }

    /// Advances to the next match, wrapping from the last match back to `0`.
    pub fn next_match(&mut self) -> Option<&SearchMatch> {
        self.step(1)
    }

    /// Moves to the previous match, wrapping from `0` back to the last match.
    pub fn prev_match(&mut self) -> Option<&SearchMatch> {
        self.step(-1)
    }

    /// Moves the active match index by `delta` with wrap-around (`stepSearchResult`).
    pub fn step(&mut self, delta: isize) -> Option<&SearchMatch> {
        let total = self.matches.len();
        if total == 0 {
            self.current_index = None;
            return None;
        }
        let current = self.current_index.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(total as isize) as usize;
        self.current_index = Some(next);
        self.matches.get(next)
    }

    /// If the current match lives inside a collapsed `<details>` block, invokes `expand_fn`
    /// with the details anchor ID to obtain updated sources, rescans, and updates
    /// `current_index` to land on the materialized match (`resolveCurrentSearchEntry`).
    pub fn expand_details_and_resolve<F>(&mut self, mut expand_fn: F) -> Option<&SearchMatch>
    where
        F: FnMut(&str) -> Option<Vec<SearchSource>>,
    {
        loop {
            let current = self.current_index?;
            let anchor_id = self
                .matches
                .get(current)
                .and_then(|m| m.hidden_details_id.clone());
            let Some(anchor_id) = anchor_id else {
                return self.matches.get(current);
            };
            let mut run_start = current;
            while run_start > 0
                && self.matches[run_start - 1].hidden_details_id.as_deref()
                    == Some(anchor_id.as_str())
            {
                run_start -= 1;
            }
            let mut run_end = current + 1;
            while run_end < self.matches.len()
                && self.matches[run_end].hidden_details_id.as_deref() == Some(anchor_id.as_str())
            {
                run_end += 1;
            }
            let offset = current - run_start;
            let old_total = self.matches.len();
            let Some(new_sources) = expand_fn(&anchor_id) else {
                return self.matches.get(current);
            };
            self.sources = new_sources;
            self.invalidate_session();
            let rescanned = scan_search_entries(&self.sources, &self.query, self.options);
            self.cache.insert(self.query.clone(), rescanned.clone());
            self.matches = rescanned;

            let new_total = self.matches.len();
            let removed = run_end - run_start;
            let materialized = (new_total as isize) - ((old_total - removed) as isize);
            if new_total == 0 {
                self.current_index = None;
                return None;
            } else if materialized <= 0 {
                self.current_index = Some(run_start.min(new_total - 1));
            } else {
                let target = run_start + offset.min((materialized as usize) - 1);
                self.current_index = Some(target.min(new_total - 1));
            }
        }
    }

    /// Builds a highlighted context snippet for match `match_index`.
    #[must_use]
    pub fn snippet_for(&self, match_index: usize) -> Option<SearchSnippet> {
        let hit = self.matches.get(match_index)?;
        let source = self.sources.get(hit.segment)?;
        let text = if hit.hidden_details_id.is_some() {
            source.hidden_text.as_str()
        } else {
            source.text.as_str()
        };
        if hit.byte_start > hit.byte_end
            || hit.byte_end > text.len()
            || !text.is_char_boundary(hit.byte_start)
            || !text.is_char_boundary(hit.byte_end)
        {
            return None;
        }

        let ctx = self.options.snippet_context_chars;
        let before_str = &text[..hit.byte_start];
        let matched = &text[hit.byte_start..hit.byte_end];
        let after_str = &text[hit.byte_end..];

        let before_chars: Vec<(usize, char)> = before_str.char_indices().collect();
        let (prefix, truncated_left) = if before_chars.len() > ctx {
            let cut = before_chars[before_chars.len() - ctx].0;
            (&before_str[cut..], true)
        } else {
            (before_str, false)
        };

        let after_chars: Vec<(usize, char)> = after_str.char_indices().collect();
        let (suffix, truncated_right) = if after_chars.len() > ctx {
            let cut = after_chars[ctx].0;
            (&after_str[..cut], true)
        } else {
            (after_str, false)
        };

        let prefix_text = if truncated_left {
            format!("…{prefix}")
        } else {
            prefix.to_string()
        };
        let suffix_text = if truncated_right {
            format!("{suffix}…")
        } else {
            suffix.to_string()
        };

        let highlighted_html = format!(
            "{}<mark class=\"iv-search-hit\">{}</mark>{}",
            escape_html(&prefix_text),
            escape_html(matched),
            escape_html(&suffix_text),
        );

        Some(SearchSnippet {
            segment: hit.segment,
            prefix: prefix_text,
            matched: matched.to_string(),
            suffix: suffix_text,
            highlighted_html,
        })
    }

    /// Builds a highlighted context snippet for the currently active match.
    #[must_use]
    pub fn current_snippet(&self) -> Option<SearchSnippet> {
        let idx = self.current_index?;
        self.snippet_for(idx)
    }

    /// Renders `sources[segment_index].text` as HTML with all visible matches in that
    /// segment wrapped in `<mark class="iv-search-hit">` (and `iv-search-current` for the
    /// active match). Overlapping match intervals are merged cleanly so output is always
    /// valid, properly escaped HTML.
    #[must_use]
    pub fn highlight_segment_html(&self, segment_index: usize) -> Option<String> {
        let source = self.sources.get(segment_index)?;
        let text = source.text.as_str();
        let mut intervals: Vec<(usize, usize, bool)> = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.segment == segment_index && m.hidden_details_id.is_none())
            .filter(|(_, m)| {
                m.byte_start < m.byte_end
                    && m.byte_end <= text.len()
                    && text.is_char_boundary(m.byte_start)
                    && text.is_char_boundary(m.byte_end)
            })
            .map(|(i, m)| (m.byte_start, m.byte_end, self.current_index == Some(i)))
            .collect();

        if intervals.is_empty() {
            return Some(escape_html(text));
        }

        intervals.sort_by_key(|&(start, end, _)| (start, end));
        let mut merged: Vec<(usize, usize, bool)> = Vec::with_capacity(intervals.len());
        for (start, end, is_current) in intervals {
            if let Some(last) = merged.last_mut()
                && start < last.1
            {
                last.1 = last.1.max(end);
                last.2 = last.2 || is_current;
            } else {
                merged.push((start, end, is_current));
            }
        }

        let mut out = String::with_capacity(text.len() + merged.len() * 40);
        let mut cursor = 0usize;
        for (start, end, is_current) in merged {
            if start > cursor {
                out.push_str(&escape_html(&text[cursor..start]));
            }
            let class = if is_current {
                "iv-search-hit iv-search-current"
            } else {
                "iv-search-hit"
            };
            out.push_str("<mark class=\"");
            out.push_str(class);
            out.push_str("\">");
            out.push_str(&escape_html(&text[start..end]));
            out.push_str("</mark>");
            cursor = end;
        }
        if cursor < text.len() {
            out.push_str(&escape_html(&text[cursor..]));
        }
        Some(out)
    }

    fn invalidate_session(&mut self) {
        self.cache.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    fn rebuild_results(&mut self, preferred_current: usize) {
        if self.query.is_empty() {
            self.matches.clear();
            self.current_index = None;
            return;
        }
        let entries = if let Some(cached) = self.cache.get(&self.query) {
            cached.clone()
        } else {
            let scanned = scan_search_entries(&self.sources, &self.query, self.options);
            self.cache.insert(self.query.clone(), scanned.clone());
            scanned
        };
        let total = entries.len();
        self.matches = entries;
        self.current_index = if total > 0 {
            Some(preferred_current.min(total - 1))
        } else {
            None
        };
    }
}

/// Scans all `sources` for `query` according to `options`.
#[must_use]
pub fn scan_search_entries(
    sources: &[SearchSource],
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for (segment, source) in sources.iter().enumerate() {
        for (byte_start, byte_end) in find_matches_in_str(&source.text, query, options) {
            results.push(SearchMatch {
                segment,
                byte_start,
                byte_end,
                hidden_details_id: None,
            });
        }
        if !source.hidden_text.is_empty() {
            let anchor = if source.details_anchor_id.is_empty() {
                format!("details-{segment}")
            } else {
                source.details_anchor_id.clone()
            };
            for (byte_start, byte_end) in find_matches_in_str(&source.hidden_text, query, options) {
                results.push(SearchMatch {
                    segment,
                    byte_start,
                    byte_end,
                    hidden_details_id: Some(anchor.clone()),
                });
            }
        }
    }
    results
}

/// Finds all UTF-8 byte ranges `(start, end)` in `haystack` matching `query`.
///
/// Correctly handles multi-byte Unicode case folding (including characters whose lowercase
/// expansion changes UTF-8 byte length) and guarantees returned offsets lie on UTF-8 char
/// boundaries of `haystack`.
#[must_use]
pub fn find_matches_in_str(
    haystack: &str,
    query: &str,
    options: SearchOptions,
) -> Vec<(usize, usize)> {
    if haystack.is_empty() || query.is_empty() {
        return Vec::new();
    }

    if options.case_sensitive {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos <= haystack.len() {
            let Some(found) = haystack[pos..].find(query) else {
                break;
            };
            let start = pos + found;
            let end = start + query.len();
            out.push((start, end));
            if options.allow_overlapping {
                let Some(ch) = haystack[start..].chars().next() else {
                    break;
                };
                pos = start + ch.len_utf8();
            } else {
                pos = end;
            }
        }
        return out;
    }

    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return Vec::new();
    }

    // Map each folded character back to its original UTF-8 character byte span.
    let mut folded: Vec<char> = Vec::with_capacity(haystack.len());
    let mut orig_start: Vec<usize> = Vec::with_capacity(haystack.len());
    let mut orig_end: Vec<usize> = Vec::with_capacity(haystack.len());
    let mut is_char_start: Vec<bool> = Vec::with_capacity(haystack.len());
    let mut is_char_end: Vec<bool> = Vec::with_capacity(haystack.len());

    for (byte_idx, ch) in haystack.char_indices() {
        let byte_end = byte_idx + ch.len_utf8();
        let first_idx = folded.len();
        for lc in ch.to_lowercase() {
            folded.push(lc);
            orig_start.push(byte_idx);
            orig_end.push(byte_end);
            is_char_start.push(false);
            is_char_end.push(false);
        }
        let last_idx = folded.len() - 1;
        is_char_start[first_idx] = true;
        is_char_end[last_idx] = true;
    }

    let n = folded.len();
    let m = needle.len();
    if m > n {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut i = 0usize;
    while i + m <= n {
        if is_char_start[i] && is_char_end[i + m - 1] && folded[i..i + m] == needle[..] {
            out.push((orig_start[i], orig_end[i + m - 1]));
            if options.allow_overlapping {
                i += 1;
                while i < n && !is_char_start[i] {
                    i += 1;
                }
            } else {
                i += m;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Extracts searchable text segments from every block of a [`RichPage`].
#[must_use]
pub fn extract_search_sources_from_rich_page(page: &RichPage) -> Vec<SearchSource> {
    let mut sources = Vec::new();
    collect_rich_blocks(&page.blocks, &mut sources);
    sources
}

fn collect_rich_blocks(blocks: &[RichBlock], out: &mut Vec<SearchSource>) {
    for block in blocks {
        match block.kind {
            RichBlockKind::Details if !block.open => {
                let mut nested = Vec::new();
                collect_rich_blocks(&block.blocks, &mut nested);
                let hidden = nested
                    .into_iter()
                    .map(|s| {
                        if s.hidden_text.is_empty() {
                            s.text
                        } else if s.text.is_empty() {
                            s.hidden_text
                        } else {
                            format!("{}\n{}", s.text, s.hidden_text)
                        }
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                let summary = block.text.text.text.clone();
                if !summary.is_empty() || !hidden.is_empty() {
                    out.push(SearchSource::collapsible(summary, hidden, &block.anchor_id));
                }
            }
            RichBlockKind::Math => {
                if !block.formula.is_empty() {
                    out.push(SearchSource::visible(&block.formula));
                }
            }
            RichBlockKind::Channel => {
                if !block.channel_title.is_empty() {
                    out.push(SearchSource::visible(&block.channel_title));
                }
            }
            RichBlockKind::RelatedArticles => {
                if !block.text.text.text.is_empty() {
                    out.push(SearchSource::visible(&block.text.text.text));
                }
                for article in &block.related_articles {
                    let combined = [
                        article.title.as_str(),
                        article.description.as_str(),
                        article.author.as_str(),
                    ]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                    if !combined.is_empty() {
                        out.push(SearchSource::visible(combined));
                    }
                }
            }
            _ => {
                if !block.text.text.text.is_empty() {
                    out.push(SearchSource::visible(&block.text.text.text));
                }
                for item in &block.list_items {
                    if !item.text.text.text.is_empty() {
                        out.push(SearchSource::visible(&item.text.text.text));
                    }
                    collect_rich_blocks(&item.blocks, out);
                }
                for row in &block.table_rows {
                    for cell in &row.cells {
                        if !cell.text.text.text.is_empty() {
                            out.push(SearchSource::visible(&cell.text.text.text));
                        }
                    }
                }
                collect_rich_blocks(&block.blocks, out);
                if !block.caption.text.text.is_empty() {
                    out.push(SearchSource::visible(&block.caption.text.text));
                }
            }
        }
    }
}

/// Extracts searchable text segments from every block of a [`Page`].
#[must_use]
pub fn extract_search_sources_from_page(page: &Page) -> Vec<SearchSource> {
    let mut sources = Vec::new();
    collect_page_blocks(&page.blocks, &mut sources);
    sources
}

fn push_non_empty(out: &mut Vec<SearchSource>, text: String) {
    if !text.is_empty() {
        out.push(SearchSource::visible(text));
    }
}

fn collect_page_blocks(blocks: &[PageBlock], out: &mut Vec<SearchSource>) {
    for block in blocks {
        match block {
            PageBlock::Title(t)
            | PageBlock::Subtitle(t)
            | PageBlock::Header(t)
            | PageBlock::Subheader(t)
            | PageBlock::Kicker(t)
            | PageBlock::Paragraph(t)
            | PageBlock::Footer(t)
            | PageBlock::Thinking(t) => push_non_empty(out, t.to_plain_text()),
            PageBlock::Heading { text, .. } => push_non_empty(out, text.to_plain_text()),
            PageBlock::AuthorDate { author, .. } => push_non_empty(out, author.to_plain_text()),
            PageBlock::Preformatted { text, .. } => push_non_empty(out, text.to_plain_text()),
            PageBlock::Blockquote { text, caption, .. }
            | PageBlock::Pullquote { text, caption } => {
                push_non_empty(out, text.to_plain_text());
                push_non_empty(out, caption.to_plain_text());
            }
            PageBlock::BlockquoteBlocks {
                blocks: nested,
                caption,
                ..
            } => {
                collect_page_blocks(nested, out);
                push_non_empty(out, caption.to_plain_text());
            }
            PageBlock::List { items } => {
                for item in items {
                    match &item.content {
                        ListItemContent::Text(t) => push_non_empty(out, t.to_plain_text()),
                        ListItemContent::Blocks(nested) => collect_page_blocks(nested, out),
                    }
                }
            }
            PageBlock::OrderedList { items, .. } => {
                for item in items {
                    match &item.content {
                        ListItemContent::Text(t) => push_non_empty(out, t.to_plain_text()),
                        ListItemContent::Blocks(nested) => collect_page_blocks(nested, out),
                    }
                }
            }
            PageBlock::Table { title, rows, .. } => {
                push_non_empty(out, title.to_plain_text());
                for row in rows {
                    for cell in &row.cells {
                        if let Some(ref t) = cell.text {
                            push_non_empty(out, t.to_plain_text());
                        }
                    }
                }
            }
            PageBlock::Details {
                title,
                blocks: nested,
                open,
            } => {
                if *open {
                    push_non_empty(out, title.to_plain_text());
                    collect_page_blocks(nested, out);
                } else {
                    let mut inner = Vec::new();
                    collect_page_blocks(nested, &mut inner);
                    let hidden = inner
                        .into_iter()
                        .map(|s| s.text)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let summary = title.to_plain_text();
                    if !summary.is_empty() || !hidden.is_empty() {
                        out.push(SearchSource::collapsible(summary, hidden, ""));
                    }
                }
            }
            PageBlock::Math { source } => push_non_empty(out, source.clone()),
            PageBlock::Cover(inner) => collect_page_blocks(std::slice::from_ref(inner), out),
            PageBlock::Photo { caption, .. }
            | PageBlock::Video { caption, .. }
            | PageBlock::Audio { caption, .. }
            | PageBlock::Document { caption, .. }
            | PageBlock::Embed { caption, .. }
            | PageBlock::Map { caption, .. } => {
                push_non_empty(out, caption.text.to_plain_text());
                push_non_empty(out, caption.credit.to_plain_text());
            }
            PageBlock::Collage { items, caption } | PageBlock::Slideshow { items, caption } => {
                collect_page_blocks(items, out);
                push_non_empty(out, caption.text.to_plain_text());
                push_non_empty(out, caption.credit.to_plain_text());
            }
            PageBlock::EmbedPost {
                author,
                blocks: nested,
                caption,
                ..
            } => {
                push_non_empty(out, author.clone());
                collect_page_blocks(nested, out);
                push_non_empty(out, caption.text.to_plain_text());
            }
            PageBlock::RelatedArticles { title, articles } => {
                push_non_empty(out, title.to_plain_text());
                for article in articles {
                    let combined = [
                        article.title.as_deref().unwrap_or(""),
                        article.description.as_deref().unwrap_or(""),
                        article.author.as_deref().unwrap_or(""),
                    ]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                    push_non_empty(out, combined);
                }
            }
            PageBlock::Channel(ch) => push_non_empty(out, ch.title.clone()),
            PageBlock::Divider | PageBlock::Anchor { .. } | PageBlock::Unsupported => {}
        }
    }
}

/// Convenience helper returning plain text of a [`RichText`] for search diagnostics.
#[must_use]
pub fn rich_text_search_plain(text: &RichText) -> String {
    text.to_plain_text()
}
