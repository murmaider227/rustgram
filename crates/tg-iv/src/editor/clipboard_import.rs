//! Port of `Telegram/SourceFiles/iv/editor/iv_editor_clipboard_import.h` and
//! `iv_editor_clipboard_import.cpp`.
//!
//! Sanitizes pasted HTML fragments, exported rich-message HTML files, Markdown text, and
//! delimited spreadsheet tables into valid [`RichBlock`] trees while enforcing
//! [`RichMessageLimits`], stripping dangerous HTML tags/scripts/`javascript:` URLs, and
//! rejecting UNC/network paths on Windows.

use std::path::{Path, PathBuf};

use crate::cached_media::sniff_content_type;
use crate::editor::blocks::degrade_block_only_entities;
use crate::editor::text_ops::formula_source_to_rich_text;
use crate::html_export::safe_export_url;
use crate::model::{normalize_fragment_id, percent_decode_utf8};
use crate::rich_page::{
    EntityInText, EntityType, GroupedMediaIntent, GroupedMediaItem, OrderedListData,
    OrderedListItemData, RICH_EXPORT_GENERATOR_MARKER, RichBlock, RichBlockKind, RichListItem,
    RichListKind, RichMessageLimits, RichTableAlignment, RichTableCell, RichTableRow,
    RichTaskState, RichTextNode, TextWithEntities,
};

/// Maximum imported table cell length in characters (`kMaxCellLength`).
pub const MAX_CELL_LENGTH: usize = 4096;

/// Maximum length of a `data:` image URI accepted on paste (`kMaxDataUriLength`, 24 MiB).
pub const MAX_DATA_URI_LENGTH: usize = 24 * 1024 * 1024;

/// Maximum header length before `,` in a `data:` image URI (`kMaxDataUriHeaderLength`).
pub const MAX_DATA_URI_HEADER_LENGTH: usize = 256;

/// Maximum number of local media placeholders per paste (`kImportedMediaPlaceholderLimit`).
pub const IMPORTED_MEDIA_PLACEHOLDER_LIMIT: usize = 4096;

/// Maximum raw Markdown clipboard length accepted (`kMaxMarkdownImportLength`, 256 KiB).
pub const MAX_MARKDOWN_IMPORT_LENGTH: usize = 256 * 1024;

/// Maximum blockquote nesting depth during Markdown paste (`kMaxMarkdownQuoteDepth`).
pub const MAX_MARKDOWN_QUOTE_DEPTH: usize = 8;

/// Base ID used for imported local media placeholders (`ImportedMediaPlaceholderId`).
pub const IMPORTED_MEDIA_PLACEHOLDER_BASE: u64 = 0xF000_0000_0000_0000;

/// Returns the synthetic placeholder media ID for imported local media at `index`.
#[must_use]
pub const fn imported_media_placeholder_id(index: usize) -> u64 {
    IMPORTED_MEDIA_PLACEHOLDER_BASE + (index as u64)
}

/// Extracts the index of a synthetic placeholder media ID, if `id` falls in the placeholder range.
#[must_use]
pub const fn imported_media_placeholder_index(id: u64) -> Option<usize> {
    let last = IMPORTED_MEDIA_PLACEHOLDER_BASE + (IMPORTED_MEDIA_PLACEHOLDER_LIMIT as u64) - 1;
    if id >= IMPORTED_MEDIA_PLACEHOLDER_BASE && id <= last {
        Some((id - IMPORTED_MEDIA_PLACEHOLDER_BASE) as usize)
    } else {
        None
    }
}

/// Limits applied when importing a pasted table (`Iv::Editor::TableImportLimits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TableImportLimits {
    /// Maximum number of rows.
    pub max_rows: usize,
    /// Maximum number of columns.
    pub max_columns: usize,
    /// Maximum total cells across the table.
    pub max_cells: usize,
    /// Maximum text length per cell.
    pub max_cell_length: usize,
}

/// Result of importing a single table from clipboard HTML or TSV (`Iv::Editor::TableImportResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct TableImportResult {
    /// Imported `RichBlockKind::Table` block.
    pub block: RichBlock,
    /// True if rows, columns, cells, or cell lengths were truncated to fit limits.
    pub truncated: bool,
}

/// A local or data-URI media asset referenced by pasted HTML (`Iv::Editor::ImportedLocalMedia`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportedLocalMedia {
    /// Canonical filesystem path when resolved from a local file.
    pub path: String,
    /// Raw decoded image bytes when resolved from a `data:image/...;base64,...` URI.
    pub content: Vec<u8>,
    /// True if sourced from a `file:` URI or a path outside the exported HTML folder.
    pub external: bool,
}

/// Result of importing clipboard HTML or Markdown into IV blocks (`Iv::Editor::BlocksImportResult`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BlocksImportResult {
    /// Sanitized [`RichBlock`] sequence ready to insert into an editor document.
    pub blocks: Vec<RichBlock>,
    /// Local or inline-data media references collected during HTML import.
    pub local_media: Vec<ImportedLocalMedia>,
    /// True if content was truncated due to [`RichMessageLimits`].
    pub truncated: bool,
}

/// Abstract clipboard payload supplied by a host UI or unit test (`QMimeData` equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardPayload {
    /// Optional `text/html` clipboard payload.
    pub html: Option<String>,
    /// Optional `text/plain` clipboard payload.
    pub plain_text: Option<String>,
    /// Optional base directory when importing a local exported `.html` file.
    pub base_path: Option<PathBuf>,
    /// True if native spreadsheet clipboard formats (Excel Biff/XML, Calc) are present.
    pub spreadsheet_hint: bool,
}

/// Computes [`TableImportLimits`] from [`RichMessageLimits`] and already-used blocks
/// (`TableImportLimitsFor`).
#[must_use]
pub fn table_import_limits_for(
    limits: &RichMessageLimits,
    used_blocks: usize,
) -> TableImportLimits {
    let rows = limits.max_blocks.saturating_sub(used_blocks + 1);
    let columns = limits.max_table_cols;
    if rows == 0 || columns == 0 {
        return TableImportLimits::default();
    }
    TableImportLimits {
        max_rows: rows,
        max_columns: columns,
        max_cells: rows.saturating_mul(columns),
        max_cell_length: MAX_CELL_LENGTH,
    }
}

/// Returns `true` if `path` is a UNC or device network path (`\\server\...` or `//server/...`).
///
/// Matches `IsNetworkPath` in `iv_editor_clipboard_import.cpp` to prevent leaking Windows SMB
/// credentials when probing paths found in pasted HTML.
#[must_use]
pub fn is_network_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && matches!(bytes[0], b'/' | b'\\') && matches!(bytes[1], b'/' | b'\\')
}

/// Returns `true` if `extension` (without leading dot) is a known media file extension
/// (`LooksLikeMediaExtension`).
#[must_use]
pub fn looks_like_media_extension(extension: &str) -> bool {
    const KNOWN: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif", "tiff", "avif", "mp4", "mov",
        "m4v", "webm", "mkv", "avi", "mp3", "m4a", "ogg", "oga", "opus", "flac", "wav", "aac",
    ];
    let lower = extension.to_ascii_lowercase();
    KNOWN.contains(&lower.as_str())
}

/// Returns `true` if `head` contains the Telegram Desktop rich-message HTML export generator tag
/// (`ExportedHtmlFilePath` / `MimeDataLooksLikeExportedHtml`).
#[must_use]
pub fn looks_like_exported_html(head: &str) -> bool {
    head.contains(RICH_EXPORT_GENERATOR_MARKER)
}

/// Decodes and validates a `data:image/...;base64,...` URI into raw image bytes
/// (`DataUriImageContent`). Rejects non-image payloads and oversized URIs.
#[must_use]
pub fn data_uri_image_content(source: &str) -> Option<Vec<u8>> {
    let comma = source.find(',')?;
    if comma > MAX_DATA_URI_HEADER_LENGTH || source.len() > MAX_DATA_URI_LENGTH {
        return None;
    }
    let header_raw = &source[..comma];
    let lower_header = header_raw.to_ascii_lowercase();
    if !lower_header.starts_with("data:image/") {
        return None;
    }
    let compact_header: String = lower_header
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    if !compact_header.ends_with(";base64") {
        return None;
    }
    let decoded = decode_base64(&source[comma + 1..])?;
    if decoded.is_empty() {
        return None;
    }
    let sniffed = sniff_content_type(&decoded);
    if matches!(
        sniffed,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
            | "image/heic"
    ) {
        Some(decoded)
    } else {
        None
    }
}

/// Resolves a local file reference from pasted HTML relative to `base_path`, blocking UNC paths
/// and requiring media extensions for external files (`ResolveLocalMedia`).
#[must_use]
pub fn resolve_local_media(source: &str, base_path: Option<&Path>) -> Option<ImportedLocalMedia> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        let local = file_url_to_local_path(trimmed)?;
        if is_network_path(&local) {
            return None;
        }
        let path = Path::new(&local);
        if !path.is_file() {
            return None;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !looks_like_media_extension(ext) {
            return None;
        }
        let canonical = path.canonicalize().ok()?;
        return Some(ImportedLocalMedia {
            path: canonical.to_string_lossy().into_owned(),
            content: Vec::new(),
            external: true,
        });
    }

    let base = base_path?;
    if trimmed.contains("://") || is_network_path(trimmed) {
        return None;
    }
    let decoded = percent_decode_utf8(trimmed);
    if is_network_path(&decoded) {
        return None;
    }
    let candidate = base.join(&decoded);
    if !candidate.is_file() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    let root = base.canonicalize().ok()?;
    let external = !canonical.starts_with(&root);
    if external {
        let ext = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !looks_like_media_extension(ext) {
            return None;
        }
    }
    Some(ImportedLocalMedia {
        path: canonical.to_string_lossy().into_owned(),
        content: Vec::new(),
        external,
    })
}

