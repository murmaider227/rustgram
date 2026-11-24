//! Port of `iv/iv_rich_message_html_export.cpp` and `iv/iv_rich_page.cpp` (Page -> HTML generator).
//!
//! Converts a [`Page`] model into a complete, XSS-safe HTML5 document suitable for
//! either local WebView hosting (via [`crate::serve::IvServer`]) or standalone HTML export.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::assets::{PAGE_CSS, PAGE_JS};
use crate::grouped_layout::{
    DEFAULT_GROUP_SPACING, DEFAULT_GROUP_WIDTH_MAX, DEFAULT_GROUP_WIDTH_MIN, MediaRect, MediaSize,
    layout_media_group,
};
use crate::model::{
    COLLAGE_MAX_ITEMS, DEFAULT_MAP_HEIGHT, DEFAULT_MAP_WIDTH, DocumentMeta, ListItemContent,
    MAX_NESTING_DEPTH, Page, PageBlock, PageCaption, PageTableRow, PhotoMeta, RichText, TableAlign,
    TableValign, TaskState, normalize_fragment_id, trim_math_source,
};

/// Default allowlist of HTTPS embed hosts permitted when using [`EmbedPolicy::SandboxedAllowlist`].
pub const DEFAULT_EMBED_ALLOWLIST: &[&str] = &[
    "www.youtube.com",
    "www.youtube-nocookie.com",
    "player.vimeo.com",
    "platform.twitter.com",
    "www.instagram.com",
    "w.soundcloud.com",
    "open.spotify.com",
    "codepen.io",
    "coub.com",
    "player.twitch.tv",
];

/// Policy governing `pageBlockEmbed` rendering (`iv_rich_message_html_export.cpp`,
/// `iv_markdown_embed_overlay.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedPolicy {
    /// Never render `<iframe>` elements; emit a clickable link card or placeholder instead.
    LinkOnly,
    /// Block embeds entirely and emit a placeholder message.
    Blocked,
    /// Render sandboxed `<iframe>` elements for HTTPS URLs whose host matches `allowed_hosts`
    /// (exact match or subdomain match). Untrusted hosts and raw `embed.html` fall back to
    /// safe link cards unless `allow_sandboxed_html` is true (in which case raw HTML is isolated
    /// behind an `<iframe sandbox="allow-scripts" srcdoc="...">` or custom-scheme `/embed/{i}`).
    SandboxedAllowlist {
        /// Allowed HTTPS embed hostnames (lowercase).
        allowed_hosts: Vec<String>,
        /// Whether raw `embed.html` may be rendered inside a strict sandboxed iframe.
        allow_sandboxed_html: bool,
    },
}

impl Default for EmbedPolicy {
    fn default() -> Self {
        Self::SandboxedAllowlist {
            allowed_hosts: DEFAULT_EMBED_ALLOWLIST
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allow_sandboxed_html: false,
        }
    }
}

/// Configuration for [`prepare_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
    /// Optional URL prefix for served assets and media (e.g. `""` for relative `/css/page.css`,
    /// `"tg-iv://localhost"` or `"http://desktop-app-resource"` for custom-scheme WebView hosts).
    pub base_url: String,
    /// If true, inline CSS and JS directly into `<style>` / `<script>` tags (ideal for standalone
    /// HTML export). If false, reference `/css/page.css` and `/js/page.js`.
    pub inline_assets: bool,
    /// Include the interactive WebView bridge (`page.js`). Set to false for static-only export.
    pub include_bridge: bool,
    /// Policy for `pageBlockEmbed` iframes.
    pub embed_policy: EmbedPolicy,
    /// Explicit theme override: `None` follows `prefers-color-scheme`, `Some(true)` forces dark,
    /// `Some(false)` forces light.
    pub dark_mode: Option<bool>,
    /// Initial zoom percentage (25..=400). Applied as inline style when not 100.
    pub zoom_percent: i32,
    /// Show deleted/inserted diff highlights for [`RichText::Diff`].
    pub display_text_diff: bool,
    /// Optional overrides mapping `photo_id` -> resolved `src` URL (e.g. `data:` URI or local file).
    pub photo_urls: HashMap<i64, String>,
    /// Optional overrides mapping `document_id` -> resolved `src` URL.
    pub document_urls: HashMap<i64, String>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            inline_assets: false,
            include_bridge: true,
            embed_policy: EmbedPolicy::default(),
            dark_mode: None,
            zoom_percent: 100,
            display_text_diff: false,
            photo_urls: HashMap::new(),
            document_urls: HashMap::new(),
        }
    }
}

/// Metadata for an embedded HTML/URL widget discovered during page preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEmbed {
    /// Zero-based index within the page (served at `/embed/{index}` if raw HTML is stored).
    pub index: usize,
    /// Safe external URL, if present.
    pub url: Option<String>,
    /// Raw HTML payload from `pageBlockEmbed`, if present.
    pub html: Option<String>,
    /// Requested width.
    pub width: Option<i32>,
    /// Requested height.
    pub height: Option<i32>,
    /// Full-width flag.
    pub full_width: bool,
    /// Allow-scrolling flag.
    pub allow_scrolling: bool,
}

/// Output of [`prepare_page`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPage {
    /// Complete HTML5 document.
    pub html: String,
    /// Derived article title.
    pub title: String,
    /// Derived site name.
    pub site_name: String,
    /// Whether the page is right-to-left.
    pub rtl: bool,
    /// Ordered list of unique normalized anchor fragment IDs in this page.
    pub anchors: Vec<String>,
    /// Unique photo IDs referenced in this page (for media preloading).
    pub photo_ids: Vec<i64>,
    /// Unique document IDs referenced in this page (for media preloading).
    pub document_ids: Vec<i64>,
    /// Whether the page contains an interactive slideshow.
    pub has_slideshow: bool,
    /// Embedded widgets discovered during rendering.
    pub embeds: Vec<PreparedEmbed>,
}

/// Escapes HTML special characters (`&`, `<`, `>`, `"`, `'`), matching `EscapeHtml`
/// in `iv_rich_message_html_export.cpp`.
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    append_escaped(&mut out, input, false);
    out
}

