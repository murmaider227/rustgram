//! Port of `Telegram/SourceFiles/iv/iv_rich_message_html_export.h` and
//! `iv_rich_message_html_export.cpp`.
//!
//! Exports [`RichPage`] articles into self-contained, XSS-sanitized HTML5 documents
//! (both in-memory and to a local directory with extracted `media/` files), as well as
//! clipboard HTML with inline `data:` URIs.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::assets::{RICH_MESSAGE_CSS, RICH_MESSAGE_JS};
use crate::grouped_layout::{
    DEFAULT_GROUP_SPACING, DEFAULT_GROUP_WIDTH_MAX, DEFAULT_GROUP_WIDTH_MIN, MediaRect, MediaSize,
    layout_media_group,
};
use crate::model::{DateFlags, normalize_fragment_id};
use crate::prepare::{append_escaped, escape_html, format_utc_date};
use crate::rich_page::{
    EntityInText, EntityType, GroupedMediaIntent, GroupedMediaItem, InlineTextObjectEntity,
    RICH_EXPORT_GENERATOR_MARKER, RichBlock, RichBlockKind, RichButtonType, RichListKind, RichPage,
    RichTableAlignment, RichTableVerticalAlignment, RichTaskState, RichTextNode, TextWithEntities,
    decode_rich_page_link_url, parse_inline_text_object_entity, utf16_to_byte_offset,
};
use crate::serve::MediaProvider;

/// Maximum folder / file name stem length matching `kMaxFolderNameLength`.
pub const MAX_FOLDER_NAME_LENGTH: usize = 48;

/// Maximum total media bytes embedded as base64 `data:` URIs in clipboard HTML
/// (`kClipboardMediaLimit`, 4 MiB).
pub const CLIPBOARD_MEDIA_LIMIT: usize = 4 * 1024 * 1024;

/// Maximum total formula PNG bytes embedded in clipboard HTML (`kClipboardFormulaLimit`, 2 MiB).
pub const CLIPBOARD_FORMULA_LIMIT: usize = 2 * 1024 * 1024;

/// Configuration for [`export_rich_page_html`] and [`export_rich_page_to_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlExportOptions {
    /// Explicit HTML `<title>`. When `None`, derived via [`title_from_page`].
    pub title: Option<String>,
    /// Map of `photo_id` -> relative file path (`media/photo_1.jpg`) or `data:` URI.
    pub photo_paths: HashMap<u64, String>,
    /// Map of `document_id` -> relative file path or `data:` URI.
    pub document_paths: HashMap<u64, String>,
    /// Map of `document_id` -> video poster image path or `data:` URI.
    pub document_posters: HashMap<u64, String>,
    /// Embed `RICH_MESSAGE_CSS` inside `<style>` (default `true`).
    pub inline_styles: bool,
    /// Embed `RICH_MESSAGE_JS` when an interactive slideshow with >1 slide is present
    /// and `clipboard == false` (default `true`).
    pub include_slideshow_script: bool,
    /// Render in clipboard mode (omits `<div class="media-missing">` placeholders and scripts).
    pub clipboard: bool,
    /// Fallback label rendered for unavailable media when `clipboard == false`.
    pub missing_media_label: String,
    /// Fallback label rendered for embeds without a safe URL.
    pub embed_fallback_label: String,
}

impl Default for HtmlExportOptions {
    fn default() -> Self {
        Self {
            title: None,
            photo_paths: HashMap::new(),
            document_paths: HashMap::new(),
            document_posters: HashMap::new(),
            inline_styles: true,
            include_slideshow_script: true,
            clipboard: false,
            missing_media_label: "Media not included".to_string(),
            embed_fallback_label: "Embedded content".to_string(),
        }
    }
}

/// Output of [`export_rich_page_html`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlExportResult {
    /// Complete sanitized HTML5 document.
    pub html: String,
    /// Resolved article title used in `<title>`.
    pub title: String,
    /// True if the article contained an interactive slideshow with multiple slides.
    pub slideshow_used: bool,
    /// Unique photo IDs encountered in the article.
    pub referenced_photo_ids: Vec<u64>,
    /// Unique document IDs encountered in the article.
    pub referenced_document_ids: Vec<u64>,
    /// Number of unsafe URLs (e.g. `javascript:`, `vbscript:`, `data:`) stripped during export.
    pub stripped_urls: usize,
}

/// Output of [`export_rich_page_to_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskExportResult {
    /// Created export directory path.
    pub folder: PathBuf,
    /// Written `.html` file path inside `folder`.
    pub html_path: PathBuf,
    /// Written media files inside `folder/media/`.
    pub media_files: Vec<PathBuf>,
    /// HTML export metadata and document text.
    pub export: HtmlExportResult,
}