fn file_url_to_local_path(url: &str) -> Option<String> {
    let rest = &url[5..]; // strip "file:"
    let rest = rest.strip_prefix("//")?;
    // Only allow empty host or "localhost"
    let path_part = if let Some(after) = rest.strip_prefix('/') {
        // file:///C:/... or file:///tmp/...
        let decoded = percent_decode_utf8(after);
        if decoded.len() >= 2
            && decoded.as_bytes()[0].is_ascii_alphabetic()
            && decoded.as_bytes()[1] == b':'
        {
            decoded
        } else {
            format!("/{decoded}")
        }
    } else {
        let after = rest
            .strip_prefix("localhost/")
            .or_else(|| rest.strip_prefix("LOCALHOST/"))?;
        let decoded = percent_decode_utf8(after);
        if decoded.len() >= 2
            && decoded.as_bytes()[0].is_ascii_alphabetic()
            && decoded.as_bytes()[1] == b':'
        {
            decoded
        } else {
            format!("/{decoded}")
        }
    };
    Some(path_part)
}

/// Returns `true` if `blocks` contain structure beyond what a plain compose field supports
/// (`RichBlocksCarryStructure`).
#[must_use]
pub fn rich_blocks_carry_structure(blocks: &[RichBlock]) -> bool {
    blocks.iter().any(|block| {
        !block_kind_fits_compose_field(block.kind) || rich_blocks_carry_structure(&block.blocks)
    })
}

const fn block_kind_fits_compose_field(kind: RichBlockKind) -> bool {
    matches!(
        kind,
        RichBlockKind::Paragraph | RichBlockKind::Code | RichBlockKind::Quote
    )
}

fn markdown_blocks_carry_structure(blocks: &[RichBlock]) -> bool {
    for block in blocks {
        if block.kind == RichBlockKind::List {
            for item in &block.list_items {
                if item.task_state != RichTaskState::None
                    || markdown_blocks_carry_structure(&item.blocks)
                {
                    return true;
                }
            }
        } else if !block_kind_fits_compose_field(block.kind)
            || markdown_blocks_carry_structure(&block.blocks)
        {
            return true;
        }
    }
    false
}

/// Returns `true` if `text` contains Markdown block structure beyond a plain compose field
/// (`TextHasMarkdownStructure`).
#[must_use]
pub fn text_has_markdown_structure(text: &str, limits: &RichMessageLimits) -> bool {
    blocks_from_markdown(text, limits, 0)
        .is_some_and(|(imported, _)| markdown_blocks_carry_structure(&imported.blocks))
}

/// Returns deduplicated external local media paths that require user confirmation before upload
/// (`ImportedMediaPathsToConfirm`).
#[must_use]
pub fn imported_media_paths_to_confirm(imported: &BlocksImportResult) -> Vec<String> {
    let mut out = Vec::new();
    for media in &imported.local_media {
        if media.external && !out.contains(&media.path) {
            out.push(media.path.clone());
        }
    }
    out
}

/// Returns `true` if `imported` contains no external file references (`CanPasteWithoutConfirmation`).
#[must_use]
pub fn can_paste_without_confirmation(imported: &BlocksImportResult) -> bool {
    imported.local_media.iter().all(|m| !m.external)
}

/// High-level clipboard importer: prefers sanitized HTML when present, then spreadsheet TSV
/// when `spreadsheet_hint` is set, then Markdown if structured Markdown syntax is detected.
#[must_use]
pub fn import_from_clipboard(
    payload: &ClipboardPayload,
    limits: &RichMessageLimits,
    used_blocks: usize,
) -> Option<BlocksImportResult> {
    if let Some(ref html) = payload.html
        && let Some(imported) =
            blocks_from_html(html, payload.base_path.as_deref(), limits, used_blocks)
    {
        return Some(imported);
    }
    if let Some(ref text) = payload.plain_text {
        if payload.spreadsheet_hint && text.contains('\t') {
            let table_limits = table_import_limits_for(limits, used_blocks);
            if let Some(table) = table_from_delimited_text(text, '\t', &table_limits) {
                return Some(BlocksImportResult {
                    blocks: vec![table.block],
                    local_media: Vec::new(),
                    truncated: table.truncated,
                });
            }
        }
        if let Some((imported, _)) = blocks_from_markdown(text, limits, used_blocks) {
            return Some(imported);
        }
    }
    None
}

// ============================================================================
// Delimited table import (`ParseDelimited` / `TableFromDelimitedText`)
// ============================================================================

/// Parses RFC-4180-style delimited text (e.g. TSV from spreadsheets) into a [`TableImportResult`].
#[must_use]
pub fn table_from_delimited_text(
    text: &str,
    delimiter: char,
    limits: &TableImportLimits,
) -> Option<TableImportResult> {
    if limits.max_rows == 0 || limits.max_columns == 0 || limits.max_cells == 0 {
        return None;
    }
    let mut truncated = false;
    let rows = parse_delimited(text, delimiter, limits, &mut truncated);
    if rows.is_empty() {
        return None;
    }
    let mut columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns > limits.max_columns {
        columns = limits.max_columns;
        truncated = true;
    }
    if columns == 0 || (rows.len() < 2 && columns < 2) {
        return None;
    }
    let mut block = RichBlock::new(RichBlockKind::Table);
    block.bordered = true;
    block.table_rows.reserve(rows.len());
    for mut fields in rows {
        let mut row = RichTableRow::default();
        row.cells.reserve(columns);
        for col in 0..columns {
            let mut cell = RichTableCell::default();
            if col < fields.len() {
                cell.text = RichTextNode::plain(std::mem::take(&mut fields[col]));
            }
            row.cells.push(cell);
        }
        block.table_rows.push(row);
    }
    Some(TableImportResult { block, truncated })
}

fn parse_delimited(
    text: &str,
    delimiter: char,
    limits: &TableImportLimits,
    truncated: &mut bool,
) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut result = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut cells = 0usize;
    let mut field_chars = 0usize;

    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if quoted {
            if ch != '"' {
                if field_chars < limits.max_cell_length {
                    field.push(ch);
                    field_chars += 1;
                } else {
                    *truncated = true;
                }
            } else if i + 1 < chars.len() && chars[i + 1] == '"' {
                if field_chars < limits.max_cell_length {
                    field.push('"');
                    field_chars += 1;
                } else {
                    *truncated = true;
                }
                i += 1;
            } else {
                quoted = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' && field.is_empty() {
            quoted = true;
            i += 1;
            continue;
        }
        if ch == delimiter {
            row.push(std::mem::take(&mut field));
            field_chars = 0;
            cells += 1;
            if cells >= limits.max_cells {
                *truncated = true;
                result.push(std::mem::take(&mut row));
                return result;
            }
            i += 1;
            continue;
        }
        if ch != '\r' && ch != '\n' {
            if field_chars < limits.max_cell_length {
                field.push(ch);
                field_chars += 1;
            } else {
                *truncated = true;
            }
            i += 1;
            continue;
        }
        if ch == '\r' && i + 1 < chars.len() && chars[i + 1] == '\n' {
            i += 1;
        }
        row.push(std::mem::take(&mut field));
        field_chars = 0;
        cells += 1;
        result.push(std::mem::take(&mut row));
        if result.len() >= limits.max_rows || cells >= limits.max_cells {
            if i + 1 < chars.len() {
                *truncated = true;
            }
            return result;
        }
        i += 1;
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        result.push(row);
    }
    result
}

// ============================================================================
// HTML clipboard sanitizer & block parser (`BlocksFromHtmlSource`)
// ============================================================================

/// Sanitizes an HTML clipboard fragment into a sequence of valid [`RichBlock`]s, stripping
/// scripts, styles, iframes, event handlers, and unsafe `javascript:`/`vbscript:`/`data:` links.
#[must_use]
pub fn blocks_from_html(
    html: &str,
    base_path: Option<&Path>,
    limits: &RichMessageLimits,
    used_blocks: usize,
) -> Option<BlocksImportResult> {
    let budget = limits.max_blocks.saturating_sub(used_blocks + 1);
    if budget == 0 || html.trim().is_empty() {
        return None;
    }

    let tokens = tokenize_html(html);
    let mut ctx = HtmlImportContext {
        base_path: base_path.map(Path::to_path_buf),
        local_media: Vec::new(),
        remaining_blocks: budget,
        max_depth: limits.max_depth.max(1),
        max_table_cols: limits.max_table_cols.max(1),
        remaining_length: limits.length_limit,
        truncated: false,
    };

    let mut pos = 0usize;
    let blocks = parse_html_blocks(&tokens, &mut pos, &[], 1, &mut ctx);
    if blocks.is_empty() && ctx.local_media.is_empty() {
        return None;
    }
    Some(BlocksImportResult {
        blocks,
        local_media: ctx.local_media,
        truncated: ctx.truncated,
    })
}

#[derive(Debug, Clone)]
enum HtmlToken {
    Open {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    Close(String),
    Text(String),
}

impl HtmlToken {
    fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_ignored_container(tag: &str) -> bool {
    matches!(
        tag,
        "script" | "style" | "head" | "noscript" | "template" | "iframe" | "object" | "svg"
    )
}

fn tokenize_html(input: &str) -> Vec<HtmlToken> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut text_start = 0usize;