/// Appends HTML-escaped text to `out`, optionally translating `\n` to `<br>\n`.
pub fn append_escaped(out: &mut String, input: &str, line_breaks: bool) {
    for ch in input.chars() {
        match ch {
            '\n' if line_breaks => out.push_str("<br>\n"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

/// Validates and normalizes a URL for safe insertion into `href` attributes,
/// matching `SafeUrl` in `iv_rich_message_html_export.cpp` with hardened control-char filtering.
///
/// Rejects `javascript:`, `vbscript:`, `data:`, `file:`, control characters, and unknown schemes.
pub fn safe_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.bytes().any(|b| b <= 0x1F || b == 0x7F) {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('#') {
        let frag = normalize_fragment_id(rest);
        return if frag.is_empty() {
            None
        } else {
            Some(format!("#{frag}"))
        };
    }
    let lower = trimmed.to_ascii_lowercase();
    const ALLOWED_PREFIXES: &[&str] = &[
        "http://",
        "https://",
        "tg://",
        "tonsite://",
        "ton://",
        "mailto:",
        "tel:",
        "ftp://",
    ];
    if ALLOWED_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Checks whether a URL is a valid `https://` URL whose host is in `allowed_hosts`
/// (exact match or `.host` subdomain match).
pub fn is_allowed_embed_url(url: &str, allowed_hosts: &[String]) -> bool {
    let trimmed = url.trim();
    if trimmed.bytes().any(|b| b <= 0x1F || b == 0x7F) {
        return false;
    }
    let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("HTTPS://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    allowed_hosts.iter().any(|allowed| {
        let a = allowed.trim().to_ascii_lowercase();
        !a.is_empty() && (host == a || (host.len() > a.len() && host.ends_with(&format!(".{a}"))))
    })
}

/// Formats a Unix timestamp (seconds) as a deterministic UTC long date string
/// (e.g. `"November 22, 2026"`). Returns `None` for non-positive timestamps.
pub fn format_utc_date(unix_seconds: i32) -> Option<String> {
    if unix_seconds <= 0 {
        return None;
    }
    let days = i64::from(unix_seconds).div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    const MONTHS: &[&str] = &[
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month_name = MONTHS.get((month - 1) as usize)?;
    Some(format!("{month_name} {day}, {year}"))
}

/// Howard Hinnant's civil-from-days algorithm (Gregorian calendar, epoch 1970-01-01 = day 0).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
}

/// Renders a [`Page`] to a complete HTML5 [`PreparedPage`].
pub fn prepare_page(page: &Page, options: &RenderOptions) -> PreparedPage {
    let photo_map = page.photo_map();
    let doc_map = page.document_map();
    let mut ctx = RenderCtx {
        options,
        photo_map,
        doc_map,
        anchors: Vec::new(),
        anchor_set: HashSet::new(),
        photo_ids: Vec::new(),
        photo_id_set: HashSet::new(),
        document_ids: Vec::new(),
        document_id_set: HashSet::new(),
        has_slideshow: false,
        embeds: Vec::new(),
    };

    let mut body = String::new();
    ctx.render_blocks(&mut body, &page.blocks, 0);

    if page.views.is_some() || page.id != 0 {
        body.push_str("<div class=\"iv-page-meta\">");
        if let Some(views) = page.views {
            let _ = write!(body, "<span class=\"iv-views\">{views} views</span>");
        } else {
            body.push_str("<span></span>");
        }
        if page.id != 0 {
            let _ = write!(
                body,
                "<a class=\"iv-report-link\" href=\"#\" data-iv-report=\"{}\">Wrong layout?</a>",
                page.id
            );
        }
        body.push_str("</div>\n");
    }

    let title = derive_page_title(page);
    let site_name = page.display_site_name();

    let mut html = String::with_capacity(body.len() + 1024);
    html.push_str("<!DOCTYPE html>\n<html");
    if page.rtl {
        html.push_str(" dir=\"rtl\"");
    }
    if let Some(dark) = options.dark_mode {
        html.push_str(if dark {
            " data-theme=\"dark\""
        } else {
            " data-theme=\"light\""
        });
    }
    let zoom = options.zoom_percent.clamp(25, 400);
    if zoom != 100 {
        let _ = write!(html, " style=\"zoom:{zoom}%\"");
    }
    html.push_str(">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"generator\" content=\"Rustgram Instant View\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>");
    append_escaped(&mut html, &title, false);
    html.push_str("</title>\n");

    let base = options.base_url.trim_end_matches('/');
    if options.inline_assets {
        html.push_str("<style>\n");
        html.push_str(PAGE_CSS);
        html.push_str("</style>\n");
    } else {
        let _ = writeln!(
            html,
            "<link rel=\"stylesheet\" href=\"{}/css/page.css\">",
            escape_html(base)
        );
    }

    html.push_str("</head>\n<body>\n<article>\n");
    html.push_str(&body);
    html.push_str("</article>\n");

    if options.include_bridge || ctx.has_slideshow {
        if options.inline_assets {
            html.push_str("<script>\n");
            html.push_str(PAGE_JS);
            html.push_str("</script>\n");
        } else {
            let _ = writeln!(
                html,
                "<script src=\"{}/js/page.js\"></script>",
                escape_html(base)
            );
        }
    }

    html.push_str("</body>\n</html>\n");

    PreparedPage {
        html,
        title,
        site_name,
        rtl: page.rtl,
        anchors: ctx.anchors,
        photo_ids: ctx.photo_ids,
        document_ids: ctx.document_ids,
        has_slideshow: ctx.has_slideshow,
        embeds: ctx.embeds,
    }
}

fn derive_page_title(page: &Page) -> String {
    for block in &page.blocks {
        match block {
            PageBlock::Title(rt) | PageBlock::Heading { level: 1, text: rt } => {
                let t = rt.to_plain_text();
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            _ => {}
        }
    }
    for block in &page.blocks {
        if let PageBlock::Heading { text: rt, .. }
        | PageBlock::Header(rt)
        | PageBlock::Subtitle(rt)
        | PageBlock::Paragraph(rt) = block
        {
            let t = rt.to_plain_text();
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(64).collect();
            }
        }
    }
    let site = page.display_site_name();
    if !site.is_empty() {
        site
    } else {
        "Instant View".to_string()
    }
}

struct RenderCtx<'a> {
    options: &'a RenderOptions,
    photo_map: HashMap<i64, &'a PhotoMeta>,
    doc_map: HashMap<i64, &'a DocumentMeta>,
    anchors: Vec<String>,
    anchor_set: HashSet<String>,
    photo_ids: Vec<i64>,
    photo_id_set: HashSet<i64>,
    document_ids: Vec<i64>,
    document_id_set: HashSet<i64>,
    has_slideshow: bool,
    embeds: Vec<PreparedEmbed>,
}

impl<'a> RenderCtx<'a> {
    fn record_anchor(&mut self, name: &str) -> Option<String> {
        let norm = normalize_fragment_id(name);
        if norm.is_empty() {
            return None;
        }
        if self.anchor_set.insert(norm.clone()) {
            self.anchors.push(norm.clone());
        }
        Some(norm)
    }

    fn record_photo(&mut self, id: i64) {
        if id != 0 && self.photo_id_set.insert(id) {
            self.photo_ids.push(id);
        }
    }

    fn record_document(&mut self, id: i64) {
        if id != 0 && self.document_id_set.insert(id) {
            self.document_ids.push(id);
        }
    }

    fn media_url(&self, kind: &str, id: i64) -> String {
        if kind == "photo"
            && let Some(override_url) = self.options.photo_urls.get(&id)
        {
            return override_url.clone();
        }
        if kind != "photo"
            && let Some(override_url) = self.options.document_urls.get(&id)
        {
            return override_url.clone();
        }
        let base = self.options.base_url.trim_end_matches('/');
        format!("{base}/media/{kind}/{id}")
    }

    fn render_blocks(&mut self, out: &mut String, blocks: &[PageBlock], depth: usize) {
        if depth >= MAX_NESTING_DEPTH {
            return;
        }
        for block in blocks {
            self.render_block(out, block, depth);
        }
    }

    fn render_block(&mut self, out: &mut String, block: &PageBlock, depth: usize) {
        if depth >= MAX_NESTING_DEPTH {
            return;
        }
        match block {
            PageBlock::Unsupported => {}
            PageBlock::Title(rt) => {
                out.push_str("<h1>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</h1>\n");
            }
            PageBlock::Subtitle(rt) => {
                out.push_str("<h2>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</h2>\n");
            }
            PageBlock::Header(rt) => {
                out.push_str("<h3>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</h3>\n");
            }
            PageBlock::Subheader(rt) => {
                out.push_str("<h4>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</h4>\n");
            }
            PageBlock::Kicker(rt) => {
                out.push_str("<p class=\"kicker\">");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</p>\n");
            }
            PageBlock::Heading { level, text } => {
                let lvl = (*level).clamp(1, 6);
                let _ = write!(out, "<h{lvl}>");
                self.render_rich_text(out, text, true, 0);
                let _ = writeln!(out, "</h{lvl}>");
            }
            PageBlock::AuthorDate {
                author,
                published_date,
            } => {
                out.push_str("<p class=\"byline\">");
                let has_author = !author.is_empty();
                if has_author {
                    self.render_rich_text(out, author, true, 0);
                }
                if let Some(date_str) = format_utc_date(*published_date) {
                    if has_author {
                        out.push_str(" · ");
                    }
                    append_escaped(out, &date_str, false);
                }
                out.push_str("</p>\n");
            }
            PageBlock::Paragraph(rt) => {
                out.push_str("<p>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</p>\n");
            }
            PageBlock::Thinking(rt) => {
                out.push_str("<blockquote class=\"thinking\">");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</blockquote>\n");
            }
            PageBlock::Preformatted { text, language } => {
                let lang = sanitize_language_token(language);
                out.push_str("<pre class=\"code\"><code");
                if !lang.is_empty() {
                    let _ = write!(out, " class=\"language-{}\"", escape_html(&lang));
                }
                out.push('>');
                self.render_rich_text(out, text, false, 0);
                out.push_str("</code></pre>\n");
            }
            PageBlock::Footer(rt) => {
                out.push_str("<footer>");
                self.render_rich_text(out, rt, true, 0);
                out.push_str("</footer>\n");
            }
            PageBlock::Divider => {
                out.push_str("<hr>\n");
            }
            PageBlock::Anchor { name } => {
                if let Some(norm) = self.record_anchor(name) {
                    let esc = escape_html(&norm);
                    let _ = writeln!(out, "<a id=\"{esc}\" name=\"{esc}\"></a>");
                }
            }
            PageBlock::List { items } => {
                out.push_str("<ul>\n");
                for item in items {
                    out.push_str("<li>");
                    render_task_checkbox(out, item.task_state);
                    match &item.content {
                        ListItemContent::Text(rt) => self.render_rich_text(out, rt, true, 0),
                        ListItemContent::Blocks(children) => {
                            self.render_blocks(out, children, depth + 1);
                        }
                    }
                    out.push_str("</li>\n");
                }
                out.push_str("</ul>\n");
            }
            PageBlock::OrderedList {
                items,
                reversed,
                start,
                list_type,
            } => {
                out.push_str("<ol");
                if *reversed {
                    out.push_str(" reversed");
                }
                if let Some(s) = start {
                    let _ = write!(out, " start=\"{s}\"");
                }
                if let Some(t) = list_type.as_deref().and_then(valid_html_ol_type) {
                    let _ = write!(out, " type=\"{t}\"");
                }
                out.push_str(">\n");
                for item in items {
                    out.push_str("<li");
                    if let Some(v) = item.value {
                        let _ = write!(out, " value=\"{v}\"");
                    }
                    if let Some(num) = &item.num
                        && !num.is_empty()
                    {
                        let _ = write!(out, " data-num=\"{}\"", escape_html(num));
                    }
                    out.push('>');
                    render_task_checkbox(out, item.task_state);
                    match &item.content {
                        ListItemContent::Text(rt) => self.render_rich_text(out, rt, true, 0),
                        ListItemContent::Blocks(children) => {
                            self.render_blocks(out, children, depth + 1);
                        }
                    }
                    out.push_str("</li>\n");
                }
                out.push_str("</ol>\n");
            }
            PageBlock::Blockquote {
                text,
                caption,
                collapsed,
            } => {
                if *collapsed {
                    out.push_str("<blockquote data-collapsed=\"true\">");
                } else {
                    out.push_str("<blockquote>");
                }
                self.render_rich_text(out, text, true, 0);
                if !caption.is_empty() {
                    out.push_str("<cite>");
                    self.render_rich_text(out, caption, true, 0);
                    out.push_str("</cite>");
                }
                out.push_str("</blockquote>\n");
            }
            PageBlock::BlockquoteBlocks { blocks, caption } => {
                out.push_str("<blockquote>");
                self.render_blocks(out, blocks, depth + 1);
                if !caption.is_empty() {
                    out.push_str("<cite>");
                    self.render_rich_text(out, caption, true, 0);
                    out.push_str("</cite>");
                }
                out.push_str("</blockquote>\n");
            }
            PageBlock::Pullquote { text, caption } => {
                out.push_str("<aside>");
                self.render_rich_text(out, text, true, 0);
                if !caption.is_empty() {
                    out.push_str("<cite>");
                    self.render_rich_text(out, caption, true, 0);
                    out.push_str("</cite>");
                }
                out.push_str("</aside>\n");
            }
            PageBlock::Photo {
                photo_id,
                caption,
                url,
                webpage_id,
                spoiler,
            } => {
                out.push_str("<figure class=\"media\">");
                let safe_link = url.as_deref().and_then(safe_url);
                if let Some(ref href) = safe_link {
                    let _ = write!(out, "<a href=\"{}\"", escape_html(href));
                    if let Some(wp) = webpage_id
                        && *wp != 0
                    {
                        let _ = write!(out, " data-webpage-id=\"{wp}\"");
                    }
                    out.push('>');
                }
                self.render_photo_img(out, *photo_id, *spoiler);
                if safe_link.is_some() {
                    out.push_str("</a>");
                }
                self.render_caption(out, caption);
                out.push_str("</figure>\n");
            }
            PageBlock::Video {
                video_id,
                caption,
                autoplay,
                loop_video,
                spoiler,
            } => {
                out.push_str("<figure class=\"media\">");
                self.render_video_tag(out, *video_id, *autoplay, *loop_video, *spoiler);
                self.render_caption(out, caption);
                out.push_str("</figure>\n");
            }
            PageBlock::Cover(inner) => {
                out.push_str("<div class=\"cover\">\n");
                self.render_block(out, inner, depth + 1);
                out.push_str("</div>\n");
            }
            PageBlock::Embed {
                url,
                html,
                poster_photo_id,
                width,
                height,
                full_width,
                allow_scrolling,
                caption,
            } => {
                self.render_embed(
                    out,
                    url.as_deref(),
                    html.as_deref(),
                    *poster_photo_id,
                    *width,
                    *height,
                    *full_width,
                    *allow_scrolling,
                    caption,
                );
            }
            PageBlock::EmbedPost {
                url,
                webpage_id,
                author_photo_id,
                author,
                date,
                blocks,
                caption,
            } => {
                out.push_str("<blockquote class=\"embed-post\">");
                if *author_photo_id != 0 {
                    self.record_photo(*author_photo_id);
                    let src = self.media_url("photo", *author_photo_id);
                    let _ = write!(
                        out,
                        "<img class=\"embed-post-photo\" src=\"{}\" data-tg-src=\"tg://photo?id={}\" data-iv-media=\"photo:{}\">",
                        escape_html(&src),
                        author_photo_id,
                        author_photo_id
                    );
                }
                let mut header_parts = Vec::new();
                if !author.is_empty() {
                    header_parts.push(escape_html(author));
                }
                if let Some(date_str) = format_utc_date(*date) {
                    header_parts.push(escape_html(&date_str));
                }
                if !header_parts.is_empty() {
                    let joined = header_parts.join(" · ");
                    out.push_str("<div class=\"embed-post-header\">");
                    if let Some(href) = safe_url(url) {
                        let _ = write!(out, "<a href=\"{}\"", escape_html(&href));
                        if *webpage_id != 0 {
                            let _ = write!(out, " data-webpage-id=\"{webpage_id}\"");
                        }
                        let _ = write!(out, ">{joined}</a>");
                    } else {
                        out.push_str(&joined);
                    }
                    out.push_str("</div>");
                }
                self.render_blocks(out, blocks, depth + 1);
                self.render_caption(out, caption);
                out.push_str("</blockquote>\n");
            }
            PageBlock::Collage { items, caption } => {
                self.render_collage(out, items, caption);
            }
            PageBlock::Slideshow { items, caption } => {
                self.render_slideshow(out, items, caption);
            }
            PageBlock::Channel(ch) => {
                let _ = write!(out, "<div class=\"channel\" data-channel-id=\"{}\">", ch.id);
                let link = ch
                    .username
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .map(|u| format!("https://t.me/{u}"));
                let username_attr = ch
                    .username
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .map(|u| format!(" data-channel-username=\"{}\"", escape_html(u)))
                    .unwrap_or_default();
                if let Some(href) = link.as_deref().and_then(safe_url) {
                    let _ = write!(
                        out,
                        "<a href=\"{}\" data-iv-channel-open=\"{}\"{}>{}</a>",
                        escape_html(&href),
                        ch.id,
                        username_attr,
                        escape_html(&ch.title)
                    );
                } else {
                    let _ = write!(
                        out,
                        "<span data-iv-channel-open=\"{}\"{}>{}</span>",
                        ch.id,
                        username_attr,
                        escape_html(&ch.title)
                    );
                }
                let (cls, label) = if ch.joined {
                    ("channel-join joined", "Joined")
                } else {
                    ("channel-join", "Join")
                };
                let _ = write!(
                    out,
                    "<button class=\"{cls}\" type=\"button\" data-iv-channel-join=\"{}\"{}>{label}</button>",
                    ch.id, username_attr
                );
                out.push_str("</div>\n");
            }
            PageBlock::Audio { audio_id, caption } => {
                self.record_document(*audio_id);
                let doc = self.doc_map.get(audio_id).copied();
                let src = self.media_url("audio", *audio_id);
                out.push_str("<figure class=\"audio\">");
                let _ = write!(
                    out,
                    "<audio controls src=\"{}\" data-tg-src=\"tg://audio?id={}\" data-iv-media=\"audio:{}\"></audio>",
                    escape_html(&src),
                    audio_id,
                    audio_id
                );
                if let Some(meta) = doc {
                    let mut parts = Vec::new();
                    if !meta.performer.is_empty() {
                        parts.push(escape_html(&meta.performer));
                    }
                    if !meta.title.is_empty() {
                        parts.push(escape_html(&meta.title));
                    }
                    if !parts.is_empty() {
                        let _ =
                            write!(out, "<div class=\"audio-meta\">{}</div>", parts.join(" — "));
                    }
                }
                self.render_caption(out, caption);
                out.push_str("</figure>\n");
            }
            PageBlock::Document {
                document_id,
                caption,
            } => {
                self.record_document(*document_id);
                let doc = self.doc_map.get(document_id).copied();
                let label = doc
                    .map(|d| d.file_name.as_str())
                    .filter(|n| !n.is_empty())
                    .unwrap_or("Document");
                let _ = write!(
                    out,
                    "<figure class=\"media document\"><div class=\"media-placeholder\" data-tg-src=\"tg://document?id={}\" data-iv-media=\"document:{}\">{}</div>",
                    document_id,
                    document_id,
                    escape_html(label)
                );
                self.render_caption(out, caption);
                out.push_str("</figure>\n");
            }
            PageBlock::Table {
                title,
                rows,
                bordered,
                striped,
                compact,
            } => {
                self.render_table(out, title, rows, *bordered, *striped, *compact);
            }
            PageBlock::Details {
                title,
                blocks,
                open,
            } => {
                if *open {
                    out.push_str("<details open><summary>");
                } else {
                    out.push_str("<details><summary>");
                }
                self.render_rich_text(out, title, true, 0);
                out.push_str("</summary>\n");
                self.render_blocks(out, blocks, depth + 1);
                out.push_str("</details>\n");
            }
            PageBlock::RelatedArticles { title, articles } => {
                out.push_str("<section class=\"related\">");
                if !title.is_empty() {
                    out.push_str("<div class=\"related-title\">");
                    self.render_rich_text(out, title, true, 0);
                    out.push_str("</div>");
                }
                for article in articles {
                    let href = safe_url(&article.url);
                    if let Some(ref safe) = href {
                        let _ = write!(
                            out,
                            "<a class=\"related-card\" href=\"{}\"",
                            escape_html(safe)
                        );
                        if article.webpage_id != 0 {
                            let _ = write!(out, " data-webpage-id=\"{}\"", article.webpage_id);
                        }
                        out.push('>');
                    } else {
                        out.push_str("<div class=\"related-card\">");
                    }
                    if let Some(pid) = article.photo_id
                        && pid != 0
                    {
                        self.record_photo(pid);
                        let src = self.media_url("photo", pid);
                        let _ = write!(
                            out,
                            "<img src=\"{}\" loading=\"lazy\" data-tg-src=\"tg://photo?id={}\" data-iv-media=\"photo:{}\">",
                            escape_html(&src),
                            pid,
                            pid
                        );
                    }
                    if let Some(ref t) = article.title
                        && !t.trim().is_empty()
                    {
                        let _ = write!(
                            out,
                            "<div class=\"related-name\">{}</div>",
                            escape_html(t.trim())
                        );
                    }
                    if let Some(ref desc) = article.description
                        && !desc.trim().is_empty()
                    {
                        let _ = write!(
                            out,
                            "<div class=\"related-desc\">{}</div>",
                            escape_html(desc.trim())
                        );
                    }
                    let mut meta = Vec::new();
                    if let Some(ref author) = article.author
                        && !author.trim().is_empty()
                    {
                        meta.push(escape_html(author.trim()));
                    }
                    if let Some(date) = article.published_date.and_then(format_utc_date) {
                        meta.push(escape_html(&date));
                    }
                    if !meta.is_empty() {
                        let _ = write!(
                            out,
                            "<div class=\"related-meta\">{}</div>",
                            meta.join(" · ")
                        );
                    }
                    if href.is_some() {
                        out.push_str("</a>\n");
                    } else {
                        out.push_str("</div>\n");
                    }
                }
                out.push_str("</section>\n");
            }
            PageBlock::Map {
                geo,
                zoom,
                width,
                height,
                caption,
            } => {
                out.push_str("<figure class=\"map\">");
                if *zoom > 0 && geo.is_valid() {
                    let w = if *width > 0 {
                        *width
                    } else {
                        DEFAULT_MAP_WIDTH
                    };
                    let h = if *height > 0 {
                        *height
                    } else {
                        DEFAULT_MAP_HEIGHT
                    };
                    let lat = format_coord(geo.lat);
                    let lon = format_coord(geo.lon);
                    let base = self.options.base_url.trim_end_matches('/');
                    let map_src = format!("{base}/media/map/{lat},{lon},{zoom},{w}x{h}");
                    let osm_url = format!(
                        "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map={zoom}/{lat}/{lon}"
                    );
                    let _ = write!(
                        out,
                        "<tg-map lat=\"{lat}\" long=\"{lon}\" zoom=\"{zoom}\"></tg-map>"
                    );
                    let _ = write!(
                        out,
                        "<a href=\"{}\"><img src=\"{}\" width=\"{w}\" height=\"{h}\" loading=\"lazy\" alt=\"{lat}, {lon}\"></a>",
                        escape_html(&osm_url),
                        escape_html(&map_src)
                    );
                }
                self.render_caption(out, caption);
                out.push_str("</figure>\n");
            }
            PageBlock::Math { source } => {
                let _ = writeln!(
                    out,
                    "<tg-math-block>{}</tg-math-block>",
                    escape_html(trim_math_source(source))
                );
            }
        }
    }

    fn render_photo_img(&mut self, out: &mut String, photo_id: i64, spoiler: bool) {
        self.record_photo(photo_id);
        let src = self.media_url("photo", photo_id);
        let dims = self
            .photo_map
            .get(&photo_id)
            .filter(|m| m.width > 0 && m.height > 0)
            .map(|m| format!(" width=\"{}\" height=\"{}\"", m.width, m.height))
            .unwrap_or_default();
        let hidden = if spoiler { " tg-spoiler" } else { "" };
        let _ = write!(
            out,
            "<img src=\"{}\" loading=\"lazy\"{dims} data-tg-src=\"tg://photo?id={}\" data-iv-media=\"photo:{}\"{hidden}>",
            escape_html(&src),
            photo_id,
            photo_id
        );
    }

    fn render_video_tag(
        &mut self,
        out: &mut String,
        video_id: i64,
        autoplay: bool,
        loop_video: bool,
        spoiler: bool,
    ) {
        self.record_document(video_id);
        let src = self.media_url("video", video_id);
        let dims = self
            .doc_map
            .get(&video_id)
            .filter(|m| m.width > 0 && m.height > 0)
            .map(|m| format!(" width=\"{}\" height=\"{}\"", m.width, m.height))
            .unwrap_or_default();
        let flags = if autoplay {
            " autoplay muted loop playsinline"
        } else if loop_video {
            " controls loop preload=\"metadata\""
        } else {
            " controls preload=\"metadata\""
        };
        let hidden = if spoiler { " tg-spoiler" } else { "" };
        let _ = write!(
            out,
            "<video src=\"{}\"{flags}{dims} data-tg-src=\"tg://video?id={}\" data-iv-media=\"video:{}\"{hidden}></video>",
            escape_html(&src),
            video_id,
            video_id
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_embed(
        &mut self,
        out: &mut String,
        url: Option<&str>,
        html: Option<&str>,
        poster_photo_id: Option<i64>,
        width: Option<i32>,
        height: Option<i32>,
        full_width: bool,
        allow_scrolling: bool,
        caption: &PageCaption,
    ) {
        let embed_index = self.embeds.len();
        let safe = url.and_then(safe_url);
        self.embeds.push(PreparedEmbed {
            index: embed_index,
            url: safe.clone(),
            html: html.map(ToString::to_string),
            width,
            height,
            full_width,
            allow_scrolling,
        });

        out.push_str("<figure class=\"embed\">");
        if let Some(pid) = poster_photo_id
            && pid != 0
        {
            self.record_photo(pid);
        }

        let mut rendered_iframe = false;
        match &self.options.embed_policy {
            EmbedPolicy::Blocked => {
                out.push_str(
                    "<div class=\"media-missing\">Embedded content blocked by policy</div>",
                );
            }
            EmbedPolicy::LinkOnly => {
                if let Some(ref href) = safe {
                    let _ = write!(
                        out,
                        "<a href=\"{}\">{}</a>",
                        escape_html(href),
                        escape_html(href)
                    );
                } else {
                    out.push_str("<div class=\"media-missing\">Embedded content</div>");
                }
            }
            EmbedPolicy::SandboxedAllowlist {
                allowed_hosts,
                allow_sandboxed_html,
            } => {
                let scrolling = if allow_scrolling { "yes" } else { "no" };
                let class_attr = if full_width {
                    "embed-frame full-width"
                } else {
                    "embed-frame"
                };
                let w_attr = width
                    .filter(|&w| w > 0)
                    .map(|w| format!(" width=\"{w}\""))
                    .unwrap_or_default();
                let h_attr = height
                    .filter(|&h| h > 0)
                    .map(|h| format!(" height=\"{h}\""))
                    .unwrap_or_else(|| " height=\"360\"".to_string());

                if let Some(ref href) = safe
                    && is_allowed_embed_url(href, allowed_hosts)
                {
                    let _ = write!(
                        out,
                        "<iframe class=\"{class_attr}\" src=\"{}\"{w_attr}{h_attr} sandbox=\"allow-scripts allow-same-origin allow-popups allow-presentation\" referrerpolicy=\"no-referrer\" loading=\"lazy\" scrolling=\"{scrolling}\"></iframe>",
                        escape_html(href)
                    );
                    rendered_iframe = true;
                } else if *allow_sandboxed_html
                    && let Some(raw_html) = html
                    && !raw_html.trim().is_empty()
                {
                    let _ = write!(
                        out,
                        "<iframe class=\"{class_attr}\" srcdoc=\"{}\"{w_attr}{h_attr} sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" loading=\"lazy\" scrolling=\"{scrolling}\"></iframe>",
                        escape_html(raw_html)
                    );
                    rendered_iframe = true;
                }

                if !rendered_iframe {
                    if let Some(ref href) = safe {
                        let _ = write!(
                            out,
                            "<a href=\"{}\">{}</a>",
                            escape_html(href),
                            escape_html(href)
                        );
                    } else {
                        out.push_str("<div class=\"media-missing\">Embedded content</div>");
                    }
                }
            }
        }

        self.render_caption(out, caption);
        out.push_str("</figure>\n");
    }

    fn render_collage(&mut self, out: &mut String, items: &[PageBlock], caption: &PageCaption) {
        let media_items = self.extract_grouped_media(items);
        if media_items.is_empty() {
            return;
        }
        let limited = &media_items[..media_items.len().min(COLLAGE_MAX_ITEMS)];
        let sizes: Vec<MediaSize> = limited
            .iter()
            .map(|m| MediaSize::new(m.width.max(1), m.height.max(1)))
            .collect();
        let layout = layout_media_group(
            &sizes,
            DEFAULT_GROUP_WIDTH_MAX,
            DEFAULT_GROUP_WIDTH_MIN,
            DEFAULT_GROUP_SPACING,
        );
        let mut full = MediaRect::default();
        for part in &layout {
            full = full.united(part.geometry);
        }
        let count = layout.len().min(limited.len());
        if count == 0 || full.is_empty() {
            return;
        }
        out.push_str("<tg-collage>");
        let _ = write!(
            out,
            "<div class=\"collage-box\" style=\"aspect-ratio:{}/{}\">",
            full.width, full.height
        );
        let pct =
            |val: i32, total: i32| format!("{:.3}", f64::from(val) * 100.0 / f64::from(total));
        for (item, part) in limited.iter().zip(layout.iter()).take(count) {
            let g = part.geometry;
            let _ = write!(
                out,
                "<div class=\"collage-item\" style=\"left:{}%;top:{}%;width:{}%;height:{}%\">",
                pct(g.x - full.x, full.width),
                pct(g.y - full.y, full.height),
                pct(g.width, full.width),
                pct(g.height, full.height)
            );
            self.render_grouped_media_item(out, item);
            out.push_str("</div>");
        }
        out.push_str("</div>");
        self.render_caption(out, caption);
        out.push_str("</tg-collage>\n");
    }

    fn render_slideshow(&mut self, out: &mut String, items: &[PageBlock], caption: &PageCaption) {
        let media_items = self.extract_grouped_media(items);
        if media_items.is_empty() {
            return;
        }
        let mut best = MediaSize::new(1, 1);
        let mut best_ratio = 0.0_f64;
        for item in &media_items {
            let w = item.width.max(1);
            let h = item.height.max(1);
            let ratio = f64::from(w) / f64::from(h);
            if ratio > best_ratio {
                best_ratio = ratio;
                best = MediaSize::new(w, h);
            }
        }
        let controls = media_items.len() > 1;
        if controls {
            self.has_slideshow = true;
        }
        out.push_str("<tg-slideshow>");
        let _ = write!(
            out,
            "<div class=\"slideshow-box\" style=\"aspect-ratio:{}/{}\"><div class=\"slideshow-track\">",
            best.width, best.height
        );
        for item in &media_items {
            out.push_str("<div class=\"slide\">");
            self.render_grouped_media_item(out, item);
            out.push_str("</div>");
        }
        out.push_str("</div>");
        if controls {
            out.push_str("<button class=\"slideshow-prev\" type=\"button\">&#8592;</button>");
            out.push_str("<button class=\"slideshow-next\" type=\"button\">&#8594;</button>");
            out.push_str("<div class=\"slideshow-dots\">");
            for _ in 0..media_items.len() {
                out.push_str("<button class=\"slideshow-dot\" type=\"button\"></button>");
            }
            out.push_str("</div>");
        }
        out.push_str("</div>");
        self.render_caption(out, caption);
        out.push_str("</tg-slideshow>\n");
    }

    fn extract_grouped_media(&self, items: &[PageBlock]) -> Vec<GroupedItem> {
        let mut out = Vec::new();
        for item in items {
            match item {
                PageBlock::Photo {
                    photo_id, spoiler, ..
                } => {
                    let (w, h) = self
                        .photo_map
                        .get(photo_id)
                        .map(|m| (m.width, m.height))
                        .unwrap_or((4, 3));
                    out.push(GroupedItem {
                        is_photo: true,
                        id: *photo_id,
                        width: w,
                        height: h,
                        autoplay: false,
                        loop_video: false,
                        spoiler: *spoiler,
                    });
                }
                PageBlock::Video {
                    video_id,
                    autoplay,
                    loop_video,
                    spoiler,
                    ..
                } => {
                    let (w, h) = self
                        .doc_map
                        .get(video_id)
                        .map(|m| (m.width, m.height))
                        .unwrap_or((16, 9));
                    out.push(GroupedItem {
                        is_photo: false,
                        id: *video_id,
                        width: w,
                        height: h,
                        autoplay: *autoplay,
                        loop_video: *loop_video,
                        spoiler: *spoiler,
                    });
                }
                _ => {}
            }
        }
        out
    }

    fn render_grouped_media_item(&mut self, out: &mut String, item: &GroupedItem) {
        if item.is_photo {
            self.render_photo_img(out, item.id, item.spoiler);
        } else {
            self.render_video_tag(out, item.id, item.autoplay, item.loop_video, item.spoiler);
        }
    }

    fn render_table(
        &mut self,
        out: &mut String,
        title: &RichText,
        rows: &[PageTableRow],
        bordered: bool,
        striped: bool,
        compact: bool,
    ) {
        out.push_str("<figure class=\"table-wrap\">");
        if !title.is_empty() {
            out.push_str("<div class=\"table-title\">");
            self.render_rich_text(out, title, true, 0);
            out.push_str("</div>");
        }
        let mut classes = Vec::new();
        if bordered {
            classes.push("bordered");
        }
        if striped {
            classes.push("striped");
        }
        if compact {
            classes.push("compact");
        }
        if classes.is_empty() {
            out.push_str("<table>\n");
        } else {
            let _ = writeln!(out, "<table class=\"{}\">", classes.join(" "));
        }
        for row in rows {
            out.push_str("<tr>");
            for cell in &row.cells {
                let tag = if cell.header { "th" } else { "td" };
                let _ = write!(out, "<{tag}");
                if cell.colspan > 1 {
                    let _ = write!(out, " colspan=\"{}\"", cell.colspan);
                }
                if cell.rowspan > 1 {
                    let _ = write!(out, " rowspan=\"{}\"", cell.rowspan);
                }
                let mut style = String::new();
                match cell.align {
                    TableAlign::Left => {}
                    TableAlign::Center => style.push_str("text-align:center;"),
                    TableAlign::Right => style.push_str("text-align:right;"),
                }
                match cell.valign {
                    TableValign::Top => {}
                    TableValign::Middle => style.push_str("vertical-align:middle;"),
                    TableValign::Bottom => style.push_str("vertical-align:bottom;"),
                }
                if !style.is_empty() {
                    let _ = write!(out, " style=\"{style}\"");
                }
                out.push('>');
                if let Some(ref rt) = cell.text {
                    self.render_rich_text(out, rt, true, 0);
                }
                let _ = write!(out, "</{tag}>");
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</table></figure>\n");
    }

    fn render_caption(&mut self, out: &mut String, caption: &PageCaption) {
        if caption.is_empty() {
            return;
        }
        out.push_str("<figcaption>");
        if !caption.text.is_empty() {
            self.render_rich_text(out, &caption.text, true, 0);
        }
        if !caption.credit.is_empty() {
            out.push_str("<cite class=\"credit\">");
            self.render_rich_text(out, &caption.credit, true, 0);
            out.push_str("</cite>");
        }
        out.push_str("</figcaption>");
    }

    fn render_rich_text(
        &mut self,
        out: &mut String,
        rt: &RichText,
        line_breaks: bool,
        depth: usize,
    ) {
        if depth >= MAX_NESTING_DEPTH {
            return;
        }
        match rt {
            RichText::Empty => {}
            RichText::Plain(s) => append_escaped(out, s, line_breaks),
            RichText::Bold(inner) => {
                out.push_str("<strong>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</strong>");
            }
            RichText::Italic(inner) => {
                out.push_str("<em>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</em>");
            }
            RichText::Underline(inner) => {
                out.push_str("<u>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</u>");
            }
            RichText::Strike(inner) => {
                out.push_str("<s>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</s>");
            }
            RichText::Fixed(inner) => {
                out.push_str("<code>");
                self.render_rich_text(out, inner, false, depth + 1);
                out.push_str("</code>");
            }
            RichText::Subscript(inner) => {
                out.push_str("<sub>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</sub>");
            }
            RichText::Superscript(inner) => {
                out.push_str("<sup>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</sup>");
            }
            RichText::Marked(inner) => {
                out.push_str("<mark>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</mark>");
            }
            RichText::Spoiler(inner) => {
                out.push_str("<tg-spoiler>");
                self.render_rich_text(out, inner, line_breaks, depth + 1);
                out.push_str("</tg-spoiler>");
            }
            RichText::Url {
                text,
                url,
                webpage_id,
            } => {
                if let Some(href) = safe_url(url) {
                    let _ = write!(out, "<a href=\"{}\"", escape_html(&href));
                    if *webpage_id != 0 {
                        let _ = write!(out, " data-webpage-id=\"{webpage_id}\"");
                    }
                    out.push('>');
                    if text.is_empty() {
                        append_escaped(out, &href, false);
                    } else {
                        self.render_rich_text(out, text, line_breaks, depth + 1);
                    }
                    out.push_str("</a>");
                } else if !text.is_empty() {
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                } else {
                    append_escaped(out, url, false);
                }
            }
            RichText::Email { text, email } => {
                let mailto = format!("mailto:{}", email.trim());
                if let Some(href) = safe_url(&mailto) {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    if text.is_empty() {
                        append_escaped(out, email.trim(), false);
                    } else {
                        self.render_rich_text(out, text, line_breaks, depth + 1);
                    }
                    out.push_str("</a>");
                } else if !text.is_empty() {
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                } else {
                    append_escaped(out, email, false);
                }
            }
            RichText::Phone { text, phone } => {
                let tel = format!("tel:{}", phone.trim());
                if let Some(href) = safe_url(&tel) {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    if text.is_empty() {
                        append_escaped(out, phone.trim(), false);
                    } else {
                        self.render_rich_text(out, text, line_breaks, depth + 1);
                    }
                    out.push_str("</a>");
                } else if !text.is_empty() {
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                } else {
                    append_escaped(out, phone, false);
                }
            }
            RichText::Image {
                document_id,
                width,
                height,
            } => {
                if *document_id != 0 {
                    self.record_document(*document_id);
                    let src = self.media_url("doc", *document_id);
                    let dims = if *width > 0 && *height > 0 {
                        format!(" width=\"{width}\" height=\"{height}\"")
                    } else {
                        String::new()
                    };
                    let _ = write!(
                        out,
                        "<img class=\"inline-image\" src=\"{}\"{dims} alt=\"[image]\" data-tg-src=\"tg://document?id={}\" data-iv-media=\"document:{}\">",
                        escape_html(&src),
                        document_id,
                        document_id
                    );
                } else {
                    out.push_str("[image]");
                }
            }
            RichText::Anchor { text, name } => {
                if let Some(norm) = self.record_anchor(name) {
                    let esc = escape_html(&norm);
                    let _ = write!(out, "<a id=\"{esc}\" name=\"{esc}\"></a>");
                }
                self.render_rich_text(out, text, line_breaks, depth + 1);
            }
            RichText::Concat(parts) => {
                for part in parts {
                    self.render_rich_text(out, part, line_breaks, depth + 1);
                }
            }
            RichText::Math { source } => {
                let _ = write!(
                    out,
                    "<tg-math>{}</tg-math>",
                    escape_html(trim_math_source(source))
                );
            }
            RichText::CustomEmoji { document_id, alt } => {
                if *document_id != 0 {
                    self.record_document(*document_id);
                    let _ = write!(
                        out,
                        "<tg-emoji emoji-id=\"{}\">{}</tg-emoji>",
                        document_id,
                        escape_html(alt)
                    );
                } else {
                    append_escaped(out, alt, false);
                }
            }
            RichText::Mention(inner) => {
                let plain = inner.to_plain_text();
                let username = plain.trim().trim_start_matches('@');
                let target = format!("https://t.me/{username}");
                if !username.is_empty()
                    && let Some(href) = safe_url(&target)
                {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                    out.push_str("</a>");
                } else {
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                }
            }
            RichText::Hashtag(inner)
            | RichText::BotCommand(inner)
            | RichText::Cashtag(inner)
            | RichText::BankCard(inner)
            | RichText::TonAddress(inner) => {
                self.render_rich_text(out, inner, line_breaks, depth + 1);
            }
            RichText::AutoUrl(inner) => {
                let plain = inner.to_plain_text();
                if let Some(href) = safe_url(&plain) {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                    out.push_str("</a>");
                } else {
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                }
            }
            RichText::AutoEmail(inner) => {
                let plain = inner.to_plain_text();
                let target = format!("mailto:{}", plain.trim());
                if let Some(href) = safe_url(&target) {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                    out.push_str("</a>");
                } else {
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                }
            }
            RichText::AutoPhone(inner) => {
                let plain = inner.to_plain_text();
                let target = format!("tel:{}", plain.trim());
                if let Some(href) = safe_url(&target) {
                    let _ = write!(out, "<a href=\"{}\">", escape_html(&href));
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                    out.push_str("</a>");
                } else {
                    self.render_rich_text(out, inner, line_breaks, depth + 1);
                }
            }
            RichText::MentionName { text, user_id } => {
                if *user_id != 0 {
                    let _ = write!(out, "<a href=\"tg://user?id={user_id}\">");
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                    out.push_str("</a>");
                } else {
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                }
            }
            RichText::Date { text, date, flags } => {
                let _ = write!(
                    out,
                    "<tg-time unix=\"{}\" format=\"{}\">",
                    date,
                    escape_html(&flags.format_code())
                );
                self.render_rich_text(out, text, line_breaks, depth + 1);
                out.push_str("</tg-time>");
            }
            RichText::Diff { text, old_text } => {
                if self.options.display_text_diff {
                    if !old_text.is_empty() {
                        out.push_str("<s class=\"diff-del\">");
                        self.render_rich_text(out, old_text, line_breaks, depth + 1);
                        out.push_str("</s>");
                    }
                    if !text.is_empty() {
                        out.push_str("<u class=\"diff-ins\">");
                        self.render_rich_text(out, text, line_breaks, depth + 1);
                        out.push_str("</u>");
                    }
                } else {
                    self.render_rich_text(out, text, line_breaks, depth + 1);
                }
            }
        }
    }
}

struct GroupedItem {
    is_photo: bool,
    id: i64,
    width: i32,
    height: i32,
    autoplay: bool,
    loop_video: bool,
    spoiler: bool,
}

fn render_task_checkbox(out: &mut String, state: TaskState) {
    match state {
        TaskState::None => {}
        TaskState::Unchecked => out.push_str("<input type=\"checkbox\" disabled> "),
        TaskState::Checked => out.push_str("<input type=\"checkbox\" disabled checked> "),
    }
}

fn valid_html_ol_type(value: &str) -> Option<&'static str> {
    match value {
        "1" => Some("1"),
        "a" => Some("a"),
        "A" => Some("A"),
        "i" => Some("i"),
        "I" => Some("I"),
        _ => None,
    }
}

fn sanitize_language_token(language: &str) -> String {
    language
        .trim()
        .chars()
        .take(32)
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '#'))
        .collect()
}

fn format_coord(value: f64) -> String {
    let s = format!("{value:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}