/// Validates and normalizes a URL for safe insertion into exported HTML `href` attributes
/// (`SafeUrl` in `iv_rich_message_html_export.cpp`).
///
/// Permits `#fragment` (normalized), `http://`, `https://`, `tg://`, `mailto:`, `tel:`, and
/// `ftp://`. Rejects `javascript:`, `vbscript:`, `data:`, `file:`, control characters, and
/// unknown schemes.
#[must_use]
pub fn safe_export_url(url: &str) -> Option<String> {
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
    const ALLOWED: &[&str] = &["http://", "https://", "tg://", "mailto:", "tel:", "ftp://"];
    if ALLOWED.iter().any(|prefix| lower.starts_with(prefix)) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Derives a title from the first non-empty heading or paragraph of `page` (`TitleFromPage`).
#[must_use]
pub fn title_from_page(page: &RichPage) -> String {
    for block in &page.blocks {
        if block.kind == RichBlockKind::Heading {
            let t = block.text.text.text.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    for block in &page.blocks {
        if block.kind == RichBlockKind::Paragraph {
            let t = block.text.text.text.trim();
            if !t.is_empty() {
                let clipped: String = t.chars().take(MAX_FOLDER_NAME_LENGTH).collect();
                return clipped.trim().to_string();
            }
        }
    }
    String::new()
}

/// Sanitizes a file or folder name component (`SanitizeFileNamePart`), stripping OS-forbidden
/// path separators and control characters, clamping to [`MAX_FOLDER_NAME_LENGTH`], and
/// trimming trailing dots/spaces (Windows filename rules).
#[must_use]
pub fn sanitize_file_name_part(value: &str, keep_extension: &str) -> String {
    let simplified: String = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let mut result = simplified.trim().to_string();
    let tail = if keep_extension.is_empty() {
        String::new()
    } else {
        keep_extension
            .chars()
            .filter(|ch| {
                !ch.is_control()
                    && !matches!(*ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
            .collect()
    };

    let char_count = result.chars().count();
    let tail_chars = tail.chars().count();
    if char_count > MAX_FOLDER_NAME_LENGTH {
        let keep = MAX_FOLDER_NAME_LENGTH.saturating_sub(tail_chars);
        let prefix: String = result.chars().take(keep).collect();
        result = format!("{prefix}{tail}");
    }
    while result.ends_with('.') || result.ends_with(' ') {
        result.pop();
    }
    result
}

/// Standard RFC 4648 base64 encoder used for inline `data:` URIs.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Sanitizes an arbitrary raw HTML fragment by stripping `<script>`, `<style>`, `<iframe>`,
/// `<object>`, `<embed>`, `<form>`, inline `on*` event handler attributes, and unsafe
/// `javascript:` / `vbscript:` / `data:` URLs.
#[must_use]
pub fn sanitize_raw_html_fragment(html: &str) -> String {
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0usize;

    while i < len {
        if bytes[i] != b'<' {
            let ch = html[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if html[i..].starts_with("<!--") {
            if let Some(end) = html[i + 4..].find("-->") {
                i = i + 4 + end + 3;
            } else {
                break;
            }
            continue;
        }
        let Some(gt_rel) = html[i + 1..].find('>') else {
            break;
        };
        let tag_inner = &html[i + 1..i + 1 + gt_rel];
        let trimmed = tag_inner.trim();
        let is_close = trimmed.starts_with('/');
        let name_slice = if is_close {
            trimmed[1..].trim_start()
        } else {
            trimmed
        };
        let name_end = name_slice
            .bytes()
            .position(|b| b.is_ascii_whitespace() || b == b'/')
            .unwrap_or(name_slice.len());
        let tag_name = name_slice[..name_end].to_ascii_lowercase();

        if matches!(
            tag_name.as_str(),
            "script" | "style" | "iframe" | "object" | "embed" | "form" | "noscript"
        ) {
            i = i + 1 + gt_rel + 1;
            if !is_close && !trimmed.ends_with('/') && tag_name != "embed" {
                let close_pat = format!("</{tag_name}");
                let rest_lower = html[i..].to_ascii_lowercase();
                if let Some(rel) = rest_lower.find(&close_pat) {
                    let after = i + rel + close_pat.len();
                    if let Some(cgt) = html[after..].find('>') {
                        i = after + cgt + 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            continue;
        }

        if is_close {
            if !tag_name.is_empty()
                && tag_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                let _ = write!(out, "</{tag_name}>");
            }
            i = i + 1 + gt_rel + 1;
            continue;
        }

        if tag_name.is_empty()
            || !tag_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            i = i + 1 + gt_rel + 1;
            continue;
        }

        // Rebuild tag keeping only safe attributes.
        let rest_attrs = &name_slice[name_end..];
        let self_close = trimmed.ends_with('/');
        let safe_attrs = filter_safe_tag_attributes(rest_attrs);
        out.push('<');
        out.push_str(&tag_name);
        out.push_str(&safe_attrs);
        if self_close {
            out.push_str(" /");
        }
        out.push('>');
        i = i + 1 + gt_rel + 1;
    }

    out
}

fn filter_safe_tag_attributes(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    let mut i = 0usize;
    while i < len {
        while i < len && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= len {
            break;
        }
        let name_start = i;
        while i < len && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'/') {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let attr_name = raw[name_start..i].to_ascii_lowercase();
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut attr_val: Option<String> = None;
        if i < len && bytes[i] == b'=' {
            i += 1;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < len && matches!(bytes[i], b'"' | b'\'') {
                let q = bytes[i];
                i += 1;
                let vs = i;
                while i < len && bytes[i] != q {
                    i += 1;
                }
                attr_val = Some(raw[vs..i].to_string());
                if i < len {
                    i += 1;
                }
            } else {
                let vs = i;
                while i < len && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
                    i += 1;
                }
                attr_val = Some(raw[vs..i].to_string());
            }
        }
        if attr_name.starts_with("on") || attr_name == "srcdoc" {
            continue;
        }
        if matches!(attr_name.as_str(), "href" | "src" | "action" | "formaction") {
            let Some(val) = attr_val else {
                continue;
            };
            let Some(safe) = safe_export_url(&val) else {
                continue;
            };
            let _ = write!(out, " {attr_name}=\"{}\"", escape_html(&safe));
            continue;
        }
        if let Some(val) = attr_val {
            let lower = val.to_ascii_lowercase();
            if lower.contains("javascript:")
                || lower.contains("vbscript:")
                || lower.contains("expression(")
            {
                continue;
            }
            let _ = write!(out, " {attr_name}=\"{}\"", escape_html(&val));
        } else {
            let _ = write!(out, " {attr_name}");
        }
    }
    out
}

/// Exports a [`RichPage`] into a self-contained sanitized HTML5 document (`GenerateHtml`).
#[must_use]
pub fn export_rich_page_html(page: &RichPage, options: &HtmlExportOptions) -> HtmlExportResult {
    let mut state = ExportState {
        options,
        slideshow_used: false,
        photo_ids: Vec::new(),
        photo_set: HashSet::new(),
        document_ids: Vec::new(),
        document_set: HashSet::new(),
        stripped_urls: 0,
    };

    let mut body = String::new();
    serialize_blocks(&mut body, &page.blocks, &mut state);

    let title = options
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            let derived = title_from_page(page);
            if derived.is_empty() {
                "Article".to_string()
            } else {
                derived
            }
        });

    let mut html = String::with_capacity(body.len() + 1024);
    html.push_str("<!DOCTYPE html>\n<html");
    if page.rtl {
        html.push_str(" dir=\"rtl\"");
    }
    html.push_str(">\n<head>\n<meta charset=\"utf-8\">\n");
    let _ = writeln!(
        html,
        "<meta name=\"generator\" content=\"{}\">",
        escape_html(RICH_EXPORT_GENERATOR_MARKER)
    );
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(html, "<title>{}</title>", escape_html(&title));
    if options.inline_styles {
        html.push_str("<style>\n");
        html.push_str(RICH_MESSAGE_CSS);
        if !RICH_MESSAGE_CSS.ends_with('\n') {
            html.push('\n');
        }
        html.push_str("</style>\n");
    }
    html.push_str("</head>\n<body>\n<article>\n");
    html.push_str(&body);
    html.push_str("</article>\n");
    if state.slideshow_used && options.include_slideshow_script && !options.clipboard {
        html.push_str("<script>\n");
        html.push_str(RICH_MESSAGE_JS);
        if !RICH_MESSAGE_JS.ends_with('\n') {
            html.push('\n');
        }
        html.push_str("</script>\n");
    }
    html.push_str("</body>\n</html>\n");

    HtmlExportResult {
        html,
        title,
        slideshow_used: state.slideshow_used,
        referenced_photo_ids: state.photo_ids,
        referenced_document_ids: state.document_ids,
        stripped_urls: state.stripped_urls,
    }
}

/// Builds clipboard-ready HTML for a slice of [`RichBlock`]s, embedding cached media blobs as
/// inline base64 `data:` URIs up to [`CLIPBOARD_MEDIA_LIMIT`] (`RichBlocksClipboardHtml`).
#[must_use]
pub fn rich_blocks_clipboard_html<M: MediaProvider>(
    blocks: &[RichBlock],
    rtl: bool,
    media: Option<&M>,
) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    let page = RichPage {
        rtl,
        blocks: blocks.to_vec(),
        ..RichPage::default()
    };
    let mut opts = HtmlExportOptions {
        clipboard: true,
        include_slideshow_script: false,
        ..HtmlExportOptions::default()
    };

    if let Some(provider) = media {
        let mut budget = CLIPBOARD_MEDIA_LIMIT;
        let (photos, docs) = collect_media_ids(&page.blocks);
        for photo_id in photos {
            if let Ok(id_i64) = i64::try_from(photo_id)
                && let Some(blob) = provider.photo(id_i64)
                && !blob.bytes.is_empty()
                && blob.bytes.len() <= budget
            {
                budget -= blob.bytes.len();
                let uri = format!(
                    "data:{};base64,{}",
                    blob.content_type,
                    encode_base64(&blob.bytes)
                );
                opts.photo_paths.insert(photo_id, uri);
            }
        }
        for doc_id in docs {
            if let Ok(id_i64) = i64::try_from(doc_id)
                && let Some(blob) = provider.document(id_i64)
                && embeddable_mime(&blob.content_type)
                && !blob.bytes.is_empty()
                && blob.bytes.len() <= budget
            {
                budget -= blob.bytes.len();
                let uri = format!(
                    "data:{};base64,{}",
                    blob.content_type,
                    encode_base64(&blob.bytes)
                );
                opts.document_paths.insert(doc_id, uri);
            }
        }
    }

    export_rich_page_html(&page, &opts).html
}

/// Exports a [`RichPage`] to a newly created subfolder under `base_dir`, writing any available
/// media blobs from `media` into `media/` and writing `<title>.html` (`RichMessageHtmlExport`).
pub fn export_rich_page_to_dir<M: MediaProvider>(
    page: &RichPage,
    base_dir: &Path,
    media: Option<&M>,
    base_options: &HtmlExportOptions,
) -> io::Result<DiskExportResult> {
    fs::create_dir_all(base_dir)?;

    let raw_title = base_options
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| title_from_page(page));
    let mut stem = sanitize_file_name_part(&raw_title, "");
    if stem.is_empty() {
        stem = "message".to_string();
    }

    let mut chosen: Option<(PathBuf, String)> = None;
    for i in 0..100usize {
        let candidate = if i == 0 {
            stem.clone()
        } else {
            format!("{stem} ({})", i + 1)
        };
        let folder = base_dir.join(&candidate);
        if !folder.exists() {
            fs::create_dir_all(&folder)?;
            chosen = Some((folder, candidate));
            break;
        }
    }
    let (folder, candidate_name) = chosen.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to allocate unique export folder",
        )
    })?;

    let mut options = base_options.clone();
    let mut media_files = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    if let Some(provider) = media {
        let (photos, docs) = collect_media_ids(&page.blocks);
        let media_dir = folder.join("media");
        let mut created_media_dir = false;

        for photo_id in photos {
            if options.photo_paths.contains_key(&photo_id) {
                continue;
            }
            if let Ok(id_i64) = i64::try_from(photo_id)
                && let Some(blob) = provider.photo(id_i64)
            {
                if !created_media_dir {
                    fs::create_dir_all(&media_dir)?;
                    created_media_dir = true;
                }
                let ext = extension_for_mime(&blob.content_type, ".jpg");
                let rel = reserve_media_name(
                    &mut used_names,
                    photo_id,
                    &format!("photo_{photo_id}{ext}"),
                );
                let full_path = folder.join(&rel);
                fs::write(&full_path, &blob.bytes)?;
                options.photo_paths.insert(photo_id, rel);
                media_files.push(full_path);
            }
        }

        for doc_id in docs {
            if options.document_paths.contains_key(&doc_id) {
                continue;
            }
            if let Ok(id_i64) = i64::try_from(doc_id)
                && let Some(blob) = provider.document(id_i64)
            {
                if !created_media_dir {
                    fs::create_dir_all(&media_dir)?;
                    created_media_dir = true;
                }
                let ext = extension_for_mime(&blob.content_type, ".bin");
                let rel =
                    reserve_media_name(&mut used_names, doc_id, &format!("doc_{doc_id}{ext}"));
                let full_path = folder.join(&rel);
                fs::write(&full_path, &blob.bytes)?;
                options.document_paths.insert(doc_id, rel);
                media_files.push(full_path);
            }
        }
    }

    let export = export_rich_page_html(page, &options);
    let html_path = folder.join(format!("{candidate_name}.html"));
    fs::write(&html_path, export.html.as_bytes())?;

    Ok(DiskExportResult {
        folder,
        html_path,
        media_files,
        export,
    })
}

fn reserve_media_name(used: &mut HashSet<String>, id: u64, raw_name: &str) -> String {
    let mut name = sanitize_file_name_part(raw_name, "");
    if name.is_empty() {
        name = format!("media_{id}.bin");
    }
    if used.contains(&name.to_ascii_lowercase()) {
        name = if let Some(dot) = name.rfind('.')
            && dot > 0
        {
            format!("{}_{id}{}", &name[..dot], &name[dot..])
        } else {
            format!("{name}_{id}")
        };
    }
    used.insert(name.to_ascii_lowercase());
    format!("media/{name}")
}

fn embeddable_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    lower.starts_with("image/") || lower.starts_with("video/") || lower.starts_with("audio/")
}