    let flush_text = |tokens: &mut Vec<HtmlToken>, start: usize, end: usize| {
        if end > start {
            let decoded = decode_html_entities(&input[start..end]);
            if !decoded.is_empty() {
                tokens.push(HtmlToken::Text(decoded));
            }
        }
    };

    while i < len {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // HTML comment `<!-- ... -->`
        if input[i..].starts_with("<!--") {
            flush_text(&mut tokens, text_start, i);
            if let Some(end_rel) = input[i + 4..].find("-->") {
                i = i + 4 + end_rel + 3;
            } else {
                i = len;
            }
            text_start = i;
            continue;
        }
        // Doctype / processing instruction `<!...>` or `<?...>`
        if i + 1 < len && matches!(bytes[i + 1], b'!' | b'?') {
            flush_text(&mut tokens, text_start, i);
            if let Some(gt) = input[i + 2..].find('>') {
                i = i + 2 + gt + 1;
            } else {
                i = len;
            }
            text_start = i;
            continue;
        }

        // Closing tag `</name>`
        if i + 1 < len && bytes[i + 1] == b'/' {
            if let Some(gt_rel) = input[i + 2..].find('>') {
                let raw_name = input[i + 2..i + 2 + gt_rel].trim();
                if !raw_name.is_empty()
                    && raw_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    flush_text(&mut tokens, text_start, i);
                    tokens.push(HtmlToken::Close(raw_name.to_ascii_lowercase()));
                    i = i + 2 + gt_rel + 1;
                    text_start = i;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        // Opening tag `<name attrs...>`
        if i + 1 < len && bytes[i + 1].is_ascii_alphabetic() {
            flush_text(&mut tokens, text_start, i);
            let mut j = i + 1;
            while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
                j += 1;
            }
            let name = input[i + 1..j].to_ascii_lowercase();
            let mut attrs = Vec::new();
            let mut self_closing = false;

            while j < len {
                while j < len && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= len {
                    break;
                }
                if bytes[j] == b'>' {
                    j += 1;
                    break;
                }
                if bytes[j] == b'/' && j + 1 < len && bytes[j + 1] == b'>' {
                    self_closing = true;
                    j += 2;
                    break;
                }
                let attr_start = j;
                while j < len
                    && !bytes[j].is_ascii_whitespace()
                    && !matches!(bytes[j], b'=' | b'>' | b'/')
                {
                    j += 1;
                }
                if j == attr_start {
                    j += 1;
                    continue;
                }
                let attr_name = input[attr_start..j].to_ascii_lowercase();
                while j < len && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let attr_val = if j < len && bytes[j] == b'=' {
                    j += 1;
                    while j < len && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < len && matches!(bytes[j], b'"' | b'\'') {
                        let quote = bytes[j];
                        j += 1;
                        let val_start = j;
                        while j < len && bytes[j] != quote {
                            j += 1;
                        }
                        let val = decode_html_entities(&input[val_start..j]);
                        if j < len {
                            j += 1;
                        }
                        val
                    } else {
                        let val_start = j;
                        while j < len && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>' {
                            j += 1;
                        }
                        decode_html_entities(&input[val_start..j])
                    }
                } else {
                    String::new()
                };
                // Never propagate inline JS event handlers (`onclick`, `onerror`, ...).
                if !attr_name.starts_with("on") {
                    attrs.push((attr_name, attr_val));
                }
            }

            if is_ignored_container(&name) && !self_closing {
                // Skip raw contents until matching `</name>`
                let close_needle = format!("</{name}");
                let rest_lower = input[j..].to_ascii_lowercase();
                if let Some(rel) = rest_lower.find(&close_needle) {
                    let after_close = j + rel + close_needle.len();
                    if let Some(gt) = input[after_close..].find('>') {
                        j = after_close + gt + 1;
                    } else {
                        j = len;
                    }
                } else {
                    j = len;
                }
                i = j;
                text_start = i;
                continue;
            }

            let is_self = self_closing || is_void_tag(&name);
            tokens.push(HtmlToken::Open {
                name,
                attrs,
                self_closing: is_self,
            });
            i = j;
            text_start = i;
            continue;
        }

        i += 1;
    }

    flush_text(&mut tokens, text_start, len);
    tokens
}

/// Decodes common named and numeric (`&#DEC;`, `&#xHEX;`) HTML character references.
#[must_use]
pub fn decode_html_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some(semi_rel) = input[i + 1..].find(';')
            && semi_rel > 0
            && semi_rel <= 12
        {
            let entity = &input[i + 1..i + 1 + semi_rel];
            if let Some(decoded) = resolve_html_entity(entity) {
                out.push_str(&decoded);
                i += semi_rel + 2;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn resolve_html_entity(entity: &str) -> Option<String> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        let code = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(code).map(|c| c.to_string());
    }
    if let Some(dec) = entity.strip_prefix('#') {
        let code = dec.parse::<u32>().ok()?;
        return char::from_u32(code).map(|c| c.to_string());
    }
    let resolved = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "middot" => "·",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        _ => return None,
    };
    Some(resolved.to_string())
}

struct HtmlImportContext {
    base_path: Option<PathBuf>,
    local_media: Vec<ImportedLocalMedia>,
    remaining_blocks: usize,
    max_depth: usize,
    max_table_cols: usize,
    remaining_length: usize,
    truncated: bool,
}

impl HtmlImportContext {
    fn allot_block(&mut self) -> bool {
        if self.remaining_blocks > 0 {
            self.remaining_blocks -= 1;
            true
        } else {
            self.truncated = true;
            false
        }
    }

    fn consume_text_budget(&mut self, text: &mut TextWithEntities) {
        let len = text.utf16_len();
        if len <= self.remaining_length {
            self.remaining_length -= len;
        } else {
            let keep = self.remaining_length;
            self.remaining_length = 0;
            self.truncated = true;
            *text = crate::rich_page::slice_text_with_entities(text, 0, keep);
        }
    }

    fn add_local_media(&mut self, source: &str) -> Option<u64> {
        if self.local_media.len() >= IMPORTED_MEDIA_PLACEHOLDER_LIMIT {
            return None;
        }
        let media = if source
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
        {
            let content = data_uri_image_content(source)?;
            ImportedLocalMedia {
                path: String::new(),
                content,
                external: false,
            }
        } else {
            resolve_local_media(source, self.base_path.as_deref())?
        };
        let placeholder = imported_media_placeholder_id(self.local_media.len());
        self.local_media.push(media);
        Some(placeholder)
    }
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "aside"
            | "pre"
            | "hr"
            | "table"
            | "details"
            | "footer"
            | "figure"
            | "tg-math-block"
            | "tg-map"
            | "tg-collage"
            | "tg-slideshow"
    )
}

