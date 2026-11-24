//! Live RPC helpers for WP-52 UX extras (`settings/settings_faq_suggestions.cpp`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/settings_faq_suggestions.cpp`

use std::time::Duration;

use tg_api::RpcClient;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::settings::backend::FaqEntry;

const FAQ_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Fetches Telegram FAQ table-of-contents entries via `messages.getWebPage` and parses its
/// `cached_page` blocks (`Settings::FaqSuggestions::request` + `parse`).
pub fn fetch_faq_suggestions(live: &LiveSession, faq_url: &str) -> Result<Vec<FaqEntry>, String> {
    let base = faq_url
        .split_once('#')
        .map_or(faq_url, |(before, _)| before)
        .trim();
    if base.is_empty() {
        return Ok(Vec::new());
    }
    let req = tl_fns::messages::GetWebPage {
        url: base.to_owned(),
        hash: 0,
    };
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(FAQ_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })?;

    let tl_enums::messages::WebPage::WebPage(resp) = res;
    let tl_enums::WebPage::WebPage(wp) = resp.webpage else {
        return Ok(Vec::new());
    };
    Ok(parse_faq_entries_from_webpage(&wp))
}

/// Parses TOC sections and `#anchor` links from a Telegram FAQ [`tl_types::WebPage`]
/// (`FaqSuggestions::parse` in `settings_faq_suggestions.cpp:115-159`).
#[must_use]
pub fn parse_faq_entries_from_webpage(page: &tl_types::WebPage) -> Vec<FaqEntry> {
    let Some(tl_enums::Page::Page(cached)) = page.cached_page.as_ref() else {
        return Vec::new();
    };
    let base_url = page.url.as_str();
    let mut entries = Vec::new();
    let mut current_section = String::new();
    let mut in_toc = false;

    for block in &cached.blocks {
        match block {
            tl_enums::PageBlock::Paragraph(p) => {
                if let Some(sec) = extract_bold_section(&p.text) {
                    current_section = sec;
                    in_toc = true;
                } else {
                    in_toc = false;
                }
            }
            tl_enums::PageBlock::List(list) => {
                if in_toc && !current_section.is_empty() {
                    for item in &list.items {
                        if let tl_enums::PageListItem::Text(t) = item
                            && let Some((title, url)) = extract_toc_url(&t.text, base_url)
                        {
                            entries.push(FaqEntry {
                                section: current_section.clone(),
                                title,
                                url,
                            });
                        }
                    }
                }
            }
            tl_enums::PageBlock::Divider | tl_enums::PageBlock::Header(_) => {
                in_toc = false;
            }
            _ => {}
        }
        if !in_toc && !entries.is_empty() {
            break;
        }
    }
    entries
}

fn extract_plain_text(text: &tl_enums::RichText, depth: u8) -> String {
    if depth >= 16 {
        return String::new();
    }
    match text {
        tl_enums::RichText::TextPlain(t) => t.text.clone(),
        tl_enums::RichText::TextConcat(c) => {
            let mut out = String::new();
            for part in &c.texts {
                out.push_str(&extract_plain_text(part, depth + 1));
            }
            out
        }
        tl_enums::RichText::TextBold(b) => extract_plain_text(&b.text, depth + 1),
        tl_enums::RichText::TextItalic(i) => extract_plain_text(&i.text, depth + 1),
        tl_enums::RichText::TextUrl(u) => extract_plain_text(&u.text, depth + 1),
        _ => String::new(),
    }
}

fn extract_bold_section(text: &tl_enums::RichText) -> Option<String> {
    if let tl_enums::RichText::TextBold(b) = text {
        let s = extract_plain_text(&b.text, 0);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn extract_toc_url(text: &tl_enums::RichText, base_url: &str) -> Option<(String, String)> {
    if let tl_enums::RichText::TextUrl(u) = text {
        let prefix = format!("{base_url}#");
        if u.url.starts_with(&prefix) {
            let title = extract_plain_text(&u.text, 0);
            if !title.is_empty() {
                return Some((title, u.url.clone()));
            }
        }
    }
    None
}