fn extension_for_mime(mime: &str, fallback: &'static str) -> &'static str {
    match mime.to_ascii_lowercase().split(';').next().unwrap_or("") {
        "image/png" => ".png",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        "image/avif" => ".avif",
        "image/svg+xml" => ".svg",
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "video/quicktime" => ".mov",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "audio/flac" => ".flac",
        "audio/wav" => ".wav",
        "application/pdf" => ".pdf",
        _ => fallback,
    }
}

fn collect_media_ids(blocks: &[RichBlock]) -> (Vec<u64>, Vec<u64>) {
    let mut photos = Vec::new();
    let mut photo_set = HashSet::new();
    let mut docs = Vec::new();
    let mut doc_set = HashSet::new();
    collect_media_ids_rec(blocks, &mut photos, &mut photo_set, &mut docs, &mut doc_set);
    (photos, docs)
}

fn collect_media_ids_from_text(
    text: &TextWithEntities,
    docs: &mut Vec<u64>,
    doc_set: &mut HashSet<u64>,
) {
    for entity in &text.entities {
        if entity.kind == EntityType::CustomEmoji
            && let Some(InlineTextObjectEntity::IvImage(img)) =
                parse_inline_text_object_entity(&entity.data)
            && img.document_id != 0
            && doc_set.insert(img.document_id)
        {
            docs.push(img.document_id);
        }
    }
}

fn collect_media_ids_rec(
    blocks: &[RichBlock],
    photos: &mut Vec<u64>,
    photo_set: &mut HashSet<u64>,
    docs: &mut Vec<u64>,
    doc_set: &mut HashSet<u64>,
) {
    for block in blocks {
        collect_media_ids_from_text(&block.text.text, docs, doc_set);
        collect_media_ids_from_text(&block.caption.text, docs, doc_set);
        match block.kind {
            RichBlockKind::Photo | RichBlockKind::EmbedPost => {
                if block.photo_id != 0 && photo_set.insert(block.photo_id) {
                    photos.push(block.photo_id);
                }
            }
            RichBlockKind::Video | RichBlockKind::Audio | RichBlockKind::File => {
                if block.document_id != 0 && doc_set.insert(block.document_id) {
                    docs.push(block.document_id);
                }
            }
            RichBlockKind::GroupedMedia => {
                for item in &block.media_items {
                    if item.kind == RichBlockKind::Photo {
                        if item.photo_id != 0 && photo_set.insert(item.photo_id) {
                            photos.push(item.photo_id);
                        }
                    } else if item.document_id != 0 && doc_set.insert(item.document_id) {
                        docs.push(item.document_id);
                    }
                }
            }
            RichBlockKind::RelatedArticles => {
                for article in &block.related_articles {
                    if article.photo_id != 0 && photo_set.insert(article.photo_id) {
                        photos.push(article.photo_id);
                    }
                }
            }
            _ => {}
        }
        for item in &block.list_items {
            collect_media_ids_from_text(&item.text.text, docs, doc_set);
            collect_media_ids_rec(&item.blocks, photos, photo_set, docs, doc_set);
        }
        for row in &block.table_rows {
            for cell in &row.cells {
                collect_media_ids_from_text(&cell.text.text, docs, doc_set);
            }
        }
        collect_media_ids_rec(&block.blocks, photos, photo_set, docs, doc_set);
    }
}

struct ExportState<'a> {
    options: &'a HtmlExportOptions,
    slideshow_used: bool,
    photo_ids: Vec<u64>,
    photo_set: HashSet<u64>,
    document_ids: Vec<u64>,
    document_set: HashSet<u64>,
    stripped_urls: usize,
}