fn parse_html_blocks(
    tokens: &[HtmlToken],
    pos: &mut usize,
    stop_tags: &[&str],
    depth: usize,
    ctx: &mut HtmlImportContext,
) -> Vec<RichBlock> {
    let mut blocks = Vec::new();
    let mut inline_buf = InlineCollector::new(false);

    let flush_inline = |inline_buf: &mut InlineCollector,
                        blocks: &mut Vec<RichBlock>,
                        ctx: &mut HtmlImportContext| {
        let mut text = inline_buf.take();
        text.trim_in_place();
        if text.is_empty() {
            return;
        }
        if !ctx.allot_block() {
            return;
        }
        ctx.consume_text_budget(&mut text);
        let mut block = RichBlock::new(RichBlockKind::Paragraph);
        block.text = RichTextNode::from_text(text);
        blocks.push(block);
    };

    while *pos < tokens.len() {
        if ctx.truncated {
            break;
        }
        match &tokens[*pos] {
            HtmlToken::Close(name) => {
                if stop_tags.contains(&name.as_str()) {
                    break;
                }
                inline_buf.close_tag(name);
                *pos += 1;
            }
            HtmlToken::Text(t) => {
                inline_buf.push_text(t);
                *pos += 1;
            }
            HtmlToken::Open {
                name,
                attrs,
                self_closing,
            } => {
                let tag = name.as_str();
                if matches!(
                    tag,
                    "html" | "body" | "article" | "main" | "section" | "div" | "figure"
                ) {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    *pos += 1;
                    if !*self_closing {
                        let nested = parse_html_blocks(tokens, pos, &[tag], depth, ctx);
                        blocks.extend(nested);
                        if matches!(tokens.get(*pos), Some(HtmlToken::Close(c)) if c == tag) {
                            *pos += 1;
                        }
                    }
                    continue;
                }

                if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let level = tag.as_bytes()[1].saturating_sub(b'0').clamp(1, 6);
                    let anchor_id = normalized_attr_id(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    let mut text = if self_c {
                        TextWithEntities::default()
                    } else {
                        collect_inline_until(tokens, pos, &[tag], false)
                    };
                    text.trim_in_place();
                    if !text.is_empty() && ctx.allot_block() {
                        ctx.consume_text_budget(&mut text);
                        let mut b = RichBlock::new(RichBlockKind::Heading);
                        b.heading_level = level;
                        b.anchor_id = anchor_id;
                        b.text = RichTextNode::from_text(text);
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "p" || tag == "footer" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let kind = if tag == "footer" {
                        RichBlockKind::Footer
                    } else {
                        RichBlockKind::Paragraph
                    };
                    let anchor_id = normalized_attr_id(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    let mut text = if self_c {
                        TextWithEntities::default()
                    } else {
                        collect_inline_until(tokens, pos, &[tag], false)
                    };
                    text.trim_in_place();
                    if !text.is_empty() && ctx.allot_block() {
                        ctx.consume_text_budget(&mut text);
                        let mut b = RichBlock::new(kind);
                        b.anchor_id = anchor_id;
                        b.text = RichTextNode::from_text(text);
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "hr" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    *pos += 1;
                    if ctx.allot_block() {
                        blocks.push(RichBlock::new(RichBlockKind::Divider));
                    }
                    continue;
                }

                if tag == "pre" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let anchor_id = normalized_attr_id(attrs);
                    let mut language = extract_language_class(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    let mut raw_code = String::new();
                    if !self_c {
                        while *pos < tokens.len() {
                            match &tokens[*pos] {
                                HtmlToken::Close(c) if c == "pre" => {
                                    *pos += 1;
                                    break;
                                }
                                HtmlToken::Open {
                                    name: inner_name,
                                    attrs: inner_attrs,
                                    ..
                                } if inner_name == "code" => {
                                    if language.is_empty() {
                                        language = extract_language_class(inner_attrs);
                                    }
                                    *pos += 1;
                                }
                                HtmlToken::Open {
                                    name: inner_name, ..
                                } if inner_name == "br" => {
                                    raw_code.push('\n');
                                    *pos += 1;
                                }
                                HtmlToken::Text(t) => {
                                    raw_code.push_str(t);
                                    *pos += 1;
                                }
                                _ => {
                                    *pos += 1;
                                }
                            }
                        }
                    }
                    if !raw_code.is_empty() && ctx.allot_block() {
                        let mut text = TextWithEntities::simple(raw_code);
                        ctx.consume_text_budget(&mut text);
                        let mut b = RichBlock::new(RichBlockKind::Code);
                        b.anchor_id = anchor_id;
                        b.language = language;
                        b.text = RichTextNode::from_text(text);
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "blockquote" || tag == "aside" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let pullquote = tag == "aside";
                    let anchor_id = normalized_attr_id(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    if !self_c && depth < ctx.max_depth && ctx.allot_block() {
                        let child_blocks = parse_html_blocks(tokens, pos, &[tag], depth + 1, ctx);
                        if matches!(tokens.get(*pos), Some(HtmlToken::Close(c)) if c == tag) {
                            *pos += 1;
                        }
                        let mut b = RichBlock::new(RichBlockKind::Quote);
                        b.pullquote = pullquote;
                        b.anchor_id = anchor_id;
                        if child_blocks.len() == 1
                            && child_blocks[0].kind == RichBlockKind::Paragraph
                        {
                            b.text = child_blocks[0].text.clone();
                        } else {
                            b.blocks = child_blocks;
                        }
                        if b.text.has_content() || !b.blocks.is_empty() {
                            blocks.push(b);
                        }
                    } else if !self_c {
                        skip_until_close(tokens, pos, tag);
                    }
                    continue;
                }

                if tag == "ul" || tag == "ol" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let list_block = parse_html_list(tokens, pos, depth, ctx);
                    if let Some(b) = list_block {
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "table" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    if let Some(table_block) = parse_html_table(tokens, pos, ctx) {
                        blocks.push(table_block);
                    }
                    continue;
                }

                if tag == "details" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let open = HtmlToken::attr(attrs, "open").is_some();
                    let anchor_id = normalized_attr_id(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    if !self_c && depth < ctx.max_depth && ctx.allot_block() {
                        let mut summary = TextWithEntities::default();
                        let mut body_blocks = Vec::new();
                        while *pos < tokens.len() {
                            match &tokens[*pos] {
                                HtmlToken::Close(c) if c == "details" => {
                                    *pos += 1;
                                    break;
                                }
                                HtmlToken::Open {
                                    name: n,
                                    self_closing: sc,
                                    ..
                                } if n == "summary" => {
                                    let sc_val = *sc;
                                    *pos += 1;
                                    if !sc_val {
                                        summary =
                                            collect_inline_until(tokens, pos, &["summary"], false);
                                        summary.trim_in_place();
                                    }
                                }
                                _ => {
                                    let nested = parse_html_blocks(
                                        tokens,
                                        pos,
                                        &["details", "summary"],
                                        depth + 1,
                                        ctx,
                                    );
                                    body_blocks.extend(nested);
                                }
                            }
                        }
                        if body_blocks.is_empty() {
                            body_blocks.push(RichBlock::new(RichBlockKind::Paragraph));
                        }
                        ctx.consume_text_budget(&mut summary);
                        let mut b = RichBlock::new(RichBlockKind::Details);
                        b.open = open;
                        b.anchor_id = anchor_id;
                        b.text = RichTextNode::from_text(summary);
                        b.blocks = body_blocks;
                        blocks.push(b);
                    } else if !self_c {
                        skip_until_close(tokens, pos, "details");
                    }
                    continue;
                }

                if tag == "tg-math-block" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let anchor_id = normalized_attr_id(attrs);
                    let self_c = *self_closing;
                    *pos += 1;
                    let formula = if self_c {
                        String::new()
                    } else {
                        collect_plain_text_until(tokens, pos, "tg-math-block")
                    };
                    let trimmed = formula.trim();
                    if !trimmed.is_empty() && ctx.allot_block() {
                        let mut b = RichBlock::math(trimmed);
                        b.anchor_id = anchor_id;
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "tg-map" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let lat = HtmlToken::attr(attrs, "lat").and_then(|v| v.parse::<f64>().ok());
                    let lon = HtmlToken::attr(attrs, "long")
                        .or_else(|| HtmlToken::attr(attrs, "lon"))
                        .and_then(|v| v.parse::<f64>().ok());
                    let zoom = HtmlToken::attr(attrs, "zoom")
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(14);
                    let self_c = *self_closing;
                    *pos += 1;
                    if !self_c {
                        skip_until_close(tokens, pos, "tg-map");
                    }
                    if let (Some(lat), Some(lon)) = (lat, lon)
                        && lat.is_finite()
                        && lon.is_finite()
                        && (-90.0..=90.0).contains(&lat)
                        && (-180.0..=180.0).contains(&lon)
                        && ctx.allot_block()
                    {
                        let mut b = RichBlock::new(RichBlockKind::Map);
                        b.latitude = lat;
                        b.longitude = lon;
                        b.zoom = zoom;
                        blocks.push(b);
                    }
                    continue;
                }

                if tag == "img" || tag == "video" || tag == "audio" {
                    let kind = match tag {
                        "img" => RichBlockKind::Photo,
                        "audio" => RichBlockKind::Audio,
                        _ => RichBlockKind::Video,
                    };
                    let tg_src = HtmlToken::attr(attrs, "data-tg-src").unwrap_or("");
                    let src = HtmlToken::attr(attrs, "src").unwrap_or("");
                    let width = HtmlToken::attr(attrs, "width")
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(0);
                    let height = HtmlToken::attr(attrs, "height")
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(0);
                    let spoiler = HtmlToken::attr(attrs, "tg-spoiler").is_some();
                    let autoplay = HtmlToken::attr(attrs, "autoplay").is_some();
                    let loop_video = HtmlToken::attr(attrs, "loop").is_some();
                    let self_c = *self_closing;
                    *pos += 1;
                    if !self_c {
                        skip_until_close(tokens, pos, tag);
                    }
                    let tg_id = match kind {
                        RichBlockKind::Photo => media_id_from_identity(tg_src, "photo"),
                        RichBlockKind::Audio => media_id_from_identity(tg_src, "audio"),
                        _ => media_id_from_identity(tg_src, "video"),
                    };
                    let resolved_id = tg_id.or_else(|| ctx.add_local_media(src));
                    if let Some(id) = resolved_id {
                        flush_inline(&mut inline_buf, &mut blocks, ctx);
                        if ctx.allot_block() {
                            let mut b = RichBlock::new(kind);
                            b.width = width;
                            b.height = height;
                            b.spoiler = spoiler;
                            b.autoplay = autoplay;
                            b.loop_video = loop_video;
                            if kind == RichBlockKind::Photo {
                                b.photo_id = id;
                            } else {
                                b.document_id = id;
                            }
                            blocks.push(b);
                        }
                    }
                    continue;
                }

                if tag == "tg-collage" || tag == "tg-slideshow" {
                    flush_inline(&mut inline_buf, &mut blocks, ctx);
                    let intent = if tag == "tg-slideshow" {
                        GroupedMediaIntent::Slideshow
                    } else {
                        GroupedMediaIntent::Collage
                    };
                    let self_c = *self_closing;
                    *pos += 1;
                    if !self_c && ctx.allot_block() {
                        let nested = parse_html_blocks(tokens, pos, &[tag], depth + 1, ctx);
                        if matches!(tokens.get(*pos), Some(HtmlToken::Close(c)) if c == tag) {
                            *pos += 1;
                        }
                        let mut b = RichBlock::new(RichBlockKind::GroupedMedia);
                        b.media_intent = intent;
                        for child in nested {
                            if matches!(child.kind, RichBlockKind::Photo | RichBlockKind::Video) {
                                b.media_items.push(GroupedMediaItem {
                                    kind: child.kind,
                                    photo_id: child.photo_id,
                                    document_id: child.document_id,
                                    width: child.width,
                                    height: child.height,
                                    autoplay: child.autoplay,
                                    loop_video: child.loop_video,
                                    spoiler: child.spoiler,
                                });
                            }
                        }
                        if !b.media_items.is_empty() {
                            blocks.push(b);
                        }
                    }
                    continue;
                }

                // Inline HTML element inside a paragraph run
                if tag == "tg-math" {
                    let self_c = *self_closing;
                    *pos += 1;
                    let tex = if self_c {
                        String::new()
                    } else {
                        collect_plain_text_until(tokens, pos, "tg-math")
                    };
                    if !tex.trim().is_empty() {
                        let inline_math = formula_source_to_rich_text(tex.trim());
                        inline_buf.push_rich(&inline_math);
                    }
                    continue;
                }

                inline_buf.open_tag(tag, attrs, *self_closing);
                *pos += 1;
            }
        }
    }

    flush_inline(&mut inline_buf, &mut blocks, ctx);
    blocks
}

fn parse_html_list(
    tokens: &[HtmlToken],
    pos: &mut usize,
    depth: usize,
    ctx: &mut HtmlImportContext,
) -> Option<RichBlock> {
    let (list_tag, ordered, reversed, start, list_type, anchor_id, self_c) =
        match tokens.get(*pos)? {
            HtmlToken::Open {
                name,
                attrs,
                self_closing,
            } => {
                let ordered = name == "ol";
                let reversed = HtmlToken::attr(attrs, "reversed").is_some();
                let start = HtmlToken::attr(attrs, "start").and_then(|v| v.parse::<i32>().ok());
                let list_type = HtmlToken::attr(attrs, "type")
                    .filter(|v| matches!(*v, "1" | "a" | "A" | "i" | "I"))
                    .map(ToOwned::to_owned);
                (
                    name.clone(),
                    ordered,
                    reversed,
                    start,
                    list_type,
                    normalized_attr_id(attrs),
                    *self_closing,
                )
            }
            _ => return None,
        };
    *pos += 1;
    if self_c {
        return None;
    }
    if depth > ctx.max_depth || !ctx.allot_block() {
        skip_until_close(tokens, pos, &list_tag);
        return None;
    }

    let mut list = RichBlock::new(RichBlockKind::List);
    list.anchor_id = anchor_id;
    list.list_kind = if ordered {
        RichListKind::Ordered
    } else {
        RichListKind::Bullet
    };
    if ordered {
        list.ordered_list = OrderedListData {
            reversed,
            start,
            list_type,
        };
    }

    while *pos < tokens.len() && !ctx.truncated {
        match &tokens[*pos] {
            HtmlToken::Close(c) if c == &list_tag => {
                *pos += 1;
                break;
            }
            HtmlToken::Open {
                name,
                attrs,
                self_closing,
            } if name == "li" => {
                if !ctx.allot_block() {
                    skip_until_close(tokens, pos, "li");
                    break;
                }
                let item_anchor = normalized_attr_id(attrs);
                let item_value =
                    HtmlToken::attr(attrs, "value").and_then(|v| v.parse::<i32>().ok());
                let li_self = *self_closing;
                *pos += 1;

                let mut item = RichListItem {
                    anchor_id: item_anchor,
                    number: OrderedListItemData {
                        value: item_value,
                        ..OrderedListItemData::default()
                    },
                    ..RichListItem::default()
                };

                if !li_self {
                    let mut inline = InlineCollector::new(false);
                    while *pos < tokens.len() && !ctx.truncated {
                        match &tokens[*pos] {
                            HtmlToken::Close(c) if c == "li" => {
                                *pos += 1;
                                break;
                            }
                            HtmlToken::Close(c) if c == &list_tag => {
                                break;
                            }
                            HtmlToken::Open { name: n, .. } if n == "li" => {
                                break;
                            }
                            HtmlToken::Open {
                                name: n,
                                attrs: input_attrs,
                                ..
                            } if n == "input"
                                && HtmlToken::attr(input_attrs, "type")
                                    .is_some_and(|t| t.eq_ignore_ascii_case("checkbox")) =>
                            {
                                item.task_state =
                                    if HtmlToken::attr(input_attrs, "checked").is_some() {
                                        RichTaskState::Checked
                                    } else {
                                        RichTaskState::Unchecked
                                    };
                                *pos += 1;
                            }
                            HtmlToken::Open { name: n, .. } if n == "ul" || n == "ol" => {
                                if let Some(nested_list) =
                                    parse_html_list(tokens, pos, depth + 1, ctx)
                                {
                                    item.blocks.push(nested_list);
                                }
                            }
                            HtmlToken::Open { name: n, .. } if is_block_tag(n) => {
                                let nested = parse_html_blocks(
                                    tokens,
                                    pos,
                                    &["li", "ul", "ol"],
                                    depth + 1,
                                    ctx,
                                );
                                item.blocks.extend(nested);
                            }
                            HtmlToken::Open {
                                name: n,
                                attrs: a,
                                self_closing: sc,
                            } => {
                                inline.open_tag(n, a, *sc);
                                *pos += 1;
                            }
                            HtmlToken::Close(c) => {
                                inline.close_tag(c);
                                *pos += 1;
                            }
                            HtmlToken::Text(t) => {
                                inline.push_text(t);
                                *pos += 1;
                            }
                        }
                    }
                    let mut text = inline.take();
                    text.trim_in_place();
                    ctx.consume_text_budget(&mut text);
                    item.text = RichTextNode::from_text(text);
                }

                if item.text.has_content()
                    || !item.blocks.is_empty()
                    || item.task_state != RichTaskState::None
                {
                    list.list_items.push(item);
                }
            }
            _ => {
                *pos += 1;
            }
        }
    }

    if list.list_items.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn parse_html_table(
    tokens: &[HtmlToken],
    pos: &mut usize,
    ctx: &mut HtmlImportContext,
) -> Option<RichBlock> {
    let (anchor_id, self_c) = match tokens.get(*pos)? {
        HtmlToken::Open {
            attrs,
            self_closing,
            ..
        } => (normalized_attr_id(attrs), *self_closing),
        _ => return None,
    };
    *pos += 1;
    if self_c {
        return None;
    }
    if !ctx.allot_block() {
        skip_until_close(tokens, pos, "table");
        return None;
    }

    let mut table = RichBlock::new(RichBlockKind::Table);
    table.anchor_id = anchor_id;
    table.bordered = true;

    while *pos < tokens.len() && !ctx.truncated {
        match &tokens[*pos] {
            HtmlToken::Close(c) if c == "table" => {
                *pos += 1;
                break;
            }
            HtmlToken::Open {
                name, self_closing, ..
            } if name == "caption" => {
                let sc = *self_closing;
                *pos += 1;
                if !sc {
                    let mut cap = collect_inline_until(tokens, pos, &["caption"], false);
                    cap.trim_in_place();
                    degrade_block_only_entities(&mut cap);
                    ctx.consume_text_budget(&mut cap);
                    table.text = RichTextNode::from_text(cap);
                }
            }
            HtmlToken::Open {
                name, self_closing, ..
            } if name == "tr" => {
                let sc = *self_closing;
                *pos += 1;
                if sc {
                    continue;
                }
                if !ctx.allot_block() {
                    skip_until_close(tokens, pos, "tr");
                    break;
                }
                let mut row = RichTableRow::default();
                while *pos < tokens.len() {
                    match &tokens[*pos] {
                        HtmlToken::Close(c) if c == "tr" => {
                            *pos += 1;
                            break;
                        }
                        HtmlToken::Close(c) if c == "table" => {
                            break;
                        }
                        HtmlToken::Open { name: n, .. } if n == "tr" => {
                            break;
                        }
                        HtmlToken::Open {
                            name: cell_tag,
                            attrs,
                            self_closing: cell_sc,
                        } if cell_tag == "td" || cell_tag == "th" => {
                            let header = cell_tag == "th";
                            let colspan = HtmlToken::attr(attrs, "colspan")
                                .and_then(|v| v.parse::<i32>().ok())
                                .unwrap_or(1)
                                .max(1);
                            let rowspan = HtmlToken::attr(attrs, "rowspan")
                                .and_then(|v| v.parse::<i32>().ok())
                                .unwrap_or(1)
                                .max(1);
                            let alignment = parse_cell_alignment(attrs);
                            let close_tag = cell_tag.clone();
                            let csc = *cell_sc;
                            *pos += 1;
                            let mut cell_text = if csc {
                                TextWithEntities::default()
                            } else {
                                collect_inline_until(
                                    tokens,
                                    pos,
                                    &[close_tag.as_str(), "tr", "table"],
                                    false,
                                )
                            };
                            cell_text.trim_in_place();
                            degrade_block_only_entities(&mut cell_text);
                            if cell_text.utf16_len() > MAX_CELL_LENGTH {
                                cell_text = crate::rich_page::slice_text_with_entities(
                                    &cell_text,
                                    0,
                                    MAX_CELL_LENGTH,
                                );
                                ctx.truncated = true;
                            }
                            ctx.consume_text_budget(&mut cell_text);
                            if row.cells.len() < ctx.max_table_cols {
                                row.cells.push(RichTableCell {
                                    text: RichTextNode::from_text(cell_text),
                                    colspan,
                                    rowspan,
                                    header,
                                    alignment,
                                    ..RichTableCell::default()
                                });
                            } else {
                                ctx.truncated = true;
                            }
                        }
                        _ => {
                            *pos += 1;
                        }
                    }
                }
                if !row.cells.is_empty() {
                    table.table_rows.push(row);
                }
            }
            _ => {
                *pos += 1;
            }
        }
    }

    if table.table_rows.is_empty() {
        None
    } else {
        Some(table)
    }
}

fn parse_cell_alignment(attrs: &[(String, String)]) -> RichTableAlignment {
    if let Some(align) = HtmlToken::attr(attrs, "align") {
        let lower = align.trim().to_ascii_lowercase();
        if lower == "center" {
            return RichTableAlignment::Center;
        } else if lower == "right" {
            return RichTableAlignment::Right;
        }
    }
    if let Some(style) = HtmlToken::attr(attrs, "style") {
        let lower = style.to_ascii_lowercase();
        if lower.contains("text-align:center") || lower.contains("text-align: center") {
            return RichTableAlignment::Center;
        }
        if lower.contains("text-align:right") || lower.contains("text-align: right") {
            return RichTableAlignment::Right;
        }
    }
    RichTableAlignment::Left
}

fn collect_inline_until(
    tokens: &[HtmlToken],
    pos: &mut usize,
    stop_tags: &[&str],
    preserve_ws: bool,
) -> TextWithEntities {
    let mut collector = InlineCollector::new(preserve_ws);
    while *pos < tokens.len() {
        match &tokens[*pos] {
            HtmlToken::Close(name) => {
                if stop_tags.contains(&name.as_str()) {
                    if stop_tags.first() == Some(&name.as_str()) {
                        *pos += 1;
                    }
                    break;
                }
                collector.close_tag(name);
                *pos += 1;
            }
            HtmlToken::Text(t) => {
                collector.push_text(t);
                *pos += 1;
            }
            HtmlToken::Open {
                name,
                attrs,
                self_closing,
            } => {
                if name == "tg-math" {
                    let sc = *self_closing;
                    *pos += 1;
                    let tex = if sc {
                        String::new()
                    } else {
                        collect_plain_text_until(tokens, pos, "tg-math")
                    };
                    if !tex.trim().is_empty() {
                        collector.push_rich(&formula_source_to_rich_text(tex.trim()));
                    }
                    continue;
                }
                collector.open_tag(name, attrs, *self_closing);
                *pos += 1;
            }
        }
    }
    collector.take()
}

fn collect_plain_text_until(tokens: &[HtmlToken], pos: &mut usize, close_tag: &str) -> String {
    let mut out = String::new();
    while *pos < tokens.len() {
        match &tokens[*pos] {
            HtmlToken::Close(c) if c == close_tag => {
                *pos += 1;
                break;
            }
            HtmlToken::Text(t) => {
                out.push_str(t);
                *pos += 1;
            }
            HtmlToken::Open { name, .. } if name == "br" => {
                out.push('\n');
                *pos += 1;
            }
            _ => {
                *pos += 1;
            }
        }
    }
    out
}

fn skip_until_close(tokens: &[HtmlToken], pos: &mut usize, close_tag: &str) {
    let mut depth = 1usize;
    while *pos < tokens.len() {
        match &tokens[*pos] {
            HtmlToken::Open {
                name,
                self_closing: false,
                ..
            } if name == close_tag => {
                depth += 1;
            }
            HtmlToken::Close(name) if name == close_tag => {
                depth -= 1;
                *pos += 1;
                if depth == 0 {
                    return;
                }
                continue;
            }
            _ => {}
        }
        *pos += 1;
    }
}

struct OpenInlineStyle {
    tag: String,
    entities: Vec<(EntityType, String)>,
    start_utf16: usize,
}

struct InlineCollector {
    text: TextWithEntities,
    stack: Vec<OpenInlineStyle>,
    preserve_ws: bool,
    pending_space: bool,
}

impl InlineCollector {
    fn new(preserve_ws: bool) -> Self {
        Self {
            text: TextWithEntities::default(),
            stack: Vec::new(),
            preserve_ws,
            pending_space: false,
        }
    }

    fn push_text(&mut self, s: &str) {
        if self.preserve_ws {
            self.text.push_str(s);
            return;
        }
        for ch in s.chars() {
            if ch.is_ascii_whitespace() {
                if !self.text.is_empty() && !self.text.text.ends_with('\n') {
                    self.pending_space = true;
                }
            } else {
                if self.pending_space {
                    self.text.push_str(" ");
                    self.pending_space = false;
                }
                self.text.text.push(ch);
            }
        }
    }

    fn push_rich(&mut self, rich: &TextWithEntities) {
        if self.pending_space {
            self.text.push_str(" ");
            self.pending_space = false;
        }
        self.text.append(rich.clone());
    }

    fn open_tag(&mut self, tag: &str, attrs: &[(String, String)], self_closing: bool) {
        if tag == "br" {
            self.pending_space = false;
            self.text.push_str("\n");
            return;
        }
        if self_closing {
            return;
        }
        let mut entities = Vec::new();
        match tag {
            "b" | "strong" => entities.push((EntityType::Bold, String::new())),
            "i" | "em" => entities.push((EntityType::Italic, String::new())),
            "u" | "ins" => entities.push((EntityType::Underline, String::new())),
            "s" | "strike" | "del" => entities.push((EntityType::StrikeOut, String::new())),
            "code" | "tt" => entities.push((EntityType::Code, String::new())),
            "sub" => entities.push((EntityType::Subscript, String::new())),
            "sup" => entities.push((EntityType::Superscript, String::new())),
            "mark" => entities.push((EntityType::Marked, String::new())),
            "tg-spoiler" => entities.push((EntityType::Spoiler, String::new())),
            "a" => {
                if let Some(href) = HtmlToken::attr(attrs, "href")
                    && let Some(safe) = safe_export_url(href)
                {
                    entities.push((EntityType::CustomUrl, safe));
                }
            }
            "span" => {
                if let Some(cls) = HtmlToken::attr(attrs, "class")
                    && cls.split_ascii_whitespace().any(|c| c == "tg-spoiler")
                {
                    entities.push((EntityType::Spoiler, String::new()));
                }
                if let Some(style) = HtmlToken::attr(attrs, "style") {
                    let lower = style.to_ascii_lowercase();
                    if lower.contains("font-weight:bold")
                        || lower.contains("font-weight: bold")
                        || lower.contains("font-weight:700")
                        || lower.contains("font-weight: 700")
                    {
                        entities.push((EntityType::Bold, String::new()));
                    }
                    if lower.contains("font-style:italic") || lower.contains("font-style: italic") {
                        entities.push((EntityType::Italic, String::new()));
                    }
                    if lower.contains("text-decoration:underline")
                        || lower.contains("text-decoration: underline")
                    {
                        entities.push((EntityType::Underline, String::new()));
                    }
                    if lower.contains("line-through") {
                        entities.push((EntityType::StrikeOut, String::new()));
                    }
                }
            }
            _ => {}
        }

        if !entities.is_empty() {
            if self.pending_space {
                self.text.push_str(" ");
                self.pending_space = false;
            }
            self.stack.push(OpenInlineStyle {
                tag: tag.to_string(),
                entities,
                start_utf16: self.text.utf16_len(),
            });
        }
    }

    fn close_tag(&mut self, tag: &str) {
        if let Some(idx) = self.stack.iter().rposition(|entry| entry.tag == tag) {
            let entry = self.stack.remove(idx);
            let end_utf16 = self.text.utf16_len();
            if end_utf16 > entry.start_utf16 {
                let len = end_utf16 - entry.start_utf16;
                for (kind, data) in entry.entities {
                    self.text.entities.push(EntityInText::with_data(
                        kind,
                        entry.start_utf16,
                        len,
                        data,
                    ));
                }
            }
        }
    }

    fn take(&mut self) -> TextWithEntities {
        // Close any unclosed inline tags at end of block.
        while let Some(entry) = self.stack.pop() {
            let end_utf16 = self.text.utf16_len();
            if end_utf16 > entry.start_utf16 {
                let len = end_utf16 - entry.start_utf16;
                for (kind, data) in entry.entities {
                    self.text.entities.push(EntityInText::with_data(
                        kind,
                        entry.start_utf16,
                        len,
                        data,
                    ));
                }
            }
        }
        self.pending_space = false;
        let mut text = std::mem::take(&mut self.text);
        text.sort_entities();
        text
    }
}

fn normalized_attr_id(attrs: &[(String, String)]) -> String {
    HtmlToken::attr(attrs, "id")
        .map(normalize_fragment_id)
        .unwrap_or_default()
}

fn extract_language_class(attrs: &[(String, String)]) -> String {
    let Some(cls) = HtmlToken::attr(attrs, "class") else {
        return String::new();
    };
    for token in cls.split_ascii_whitespace() {
        if let Some(lang) = token.strip_prefix("language-")
            && !lang.is_empty()
        {
            return lang.to_string();
        }
    }
    String::new()
}

fn media_id_from_identity(identity: &str, kind: &str) -> Option<u64> {
    let prefix = format!("tg://{kind}?");
    if identity.len() < prefix.len() || !identity[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return None;
    }
    for pair in identity[prefix.len()..].split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == "id"
        {
            return v.parse::<u64>().ok().filter(|&id| id != 0);
        }
    }
    None
}

// ============================================================================
// Markdown clipboard import (`BlocksFromMarkdown` in iv_editor_clipboard_import.cpp)
// ============================================================================

/// Imports structured Markdown clipboard text into [`RichBlock`]s, returning
/// `(BlocksImportResult, beyond_compose_field)` when Markdown syntax is present
/// (`BlocksFromMarkdown`).
#[must_use]
pub fn blocks_from_markdown(
    text: &str,
    limits: &RichMessageLimits,
    used_blocks: usize,
) -> Option<(BlocksImportResult, bool)> {
    let budget = limits.max_blocks.saturating_sub(used_blocks + 1);
    let u16_len = text.encode_utf16().count();
    if text.is_empty()
        || text.len() > MAX_MARKDOWN_IMPORT_LENGTH
        || u16_len > limits.length_limit
        || budget == 0
    {
        return None;
    }
    let mut builder = MarkdownBlocksBuilder {
        budget,
        truncated: false,
        marked: false,
        beyond_compose_field: false,
    };
    let mut blocks = Vec::new();
    parse_markdown_blocks(&mut builder, &mut blocks, text, 0);
    if blocks.is_empty() || !builder.marked {
        return None;
    }
    Some((
        BlocksImportResult {
            blocks,
            local_media: Vec::new(),
            truncated: builder.truncated,
        },
        builder.beyond_compose_field,
    ))
}

struct MarkdownBlocksBuilder {
    budget: usize,
    truncated: bool,
    marked: bool,
    beyond_compose_field: bool,
}

impl MarkdownBlocksBuilder {
    fn allot(&mut self) -> bool {
        if self.budget > 0 {
            self.budget -= 1;
            true
        } else {
            self.truncated = true;
            false
        }
    }
}

#[derive(Clone, Default)]
struct InlineMarkStack {
    kinds: Vec<(EntityType, String)>,
}

impl InlineMarkStack {
    fn with(&self, kind: EntityType, data: String) -> Self {
        let mut next = self.clone();
        if !next.kinds.iter().any(|(k, d)| *k == kind && *d == data) {
            next.kinds.push((kind, data));
        }
        next
    }
}

struct MarkdownInlineState {
    text: TextWithEntities,
    marked: bool,
    beyond_compose_field: bool,
}

impl MarkdownInlineState {
    fn append_run(&mut self, slice: &str, stack: &InlineMarkStack) {
        if slice.is_empty() {
            return;
        }
        let offset = self.text.utf16_len();
        self.text.push_str(slice);
        let len = slice.encode_utf16().count();
        for (kind, data) in &stack.kinds {
            self.text
                .entities
                .push(EntityInText::with_data(*kind, offset, len, data.clone()));
        }
    }
}

const fn markdown_escapable(ch: char) -> bool {
    matches!(
        ch,
        '\\' | '*' | '_' | '~' | '|' | '`' | '[' | ']' | '(' | ')' | '#' | '!' | '-' | '.' | '>'
    )
}

fn markdown_link_url(inside: &str) -> Option<String> {
    let mut url = inside.trim();
    if let Some(space) = url.find(' ') {
        url = &url[..space];
    }
    if url.is_empty() {
        return None;
    }
    let normalized = if url.starts_with("www.") {
        format!("https://{url}")
    } else {
        url.to_string()
    };
    safe_export_url(&normalized)
}

fn find_markdown_link_close(chars: &[char], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' {
            i += 2;
            continue;
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn slice_chars(chars: &[char], from: usize, to: usize) -> String {
    chars[from..to].iter().collect()
}

fn find_markdown_closer(
    chars: &[char],
    from: usize,
    marker: &[char],
    word_bounded: bool,
) -> Option<usize> {
    let mlen = marker.len();
    for i in from..=chars.len().saturating_sub(mlen) {
        if &chars[i..i + mlen] != marker || chars[i - 1].is_whitespace() {
            continue;
        }
        if word_bounded && i + mlen < chars.len() && chars[i + mlen].is_alphanumeric() {
            continue;
        }
        return Some(i);
    }
    None
}

fn parse_markdown_inline(state: &mut MarkdownInlineState, input: &str, stack: &InlineMarkStack) {
    struct MarkerDef {
        marker: &'static [char],
        first: EntityType,
        second: Option<EntityType>,
        word_bounded: bool,
        beyond_compose_field: bool,
    }
    const MARKERS: &[MarkerDef] = &[
        MarkerDef {
            marker: &['*', '*', '*'],
            first: EntityType::Bold,
            second: Some(EntityType::Italic),
            word_bounded: false,
            beyond_compose_field: true,
        },
        MarkerDef {
            marker: &['_', '_', '_'],
            first: EntityType::Bold,
            second: Some(EntityType::Italic),
            word_bounded: true,
            beyond_compose_field: true,
        },
        MarkerDef {
            marker: &['*', '*'],
            first: EntityType::Bold,
            second: None,
            word_bounded: false,
            beyond_compose_field: false,
        },
        MarkerDef {
            marker: &['_', '_'],
            first: EntityType::Italic,
            second: None,
            word_bounded: true,
            beyond_compose_field: false,
        },
        MarkerDef {
            marker: &['~', '~'],
            first: EntityType::StrikeOut,
            second: None,
            word_bounded: false,
            beyond_compose_field: false,
        },
        MarkerDef {
            marker: &['|', '|'],
            first: EntityType::Spoiler,
            second: None,
            word_bounded: false,
            beyond_compose_field: false,
        },
        MarkerDef {
            marker: &['*'],
            first: EntityType::Italic,
            second: None,
            word_bounded: true,
            beyond_compose_field: true,
        },
        MarkerDef {
            marker: &['_'],
            first: EntityType::Italic,
            second: None,
            word_bounded: true,
            beyond_compose_field: true,
        },
    ];

    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut literal_from = 0usize;
    let mut i = 0usize;

    while i < len {
        let ch = chars[i];
        if ch == '\\' && i + 1 < len && markdown_escapable(chars[i + 1]) {
            state.append_run(&slice_chars(&chars, literal_from, i), stack);
            state.append_run(&chars[i + 1].to_string(), stack);
            state.marked = true;
            state.beyond_compose_field = true;
            i += 2;
            literal_from = i;
            continue;
        }
        if ch == '`'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '`')
            && rel > 0
        {
            let closer = i + 1 + rel;
            state.append_run(&slice_chars(&chars, literal_from, i), stack);
            let code_stack = stack.with(EntityType::Code, String::new());
            state.append_run(&slice_chars(&chars, i + 1, closer), &code_stack);
            state.marked = true;
            i = closer + 1;
            literal_from = i;
            continue;
        }
        if ch == '<'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '>')
            && rel > 0
        {
            let closer = i + 1 + rel;
            let raw_inside = slice_chars(&chars, i + 1, closer);
            if let Some(url) = markdown_link_url(&raw_inside) {
                state.append_run(&slice_chars(&chars, literal_from, i), stack);
                let link_stack = stack.with(EntityType::CustomUrl, url);
                state.append_run(&raw_inside, &link_stack);
                state.marked = true;
                state.beyond_compose_field = true;
                i = closer + 1;
                literal_from = i;
                continue;
            }
        }
        if ch == '[' || (ch == '!' && i + 1 < len && chars[i + 1] == '[') {
            let open = if ch == '!' { i + 1 } else { i };
            let close_bracket =
                (open + 1..len.saturating_sub(1)).find(|&k| chars[k] == ']' && chars[k + 1] == '(');
            if let Some(cb) = close_bracket
                && let Some(cp) = find_markdown_link_close(&chars, cb + 2)
                && cp > cb + 2
            {
                let url_raw = slice_chars(&chars, cb + 2, cp);
                let inner = slice_chars(&chars, open + 1, cb);
                if let Some(url) = markdown_link_url(&url_raw)
                    && !inner.is_empty()
                {
                    state.append_run(&slice_chars(&chars, literal_from, i), stack);
                    let link_stack = stack.with(EntityType::CustomUrl, url);
                    parse_markdown_inline(state, &inner, &link_stack);
                    state.marked = true;
                    state.beyond_compose_field = true;
                    i = cp + 1;
                    literal_from = i;
                    continue;
                }
            }
        }

        let mut matched = false;
        for def in MARKERS {
            let mlen = def.marker.len();
            if i + mlen > len || &chars[i..i + mlen] != def.marker {
                continue;
            }
            if def.word_bounded && i > 0 && chars[i - 1].is_alphanumeric() {
                continue;
            }
            let content_from = i + mlen;
            if content_from >= len || chars[content_from].is_whitespace() {
                continue;
            }
            let Some(closer) =
                find_markdown_closer(&chars, content_from + 1, def.marker, def.word_bounded)
            else {
                continue;
            };
            let mut next_stack = stack.with(def.first, String::new());
            if let Some(second) = def.second {
                next_stack = next_stack.with(second, String::new());
            }
            state.append_run(&slice_chars(&chars, literal_from, i), stack);
            let inner = slice_chars(&chars, content_from, closer);
            parse_markdown_inline(state, &inner, &next_stack);
            state.marked = true;
            if def.beyond_compose_field {
                state.beyond_compose_field = true;
            }
            i = closer + mlen;
            literal_from = i;
            matched = true;
            break;
        }
        if matched {
            continue;
        }
        i += 1;
    }

    state.append_run(&slice_chars(&chars, literal_from, len), stack);
}

fn markdown_inline_text(line: &str, builder: &mut MarkdownBlocksBuilder) -> TextWithEntities {
    let mut state = MarkdownInlineState {
        text: TextWithEntities::default(),
        marked: false,
        beyond_compose_field: false,
    };
    parse_markdown_inline(&mut state, line, &InlineMarkStack::default());
    builder.marked = builder.marked || state.marked;
    builder.beyond_compose_field = builder.beyond_compose_field || state.beyond_compose_field;
    state.text.sort_entities();
    state.text
}

struct MarkdownListMarker {
    indent: usize,
    ordered: bool,
    number: i32,
    task: RichTaskState,
    content_from: usize,
}

fn parse_markdown_list_marker(line: &str) -> Option<MarkdownListMarker> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut indent = 0usize;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        indent += if bytes[i] == b'\t' { 4 } else { 1 };
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut ordered = false;
    let mut number = 1i32;
    if matches!(bytes[i], b'-' | b'*' | b'+') {
        i += 1;
    } else if bytes[i].is_ascii_digit() {
        let mut digits = 0usize;
        let mut val = 0i32;
        while i < bytes.len() && bytes[i].is_ascii_digit() && digits < 9 {
            val = val * 10 + i32::from(bytes[i] - b'0');
            i += 1;
            digits += 1;
        }
        if i >= bytes.len() || !matches!(bytes[i], b'.' | b')') {
            return None;
        }
        i += 1;
        ordered = true;
        number = val;
    } else {
        return None;
    }
    if i >= bytes.len() || bytes[i] != b' ' {
        return None;
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let mut task = RichTaskState::None;
    if i + 2 < bytes.len()
        && bytes[i] == b'['
        && bytes[i + 2] == b']'
        && (i + 3 == bytes.len() || bytes[i + 3] == b' ')
    {
        match bytes[i + 1] {
            b' ' => {
                task = RichTaskState::Unchecked;
                i += 3;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
            }
            b'x' | b'X' => {
                task = RichTaskState::Checked;
                i += 3;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
            }
            _ => {}
        }
    }
    Some(MarkdownListMarker {
        indent,
        ordered,
        number,
        task,
        content_from: i,
    })
}

fn markdown_divider_line(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    let ch = bytes[0];
    if !matches!(ch, b'-' | b'*' | b'_') {
        return false;
    }
    let mut count = 0usize;
    for &b in bytes {
        if b == ch {
            count += 1;
        } else if b != b' ' {
            return false;
        }
    }
    count >= 3
}

fn markdown_setext_h1_line(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.bytes().all(|b| b == b'=')
}

fn markdown_table_separator_line(trimmed: &str) -> bool {
    trimmed.contains('-')
        && trimmed.contains('|')
        && trimmed
            .bytes()
            .all(|b| matches!(b, b'-' | b'|' | b':' | b' '))
}

fn split_markdown_table_row(line: &str) -> Vec<String> {
    let mut trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('|') {
        trimmed = rest;
    }
    if trimmed.ends_with('|') && !trimmed.ends_with("\\|") {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    let bytes = trimmed.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    for i in 0..=bytes.len() {
        if i == bytes.len() || (bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\')) {
            out.push(trimmed[from..i].trim().to_string());
            from = i + 1;
        }
    }
    out
}

fn markdown_cell_alignment(separator: &str) -> RichTableAlignment {
    let left = separator.starts_with(':');
    let right = separator.ends_with(':');
    if left && right {
        RichTableAlignment::Center
    } else if right {
        RichTableAlignment::Right
    } else {
        RichTableAlignment::Left
    }
}

fn parse_markdown_blocks(
    builder: &mut MarkdownBlocksBuilder,
    out: &mut Vec<RichBlock>,
    text: &str,
    depth: usize,
) {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let count = lines.len();
    let mut index = 0usize;

    while index < count {
        if builder.truncated {
            return;
        }
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        // Fenced code block
        if let Some(after_fence) = trimmed.strip_prefix("```") {
            let language = after_fence.trim().to_string();
            let mut content = Vec::new();
            index += 1;
            while index < count && !lines[index].trim().starts_with("```") {
                content.push(lines[index]);
                index += 1;
            }
            if index < count {
                index += 1;
            }
            if !builder.allot() {
                return;
            }
            out.push(RichBlock::code(language, content.join("\n")));
            builder.marked = true;
            continue;
        }

        // ATX heading `#`..`######`
        let heading_level = trimmed.bytes().take(6).take_while(|&b| b == b'#').count();
        if heading_level > 0 && trimmed.as_bytes().get(heading_level) == Some(&b' ') {
            if !builder.allot() {
                return;
            }
            let content = trimmed[heading_level + 1..].trim();
            let mut b = RichBlock::new(RichBlockKind::Heading);
            b.heading_level = heading_level as u8;
            b.text = RichTextNode::from_text(markdown_inline_text(content, builder));
            out.push(b);
            builder.marked = true;
            index += 1;
            continue;
        }

        // Divider
        if markdown_divider_line(trimmed) {
            if !builder.allot() {
                return;
            }
            out.push(RichBlock::new(RichBlockKind::Divider));
            builder.marked = true;
            index += 1;
            continue;
        }

        // Blockquote
        if trimmed.starts_with('>') && depth < MAX_MARKDOWN_QUOTE_DEPTH {
            let mut inner = Vec::new();
            while index < count {
                let q = lines[index].trim();
                let Some(rest) = q.strip_prefix('>') else {
                    break;
                };
                inner.push(rest.strip_prefix(' ').unwrap_or(rest));
                index += 1;
            }
            if !builder.allot() {
                return;
            }
            let mut nested = Vec::new();
            parse_markdown_blocks(builder, &mut nested, &inner.join("\n"), depth + 1);
            let mut b = RichBlock::new(RichBlockKind::Quote);
            if nested.len() == 1 && nested[0].kind == RichBlockKind::Paragraph {
                b.text = nested[0].text.clone();
            } else {
                b.blocks = nested;
            }
            out.push(b);
            builder.marked = true;
            builder.beyond_compose_field = true;
            continue;
        }

        // GFM Table
        if trimmed.contains('|')
            && index + 1 < count
            && markdown_table_separator_line(lines[index + 1].trim())
        {
            let header = split_markdown_table_row(line);
            let separators = split_markdown_table_row(lines[index + 1]);
            if !header.is_empty() && separators.len() == header.len() {
                if !builder.allot() {
                    return;
                }
                let mut block = RichBlock::new(RichBlockKind::Table);
                block.bordered = true;
                let make_row =
                    |cells: &[String], is_header: bool, b: &mut MarkdownBlocksBuilder| {
                        let mut row = RichTableRow::default();
                        for (col_idx, cell_raw) in cells.iter().enumerate() {
                            let unescaped = cell_raw.replace("\\|", "|");
                            let clipped: String = unescaped.chars().take(MAX_CELL_LENGTH).collect();
                            row.cells.push(RichTableCell {
                                text: RichTextNode::from_text(markdown_inline_text(&clipped, b)),
                                header: is_header,
                                alignment: separators
                                    .get(col_idx)
                                    .map(|s| markdown_cell_alignment(s))
                                    .unwrap_or(RichTableAlignment::Left),
                                ..RichTableCell::default()
                            });
                        }
                        row
                    };
                block.table_rows.push(make_row(&header, true, builder));
                index += 2;
                while index < count && lines[index].trim().contains('|') {
                    let mut cells = split_markdown_table_row(lines[index]);
                    cells.truncate(header.len());
                    while cells.len() < header.len() {
                        cells.push(String::new());
                    }
                    block.table_rows.push(make_row(&cells, false, builder));
                    index += 1;
                }
                out.push(block);
                builder.marked = true;
                continue;
            }
        }

        // Bullet / Ordered / Task list (with indentation nesting)
        if let Some(first_marker) = parse_markdown_list_marker(line) {
            if let Some(list_block) =
                parse_markdown_list(builder, &lines, &mut index, first_marker.indent)
            {
                out.push(list_block);
            }
            continue;
        }

        // Setext H1 (`===`)
        if index + 1 < count && markdown_setext_h1_line(lines[index + 1].trim()) {
            if !builder.allot() {
                return;
            }
            let mut b = RichBlock::new(RichBlockKind::Heading);
            b.heading_level = 1;
            b.text = RichTextNode::from_text(markdown_inline_text(trimmed, builder));
            out.push(b);
            builder.marked = true;
            index += 2;
            continue;
        }

        if !builder.allot() {
            return;
        }
        let mut p = RichBlock::new(RichBlockKind::Paragraph);
        p.text = RichTextNode::from_text(markdown_inline_text(trimmed, builder));
        out.push(p);
        index += 1;
    }
}

fn parse_markdown_list(
    builder: &mut MarkdownBlocksBuilder,
    lines: &[&str],
    index: &mut usize,
    base_indent: usize,
) -> Option<RichBlock> {
    let first = parse_markdown_list_marker(lines[*index])?;
    if !builder.allot() {
        return None;
    }
    let mut list = RichBlock::new(RichBlockKind::List);
    list.list_kind = if first.ordered {
        RichListKind::Ordered
    } else {
        RichListKind::Bullet
    };
    if first.ordered && first.number != 1 {
        list.ordered_list.start = Some(first.number);
    }

    while *index < lines.len() && !builder.truncated {
        let Some(marker) = parse_markdown_list_marker(lines[*index]) else {
            break;
        };
        if marker.indent < base_indent {
            break;
        }
        if marker.indent > base_indent {
            if let Some(owner) = list.list_items.last_mut()
                && let Some(nested) = parse_markdown_list(builder, lines, index, marker.indent)
            {
                if owner.text.has_content() {
                    let mut p = RichBlock::new(RichBlockKind::Paragraph);
                    p.text = std::mem::take(&mut owner.text);
                    owner.blocks.push(p);
                }
                owner.blocks.push(nested);
            } else {
                break;
            }
            continue;
        }
        let is_ordered = list.list_kind == RichListKind::Ordered;
        if marker.ordered != is_ordered {
            break;
        }
        let content = &lines[*index][marker.content_from..];
        let mut entry = RichListItem {
            task_state: marker.task,
            text: RichTextNode::from_text(markdown_inline_text(content, builder)),
            ..RichListItem::default()
        };
        if marker.ordered && list.list_items.is_empty() && marker.number != 1 {
            entry.number.value = Some(marker.number);
        }
        list.list_items.push(entry);
        builder.marked = true;
        *index += 1;
    }

    if list.list_items.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}