impl ExportState<'_> {
    fn track_photo(&mut self, id: u64) {
        if id != 0 && self.photo_set.insert(id) {
            self.photo_ids.push(id);
        }
    }

    fn track_document(&mut self, id: u64) {
        if id != 0 && self.document_set.insert(id) {
            self.document_ids.push(id);
        }
    }

    fn anchor_href(&mut self, url: &str) -> String {
        if url.trim().is_empty() {
            return String::new();
        }
        match safe_export_url(url) {
            Some(safe) => format!(" href=\"{}\"", escape_html(&safe)),
            None => {
                self.stripped_urls += 1;
                String::new()
            }
        }
    }
}

fn id_attribute(anchor_id: &str) -> String {
    if anchor_id.is_empty() {
        String::new()
    } else {
        format!(" id=\"{}\"", escape_html(anchor_id))
    }
}

fn anchor_tag(anchor_id: &str) -> String {
    if anchor_id.is_empty() {
        String::new()
    } else {
        format!("<a name=\"{}\"></a>", escape_html(anchor_id))
    }
}

fn media_href(relative: &str) -> String {
    if relative.starts_with("data:") {
        return escape_html(relative);
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(relative.len());
    for &b in relative.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(b as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(b >> 4) as usize] as char);
            encoded.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    escape_html(&encoded)
}

fn tg_media_source(kind: RichBlockKind, photo_id: u64, document_id: u64) -> String {
    if kind == RichBlockKind::Photo {
        if photo_id != 0 {
            format!(" data-tg-src=\"tg://photo?id={photo_id}\"")
        } else {
            String::new()
        }
    } else if document_id != 0 {
        let label = if kind == RichBlockKind::Audio {
            "audio"
        } else {
            "video"
        };
        format!(" data-tg-src=\"tg://{label}?id={document_id}\"")
    } else {
        String::new()
    }
}

fn append_extra_anchors(out: &mut String, text: &RichTextNode) {
    out.push_str(&anchor_tag(&text.anchor_id));
    for id in &text.anchor_ids {
        out.push_str(&anchor_tag(id));
    }
}

#[derive(Default)]
struct EntityTags {
    open: String,
    close: String,
    replace: String,
    replaced: bool,
    raw_inner: bool,
}

fn link_tags(url: &str, state: &mut ExportState<'_>) -> EntityTags {
    let href = state.anchor_href(url);
    if href.is_empty() {
        EntityTags::default()
    } else {
        EntityTags {
            open: format!("<a{href}>"),
            close: "</a>".to_string(),
            ..EntityTags::default()
        }
    }
}

fn deserialize_date_flags(data: &str) -> (i32, DateFlags) {
    let Some((d_str, code)) = data.split_once(':') else {
        return (data.parse::<i32>().unwrap_or(0), DateFlags::default());
    };
    let date = d_str.parse::<i32>().unwrap_or(0);
    let mut flags = DateFlags::default();
    for ch in code.chars() {
        match ch {
            'r' => flags.relative = true,
            't' => flags.short_time = true,
            'T' => flags.long_time = true,
            'd' => flags.short_date = true,
            'D' => flags.long_date = true,
            'w' => flags.day_of_week = true,
            _ => {}
        }
    }
    (date, flags)
}

fn entity_tags_for(entity: &EntityInText, inner: &str, state: &mut ExportState<'_>) -> EntityTags {
    match entity.kind {
        EntityType::Bold => EntityTags {
            open: "<strong>".to_string(),
            close: "</strong>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Italic => EntityTags {
            open: "<em>".to_string(),
            close: "</em>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Underline => EntityTags {
            open: "<u>".to_string(),
            close: "</u>".to_string(),
            ..EntityTags::default()
        },
        EntityType::StrikeOut => EntityTags {
            open: "<s>".to_string(),
            close: "</s>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Code => EntityTags {
            open: "<code>".to_string(),
            close: "</code>".to_string(),
            raw_inner: true,
            ..EntityTags::default()
        },
        EntityType::Pre => EntityTags {
            open: "<pre>".to_string(),
            close: "</pre>".to_string(),
            raw_inner: true,
            ..EntityTags::default()
        },
        EntityType::Blockquote => EntityTags {
            open: "<blockquote>".to_string(),
            close: "</blockquote>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Spoiler => EntityTags {
            open: "<tg-spoiler>".to_string(),
            close: "</tg-spoiler>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Subscript => EntityTags {
            open: "<sub>".to_string(),
            close: "</sub>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Superscript => EntityTags {
            open: "<sup>".to_string(),
            close: "</sup>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Marked => EntityTags {
            open: "<mark>".to_string(),
            close: "</mark>".to_string(),
            ..EntityTags::default()
        },
        EntityType::Url => link_tags(inner, state),
        EntityType::CustomUrl => {
            let url = decode_rich_page_link_url(&entity.data)
                .map(|d| d.url)
                .unwrap_or_else(|| entity.data.clone());
            link_tags(&url, state)
        }
        EntityType::Email => link_tags(&format!("mailto:{inner}"), state),
        EntityType::Phone => link_tags(&format!("tel:{inner}"), state),
        EntityType::Mention => {
            let username = inner.strip_prefix('@').unwrap_or(inner);
            link_tags(&format!("https://t.me/{username}"), state)
        }
        EntityType::MentionName => {
            if let Ok(uid) = entity.data.trim().parse::<u64>()
                && uid != 0
            {
                link_tags(&format!("tg://user?id={uid}"), state)
            } else {
                EntityTags::default()
            }
        }
        EntityType::CustomEmoji => {
            if let Some(obj) = parse_inline_text_object_entity(&entity.data) {
                match obj {
                    InlineTextObjectEntity::Formula(f) => EntityTags {
                        replace: format!("<tg-math>{}</tg-math>", escape_html(&f.trimmed_tex)),
                        replaced: true,
                        ..EntityTags::default()
                    },
                    InlineTextObjectEntity::IvImage(img) => {
                        state.track_document(img.document_id);
                        if let Some(path) = state.options.document_paths.get(&img.document_id) {
                            EntityTags {
                                replace: format!(
                                    "<img class=\"inline-image\" src=\"{}\" alt=\"{}\">",
                                    media_href(path),
                                    escape_html(&img.replacement_text)
                                ),
                                replaced: true,
                                ..EntityTags::default()
                            }
                        } else {
                            let fallback = if img.replacement_text.is_empty() {
                                inner
                            } else {
                                &img.replacement_text
                            };
                            EntityTags {
                                replace: escape_html(fallback),
                                replaced: true,
                                ..EntityTags::default()
                            }
                        }
                    }
                    InlineTextObjectEntity::Button(btn) => {
                        let label = escape_html(&btn.label.text);
                        if matches!(
                            btn.button_type,
                            RichButtonType::Url | RichButtonType::WebView | RichButtonType::UrlAuth
                        ) && let Ok(url) = std::str::from_utf8(&btn.data)
                        {
                            let href = state.anchor_href(url);
                            if !href.is_empty() {
                                return EntityTags {
                                    replace: format!(
                                        "<a class=\"inline-button\"{href}>{label}</a>"
                                    ),
                                    replaced: true,
                                    ..EntityTags::default()
                                };
                            }
                        }
                        EntityTags {
                            replace: format!("<span class=\"inline-button\">{label}</span>"),
                            replaced: true,
                            ..EntityTags::default()
                        }
                    }
                }
            } else if let Ok(emoji_id) = entity.data.trim().parse::<u64>()
                && emoji_id != 0
            {
                EntityTags {
                    open: format!("<tg-emoji emoji-id=\"{emoji_id}\">"),
                    close: "</tg-emoji>".to_string(),
                    ..EntityTags::default()
                }
            } else {
                EntityTags::default()
            }
        }
        EntityType::FormattedDate => {
            let (unix, flags) = deserialize_date_flags(&entity.data);
            EntityTags {
                open: format!(
                    "<tg-time unix=\"{unix}\" format=\"{}\">",
                    flags.format_code()
                ),
                close: "</tg-time>".to_string(),
                ..EntityTags::default()
            }
        }
        _ => EntityTags::default(),
    }
}

fn slice_utf16(text: &str, from: usize, till: usize) -> &str {
    let b0 = utf16_to_byte_offset(text, from);
    let b1 = utf16_to_byte_offset(text, till);
    if b0 <= b1 && b1 <= text.len() {
        &text[b0..b1]
    } else {
        ""
    }
}

#[allow(clippy::too_many_arguments)]
fn serialize_text_part(
    out: &mut String,
    text: &str,
    entities: &[EntityInText],
    from: usize,
    till: usize,
    entity_from: usize,
    line_breaks: bool,
    state: &mut ExportState<'_>,
) {
    let mut pos = from;
    for (i, entity) in entities.iter().enumerate().skip(entity_from) {
        if entity.offset >= till {
            break;
        }
        if entity.offset < pos || entity.length == 0 {
            continue;
        }
        let end = entity.end().min(till);
        append_escaped(out, slice_utf16(text, pos, entity.offset), line_breaks);
        let inner = slice_utf16(text, entity.offset, end);
        let tags = entity_tags_for(entity, inner, state);
        if tags.replaced {
            out.push_str(&tags.replace);
        } else {
            out.push_str(&tags.open);
            serialize_text_part(
                out,
                text,
                entities,
                entity.offset,
                end,
                i + 1,
                line_breaks && !tags.raw_inner,
                state,
            );
            out.push_str(&tags.close);
        }
        pos = end;
    }
    append_escaped(out, slice_utf16(text, pos, till), line_breaks);
}

fn serialize_text(
    out: &mut String,
    text: &TextWithEntities,
    line_breaks: bool,
    state: &mut ExportState<'_>,
) {
    let mut sorted = text.entities.clone();
    sorted.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| b.length.cmp(&a.length))
    });
    let total = text.utf16_len();
    serialize_text_part(out, &text.text, &sorted, 0, total, 0, line_breaks, state);
}

fn serialize_rich_text(
    out: &mut String,
    text: &RichTextNode,
    line_breaks: bool,
    state: &mut ExportState<'_>,
) {
    append_extra_anchors(out, text);
    serialize_text(out, &text.text, line_breaks, state);
}

fn append_missing_media(out: &mut String, label: &str, state: &ExportState<'_>) {
    let shown = if label.is_empty() {
        state.options.missing_media_label.as_str()
    } else {
        label
    };
    let _ = write!(
        out,
        "<div class=\"media-missing\">{}</div>",
        escape_html(shown)
    );
}

fn serialize_caption(out: &mut String, caption: &RichTextNode, state: &mut ExportState<'_>) {
    if !caption.text.is_empty() {
        out.push_str("<figcaption>");
        serialize_rich_text(out, caption, true, state);
        out.push_str("</figcaption>");
    }
}

#[allow(clippy::too_many_arguments)]
fn serialize_media_item(
    out: &mut String,
    kind: RichBlockKind,
    photo_id: u64,
    document_id: u64,
    width: i32,
    height: i32,
    autoplay: bool,
    loop_video: bool,
    spoiler: bool,
    state: &mut ExportState<'_>,
) {
    let dimensions = if width > 0 && height > 0 {
        format!(" width=\"{width}\" height=\"{height}\"")
    } else {
        String::new()
    };
    let identity = tg_media_source(kind, photo_id, document_id);
    let hidden = if spoiler { " tg-spoiler" } else { "" };

    if kind == RichBlockKind::Photo {
        state.track_photo(photo_id);
        if let Some(path) = state.options.photo_paths.get(&photo_id) {
            let _ = write!(
                out,
                "<img src=\"{}\" loading=\"lazy\"{dimensions}{identity}{hidden}>",
                media_href(path)
            );
        } else if !state.options.clipboard {
            append_missing_media(out, "", state);
        } else if !identity.is_empty() {
            let _ = write!(out, "<img{dimensions}{identity}{hidden}>");
        }
    } else {
        state.track_document(document_id);
        let flags = if autoplay {
            " autoplay muted loop playsinline"
        } else if loop_video {
            " controls loop preload=\"metadata\""
        } else {
            " controls preload=\"metadata\""
        };
        let poster = state
            .options
            .document_posters
            .get(&document_id)
            .map(|p| format!(" poster=\"{}\"", media_href(p)))
            .unwrap_or_default();
        if let Some(path) = state.options.document_paths.get(&document_id) {
            let _ = write!(
                out,
                "<video src=\"{}\"{poster}{flags}{dimensions}{identity}{hidden}></video>",
                media_href(path)
            );
        } else if !state.options.clipboard {
            append_missing_media(out, "", state);
        } else if !identity.is_empty() {
            let _ = write!(
                out,
                "<video{poster}{flags}{dimensions}{identity}{hidden}></video>"
            );
        }
    }
}

fn serialize_fill_media(out: &mut String, item: &GroupedMediaItem, state: &mut ExportState<'_>) {
    serialize_media_item(
        out,
        item.kind,
        item.photo_id,
        item.document_id,
        item.width,
        item.height,
        item.autoplay,
        item.loop_video,
        item.spoiler,
        state,
    );
}

fn serialize_collage(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    let sizes: Vec<MediaSize> = block
        .media_items
        .iter()
        .map(|item| MediaSize::new(item.width.max(1), item.height.max(1)))
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
    let count = layout.len().min(block.media_items.len());
    if count == 0 || full.is_empty() {
        return;
    }
    let _ = write!(
        out,
        "<tg-collage{}><div class=\"collage-box\" style=\"aspect-ratio:{}/{}\">",
        id_attribute(&block.anchor_id),
        full.width,
        full.height
    );
    for (idx, part) in layout.iter().take(count).enumerate() {
        let g = part.geometry;
        let left = f64::from(g.x - full.x) * 100.0 / f64::from(full.width);
        let top = f64::from(g.y - full.y) * 100.0 / f64::from(full.height);
        let width = f64::from(g.width) * 100.0 / f64::from(full.width);
        let height = f64::from(g.height) * 100.0 / f64::from(full.height);
        let _ = write!(
            out,
            "<div class=\"collage-item\" style=\"left:{left:.3}%;top:{top:.3}%;width:{width:.3}%;height:{height:.3}%\">"
        );
        serialize_fill_media(out, &block.media_items[idx], state);
        out.push_str("</div>");
    }
    out.push_str("</div>");
    serialize_caption(out, &block.caption, state);
    out.push_str("</tg-collage>\n");
}

fn serialize_slideshow(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    if block.media_items.is_empty() {
        return;
    }
    let mut best_w = 1i32;
    let mut best_h = 1i32;
    let mut best_ratio = 0.0f64;
    for item in &block.media_items {
        let w = item.width.max(1);
        let h = item.height.max(1);
        let ratio = f64::from(w) / f64::from(h);
        if ratio > best_ratio {
            best_ratio = ratio;
            best_w = w;
            best_h = h;
        }
    }
    let controls = block.media_items.len() > 1;
    if controls {
        state.slideshow_used = true;
    }
    let _ = write!(
        out,
        "<tg-slideshow{}><div class=\"slideshow-box\" style=\"aspect-ratio:{best_w}/{best_h}\"><div class=\"slideshow-track\">",
        id_attribute(&block.anchor_id)
    );
    for item in &block.media_items {
        out.push_str("<div class=\"slide\">");
        serialize_fill_media(out, item, state);
        out.push_str("</div>");
    }
    out.push_str("</div>");
    if controls {
        out.push_str(
            "<button class=\"slideshow-prev\" type=\"button\">&#8592;</button>\
             <button class=\"slideshow-next\" type=\"button\">&#8594;</button>\
             <div class=\"slideshow-dots\">",
        );
        for _ in 0..block.media_items.len() {
            out.push_str("<button class=\"slideshow-dot\" type=\"button\"></button>");
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    serialize_caption(out, &block.caption, state);
    out.push_str("</tg-slideshow>\n");
}

fn serialize_list_items(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    for item in &block.list_items {
        let _ = write!(out, "<li{}", id_attribute(&item.anchor_id));
        if let Some(value) = item.number.value {
            let _ = write!(out, " value=\"{value}\"");
        }
        out.push('>');
        if item.task_state != RichTaskState::None {
            let checked = if item.task_state == RichTaskState::Checked {
                " checked"
            } else {
                ""
            };
            let _ = write!(out, "<input type=\"checkbox\" disabled{checked}> ");
        }
        serialize_rich_text(out, &item.text, true, state);
        serialize_blocks(out, &item.blocks, state);
        out.push_str("</li>\n");
    }
}

fn serialize_list(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    if block.list_kind == RichListKind::Ordered {
        let mut attrs = id_attribute(&block.anchor_id);
        if block.ordered_list.reversed {
            attrs.push_str(" reversed");
        }
        if let Some(start) = block.ordered_list.start {
            let _ = write!(attrs, " start=\"{start}\"");
        }
        if let Some(ref ty) = block.ordered_list.list_type
            && matches!(ty.as_str(), "1" | "a" | "A" | "i" | "I")
        {
            let _ = write!(attrs, " type=\"{ty}\"");
        }
        let _ = writeln!(out, "<ol{attrs}>");
        serialize_list_items(out, block, state);
        out.push_str("</ol>\n");
    } else {
        let _ = writeln!(out, "<ul{}>", id_attribute(&block.anchor_id));
        serialize_list_items(out, block, state);
        out.push_str("</ul>\n");
    }
}

fn serialize_table(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    let _ = write!(
        out,
        "<figure class=\"table-wrap\"{}>",
        id_attribute(&block.anchor_id)
    );
    if !block.text.text.is_empty() {
        out.push_str("<div class=\"table-title\">");
        serialize_rich_text(out, &block.text, true, state);
        out.push_str("</div>");
    }
    let mut classes = Vec::new();
    if block.bordered {
        classes.push("bordered");
    }
    if block.striped {
        classes.push("striped");
    }
    if block.compact {
        classes.push("compact");
    }
    if classes.is_empty() {
        out.push_str("<table>\n");
    } else {
        let _ = writeln!(out, "<table class=\"{}\">", classes.join(" "));
    }
    for row in &block.table_rows {
        out.push_str("<tr>");
        for cell in &row.cells {
            let tag = if cell.header { "th" } else { "td" };
            let mut attrs = String::new();
            if cell.colspan > 1 {
                let _ = write!(attrs, " colspan=\"{}\"", cell.colspan);
            }
            if cell.rowspan > 1 {
                let _ = write!(attrs, " rowspan=\"{}\"", cell.rowspan);
            }
            let mut style = String::new();
            match cell.alignment {
                RichTableAlignment::Center => style.push_str("text-align:center;"),
                RichTableAlignment::Right => style.push_str("text-align:right;"),
                RichTableAlignment::Left => {}
            }
            match cell.vertical_alignment {
                RichTableVerticalAlignment::Middle => style.push_str("vertical-align:middle;"),
                RichTableVerticalAlignment::Bottom => style.push_str("vertical-align:bottom;"),
                RichTableVerticalAlignment::Top => {}
            }
            if !style.is_empty() {
                let _ = write!(attrs, " style=\"{style}\"");
            }
            let _ = write!(out, "<{tag}{attrs}>");
            serialize_rich_text(out, &cell.text, true, state);
            let _ = write!(out, "</{tag}>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table></figure>\n");
}

fn serialize_related_articles(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    let _ = write!(
        out,
        "<section class=\"related\"{}",
        id_attribute(&block.anchor_id)
    );
    out.push('>');
    if !block.text.text.is_empty() {
        out.push_str("<div class=\"related-title\">");
        serialize_rich_text(out, &block.text, true, state);
        out.push_str("</div>");
    }
    for article in &block.related_articles {
        state.track_photo(article.photo_id);
        let href = state.anchor_href(&article.url);
        if href.is_empty() {
            out.push_str("<div class=\"related-card\">");
        } else {
            let _ = write!(out, "<a class=\"related-card\"{href}>");
        }
        if let Some(path) = state.options.photo_paths.get(&article.photo_id) {
            let _ = write!(out, "<img src=\"{}\">", media_href(path));
        }
        if !article.title.is_empty() {
            let _ = write!(
                out,
                "<div class=\"related-name\">{}</div>",
                escape_html(&article.title)
            );
        }
        if !article.description.is_empty() {
            let _ = write!(
                out,
                "<div class=\"related-desc\">{}</div>",
                escape_html(&article.description)
            );
        }
        let mut meta = Vec::new();
        if !article.author.is_empty() {
            meta.push(escape_html(&article.author));
        }
        if article.published_date != 0
            && let Some(date) = format_utc_date(article.published_date)
        {
            meta.push(escape_html(&date));
        }
        if !meta.is_empty() {
            let _ = write!(
                out,
                "<div class=\"related-meta\">{}</div>",
                meta.join(" · ")
            );
        }
        if href.is_empty() {
            out.push_str("</div>\n");
        } else {
            out.push_str("</a>\n");
        }
    }
    out.push_str("</section>\n");
}

fn serialize_block(out: &mut String, block: &RichBlock, state: &mut ExportState<'_>) {
    match block.kind {
        RichBlockKind::Heading => {
            let level = block.heading_level.clamp(1, 6);
            let _ = write!(out, "<h{level}{}>", id_attribute(&block.anchor_id));
            serialize_rich_text(out, &block.text, true, state);
            let _ = writeln!(out, "</h{level}>");
        }
        RichBlockKind::Paragraph => {
            let _ = write!(out, "<p{}>", id_attribute(&block.anchor_id));
            serialize_rich_text(out, &block.text, true, state);
            out.push_str("</p>\n");
        }
        RichBlockKind::Footer => {
            let _ = write!(out, "<footer{}>", id_attribute(&block.anchor_id));
            serialize_rich_text(out, &block.text, true, state);
            out.push_str("</footer>\n");
        }
        RichBlockKind::Thinking => {
            let _ = write!(
                out,
                "<blockquote class=\"thinking\"{}>",
                id_attribute(&block.anchor_id)
            );
            serialize_rich_text(out, &block.text, true, state);
            out.push_str("</blockquote>\n");
        }
        RichBlockKind::AuthorDate => {
            let _ = write!(
                out,
                "<p class=\"byline\"{}>",
                id_attribute(&block.anchor_id)
            );
            serialize_rich_text(out, &block.text, true, state);
            if block.date != 0
                && let Some(date) = format_utc_date(block.date)
            {
                if !block.text.text.is_empty() {
                    out.push_str(" · ");
                }
                out.push_str(&escape_html(&date));
            }
            out.push_str("</p>\n");
        }
        RichBlockKind::Code => {
            let lang_attr = if block.language.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{}\"", escape_html(&block.language))
            };
            let _ = write!(
                out,
                "<pre class=\"code\"{}><code{lang_attr}>",
                id_attribute(&block.anchor_id)
            );
            serialize_rich_text(out, &block.text, false, state);
            out.push_str("</code></pre>\n");
        }
        RichBlockKind::Divider => {
            out.push_str("<hr>\n");
        }
        RichBlockKind::Anchor => {
            let tag = anchor_tag(&block.anchor_id);
            if !tag.is_empty() {
                out.push_str(&tag);
                out.push('\n');
            }
        }
        RichBlockKind::List => serialize_list(out, block, state),
        RichBlockKind::Quote => {
            let tag = if block.pullquote {
                "aside"
            } else {
                "blockquote"
            };
            let _ = write!(out, "<{tag}{}>", id_attribute(&block.anchor_id));
            serialize_rich_text(out, &block.text, true, state);
            serialize_blocks(out, &block.blocks, state);
            if !block.caption.text.is_empty() {
                out.push_str("<cite>");
                serialize_rich_text(out, &block.caption, true, state);
                out.push_str("</cite>");
            }
            let _ = writeln!(out, "</{tag}>");
        }
        RichBlockKind::Photo => {
            let _ = write!(
                out,
                "<figure class=\"media\"{}>",
                id_attribute(&block.anchor_id)
            );
            let href = state.anchor_href(&block.url);
            if !href.is_empty() {
                let _ = write!(out, "<a{href}>");
            }
            serialize_media_item(
                out,
                RichBlockKind::Photo,
                block.photo_id,
                0,
                block.width,
                block.height,
                false,
                false,
                block.spoiler,
                state,
            );
            if !href.is_empty() {
                out.push_str("</a>");
            }
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::Video => {
            let _ = write!(
                out,
                "<figure class=\"media\"{}>",
                id_attribute(&block.anchor_id)
            );
            serialize_media_item(
                out,
                RichBlockKind::Video,
                0,
                block.document_id,
                block.width,
                block.height,
                block.autoplay,
                block.loop_video,
                block.spoiler,
                state,
            );
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::Audio => {
            state.track_document(block.document_id);
            let _ = write!(
                out,
                "<figure class=\"audio\"{}>",
                id_attribute(&block.anchor_id)
            );
            let identity = tg_media_source(RichBlockKind::Audio, 0, block.document_id);
            if let Some(path) = state.options.document_paths.get(&block.document_id) {
                let _ = write!(
                    out,
                    "<audio controls src=\"{}\"{identity}></audio>",
                    media_href(path)
                );
            } else if !state.options.clipboard {
                append_missing_media(out, &block.file_name, state);
            } else if !identity.is_empty() {
                let _ = write!(out, "<audio controls{identity}></audio>");
            }
            let mut meta = Vec::new();
            if !block.audio_performer.is_empty() {
                meta.push(escape_html(&block.audio_performer));
            }
            if !block.audio_title.is_empty() {
                meta.push(escape_html(&block.audio_title));
            }
            if !meta.is_empty() {
                let _ = write!(out, "<div class=\"audio-meta\">{}</div>", meta.join(" — "));
            }
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::File => {
            state.track_document(block.document_id);
            let _ = write!(
                out,
                "<figure class=\"file\"{}>",
                id_attribute(&block.anchor_id)
            );
            let label = if block.file_name.is_empty() {
                "File"
            } else {
                &block.file_name
            };
            if let Some(path) = state.options.document_paths.get(&block.document_id) {
                let _ = write!(
                    out,
                    "<a href=\"{}\" download>{}</a>",
                    media_href(path),
                    escape_html(label)
                );
            } else if !state.options.clipboard {
                append_missing_media(out, label, state);
            }
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::GroupedMedia => {
            if block.media_intent == GroupedMediaIntent::Slideshow {
                serialize_slideshow(out, block, state);
            } else {
                serialize_collage(out, block, state);
            }
        }
        RichBlockKind::Channel => {
            let link = if block.username.is_empty() {
                String::new()
            } else {
                format!("https://t.me/{}", block.username)
            };
            let href = state.anchor_href(&link);
            let _ = write!(
                out,
                "<div class=\"channel\"{}>",
                id_attribute(&block.anchor_id)
            );
            if !href.is_empty() {
                let _ = write!(out, "<a{href}>{}</a>", escape_html(&block.channel_title));
            } else {
                out.push_str(&escape_html(&block.channel_title));
            }
            out.push_str("</div>\n");
        }
        RichBlockKind::Embed => {
            // Never emit raw `block.html` in exported HTML (mirrors iv_rich_message_html_export.cpp).
            let _ = write!(
                out,
                "<figure class=\"embed\"{}>",
                id_attribute(&block.anchor_id)
            );
            let href = state.anchor_href(&block.url);
            if !href.is_empty() {
                let _ = write!(out, "<a{href}>{}</a>", escape_html(&block.url));
            } else {
                append_missing_media(out, &state.options.embed_fallback_label, state);
            }
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::EmbedPost => {
            state.track_photo(block.photo_id);
            let _ = write!(
                out,
                "<blockquote class=\"embed-post\"{}>",
                id_attribute(&block.anchor_id)
            );
            if let Some(path) = state.options.photo_paths.get(&block.photo_id) {
                let _ = write!(
                    out,
                    "<img class=\"embed-post-photo\" src=\"{}\">",
                    media_href(path)
                );
            }
            let mut header = Vec::new();
            if !block.author.is_empty() {
                header.push(escape_html(&block.author));
            }
            if block.date != 0
                && let Some(date) = format_utc_date(block.date)
            {
                header.push(escape_html(&date));
            }
            if !header.is_empty() {
                let href = state.anchor_href(&block.url);
                let joined = header.join(" · ");
                if href.is_empty() {
                    let _ = write!(out, "<div class=\"embed-post-header\">{joined}</div>");
                } else {
                    let _ = write!(
                        out,
                        "<div class=\"embed-post-header\"><a{href}>{joined}</a></div>"
                    );
                }
            }
            serialize_blocks(out, &block.blocks, state);
            serialize_caption(out, &block.caption, state);
            out.push_str("</blockquote>\n");
        }
        RichBlockKind::Math => {
            let _ = writeln!(
                out,
                "<tg-math-block{}>{}</tg-math-block>",
                id_attribute(&block.anchor_id),
                escape_html(block.formula.trim())
            );
        }
        RichBlockKind::Table => serialize_table(out, block, state),
        RichBlockKind::Details => {
            let open_attr = if block.open { " open" } else { "" };
            let _ = write!(
                out,
                "<details{}{open_attr}><summary>",
                id_attribute(&block.anchor_id)
            );
            serialize_rich_text(out, &block.text, true, state);
            out.push_str("</summary>\n");
            serialize_blocks(out, &block.blocks, state);
            out.push_str("</details>\n");
        }
        RichBlockKind::RelatedArticles => serialize_related_articles(out, block, state),
        RichBlockKind::Map => {
            let _ = write!(
                out,
                "<figure class=\"map\"{}>",
                id_attribute(&block.anchor_id)
            );
            if block.zoom > 0
                && block.latitude.is_finite()
                && block.longitude.is_finite()
                && (-90.0..=90.0).contains(&block.latitude)
                && (-180.0..=180.0).contains(&block.longitude)
            {
                let lat = block.latitude.to_string();
                let lon = block.longitude.to_string();
                let zoom = block.zoom.to_string();
                let url = format!(
                    "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map={zoom}/{lat}/{lon}"
                );
                let href = state.anchor_href(&url);
                let _ = write!(
                    out,
                    "<tg-map lat=\"{lat}\" long=\"{lon}\" zoom=\"{zoom}\"></tg-map><a{href}>{}, {}</a>",
                    escape_html(&lat),
                    escape_html(&lon)
                );
            }
            serialize_caption(out, &block.caption, state);
            out.push_str("</figure>\n");
        }
        RichBlockKind::ButtonRow => {
            let _ = write!(
                out,
                "<div class=\"button-row\"{}>",
                id_attribute(&block.anchor_id)
            );
            for btn in &block.buttons {
                let mut label_html = String::new();
                serialize_rich_text(&mut label_html, &btn.text, false, state);
                if matches!(
                    btn.button_type,
                    RichButtonType::Url | RichButtonType::WebView | RichButtonType::UrlAuth
                ) && let Ok(url) = std::str::from_utf8(&btn.data)
                {
                    let href = state.anchor_href(url);
                    if !href.is_empty() {
                        let _ = write!(out, "<a class=\"rich-button\"{href}>{label_html}</a>");
                        continue;
                    }
                }
                let _ = write!(out, "<span class=\"rich-button\">{label_html}</span>");
            }
            out.push_str("</div>\n");
        }
        RichBlockKind::Unsupported => {}
    }
}

fn serialize_blocks(out: &mut String, blocks: &[RichBlock], state: &mut ExportState<'_>) {
    for block in blocks {
        serialize_block(out, block, state);
    }
}
