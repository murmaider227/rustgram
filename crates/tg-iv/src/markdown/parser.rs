//! Port of `iv/markdown/iv_markdown_parse.cpp`, `iv_markdown_parse_convert.cpp`,
//! `iv_markdown_parse_finalize.cpp`, `iv_markdown_prepare_blocks.cpp`,
//! `iv_markdown_prepare_inline.cpp`, and `iv_markdown_prepare_links.cpp`.
//!
//! Pure-Rust GFM + Math + Telegram Markdown parser that builds a [`PreparedDocument`] AST
//! and converts it into a [`RichPage`].

use std::collections::HashMap;

use crate::markdown::document::{
    HtmlBlockKind, ListDelimiter, ListKind, MarkdownNode, MarkdownParseLimits,
    MarkdownTableAlignment, MarkdownTaskState, MathFormula, MathKind, NodeKind, ParseOptions,
    ParseResult, ParseStats, PreparedDocument, SourceRange, ValidatedMarkdownSource,
};
use crate::markdown::math_extract::{MathScanBlock, extract_math_regions, range_for_offsets};
use crate::markdown::validate::validate_markdown_source_for_iv;
use crate::model::{normalize_fragment_id, trim_math_source};
use crate::rich_page::{
    EntityInText, EntityType, InlineTextObjectEntity, InlineTextObjectFormulaData,
    OBJECT_REPLACEMENT_CHAR, OrderedListData, RichBlock, RichBlockKind, RichListItem, RichListKind,
    RichPage, RichTableAlignment, RichTableCell, RichTableRow, RichTaskState, RichTextNode,
    TextWithEntities, inline_formula_copy_source, serialize_inline_text_object_entity,
};

/// Parses raw Markdown bytes using [`MarkdownParseLimits::IV_DEFAULT`] or custom `options`
/// (`ParseMarkdownForIv`).
#[must_use]
pub fn parse_markdown_for_iv(source: &[u8], options: ParseOptions) -> ParseResult {
    let limits = options.limits;
    let validated = validate_markdown_source_for_iv(source, options);
    if !validated.ok {
        return parse_failure(validated.source.source_name, validated.error);
    }
    parse_validated_markdown(validated.source, limits)
}

/// Convenience helper parsing a UTF-8 Markdown string into a [`RichPage`].
pub fn markdown_to_rich_page(markdown: &str) -> Result<RichPage, String> {
    let parsed = parse_markdown_for_iv(markdown.as_bytes(), ParseOptions::default());
    if !parsed.ok {
        return Err(parsed.error);
    }
    Ok(prepared_document_to_rich_page(&parsed.document))
}

fn parse_failure(source_name: String, error: String) -> ParseResult {
    ParseResult {
        document: PreparedDocument::empty(source_name),
        error,
        ok: false,
    }
}

/// Parses a validated Markdown source into a [`PreparedDocument`].
#[must_use]
pub fn parse_validated_markdown(
    source: ValidatedMarkdownSource,
    limits: MarkdownParseLimits,
) -> ParseResult {
    let mut doc = PreparedDocument::empty(source.source_name.clone());
    doc.source_text = source.decoded.clone();

    let mut mask = vec![false; source.normalized.len()];
    let mut scan_blocks = Vec::new();
    let mut parser = BlockParser {
        src: &source.decoded,
        bytes: &source.normalized,
        line_starts: &source.line_starts,
        limits,
        mask: &mut mask,
        scan_blocks: &mut scan_blocks,
        stats: &mut doc.stats,
        warnings: &mut doc.warnings,
        node_count: 1,
        aborted: None,
    };

    let children = parser.parse_blocks(0, source.decoded.len(), 1);
    if let Some(err) = parser.aborted {
        return parse_failure(source.source_name, err);
    }
    doc.document.children = children;
    if !source.normalized.is_empty() {
        doc.document.range = range_for_offsets(
            &source.line_starts,
            source.normalized.len(),
            0,
            source.normalized.len(),
        );
    }

    // Extract math formulas using tdesktop's exact heuristics while respecting masked regions.
    let formulas = match extract_math_regions(
        &source.normalized,
        &mask,
        &source.line_starts,
        &scan_blocks,
        limits.max_formula_bytes,
        limits.max_formula_count,
    ) {
        Ok(f) => f,
        Err(err) => return parse_failure(source.source_name, err),
    };
    doc.formulas = formulas;

    // Normalize display math out of paragraphs and attach inline formulas.
    normalize_display_math_in_document(&mut doc, &source.normalized, &source.line_starts);
    attach_inline_formulas(&mut doc.document, &doc.formulas);

    // Semantic finalization: heading anchors, footnote ordinals, fragment validation.
    finalize_document_semantics(&mut doc);

    // Compute stats & title.
    doc.stats.converted_node_count = count_nodes(&doc.document);
    doc.stats.inline_formula_count = doc
        .formulas
        .iter()
        .filter(|f| f.kind == MathKind::Inline)
        .count();
    doc.stats.display_formula_count = doc
        .formulas
        .iter()
        .filter(|f| f.kind == MathKind::Display)
        .count();
    if let Some(title) = first_heading_title(&doc.document) {
        doc.title = title;
    }
    doc.empty = doc.document.children.is_empty();

    ParseResult {
        document: doc,
        error: String::new(),
        ok: true,
    }
}

/// Computes a heading anchor slug matching `AnchorIdBaseFromText` (`iv_markdown_parse_finalize.cpp`).
#[must_use]
pub fn anchor_id_base_from_text(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let mut result = String::new();
    let mut pending_hyphen = false;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            if pending_hyphen && !result.is_empty() {
                result.push('-');
            }
            result.push(ch);
            pending_hyphen = false;
        } else if !result.is_empty() {
            pending_hyphen = true;
        }
    }
    if result.is_empty() {
        "section".to_string()
    } else {
        result
    }
}

/// Decodes backslash-escaped ASCII punctuation and basic HTML character entities
/// (`DecodeMarkdownTextPrefix` in `iv_markdown_prepare_links.cpp`).
#[must_use]
pub fn decode_markdown_escapes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && is_escapable_ascii(bytes[i + 1]) {
            out.push(bytes[i + 1] as char);
            i += 2;
        } else if bytes[i] == b'&'
            && let Some(semi_rel) = bytes[i + 1..].iter().position(|&b| b == b';')
            && semi_rel > 0
            && semi_rel <= 32
            && let Some(decoded) = decode_html_entity(&input[i + 1..i + 1 + semi_rel])
        {
            out.push(decoded);
            i += semi_rel + 2;
        } else {
            let ch = input[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

const fn is_escapable_ascii(b: u8) -> bool {
    matches!(b, 0x21..=0x2F | 0x3A..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E)
}

fn decode_html_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            let v = u32::from_str_radix(&entity[2..], 16).ok()?;
            char::from_u32(v)
        }
        _ if entity.starts_with('#') => {
            let v = entity[1..].parse::<u32>().ok()?;
            char::from_u32(v)
        }
        _ => None,
    }
}

/// Extracts the first whitespace-delimited token of a fenced code info string (`FirstInfoToken`).
#[must_use]
pub fn first_info_token(info: &str) -> String {
    let trimmed = info.trim();
    trimmed.split_whitespace().next().unwrap_or("").to_string()
}

// ============================================================================
// Block Parser
// ============================================================================

struct BlockParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    line_starts: &'a [usize],
    limits: MarkdownParseLimits,
    mask: &'a mut [bool],
    scan_blocks: &'a mut Vec<MathScanBlock>,
    stats: &'a mut ParseStats,
    warnings: &'a mut Vec<String>,
    node_count: usize,
    aborted: Option<String>,
}

#[derive(Clone, Copy)]
struct LineSlice {
    start: usize,
    end: usize, // excludes trailing \r?\n
}

fn split_lines(src: &str, start: usize, end: usize) -> Vec<LineSlice> {
    let mut lines = Vec::new();
    let mut pos = start;
    while pos < end {
        let rest = &src.as_bytes()[pos..end];
        let rel_nl = rest.iter().position(|&b| b == b'\n');
        let (line_end, next) = match rel_nl {
            Some(nl) => {
                let raw_end = pos + nl;
                let trimmed_cr = if raw_end > pos && src.as_bytes()[raw_end - 1] == b'\r' {
                    raw_end - 1
                } else {
                    raw_end
                };
                (trimmed_cr, pos + nl + 1)
            }
            None => {
                let trimmed_cr = if end > pos && src.as_bytes()[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                };
                (trimmed_cr, end)
            }
        };
        lines.push(LineSlice {
            start: pos,
            end: line_end,
        });
        pos = next;
    }
    lines
}

impl<'a> BlockParser<'a> {
    fn bump_node(&mut self, depth: usize) -> bool {
        self.stats.max_depth = self.stats.max_depth.max(depth);
        if depth > self.limits.max_nesting {
            self.aborted = Some("cmark-nesting-too-deep".to_string());
            return false;
        }
        self.node_count += 1;
        self.stats.cmark_node_count = self.node_count;
        if self.node_count > self.limits.max_cmark_nodes {
            self.aborted = Some("too-many-cmark-nodes".to_string());
            return false;
        }
        true
    }

    fn make_range(&self, start: usize, end: usize) -> SourceRange {
        if start >= end || end > self.bytes.len() {
            return SourceRange::default();
        }
        range_for_offsets(self.line_starts, self.bytes.len(), start, end)
    }

    fn mark_mask(&mut self, start: usize, end: usize) {
        let s = start.min(self.mask.len());
        let e = end.min(self.mask.len());
        for m in &mut self.mask[s..e] {
            *m = true;
        }
    }

    fn record_scan_block(&mut self, range: SourceRange, kind: &str) {
        if range.available {
            self.scan_blocks.push(MathScanBlock {
                range,
                node_kind: kind.to_string(),
            });
        }
    }

    fn parse_blocks(&mut self, start: usize, end: usize, depth: usize) -> Vec<MarkdownNode> {
        if self.aborted.is_some() || !self.bump_node(depth) {
            return Vec::new();
        }
        let lines = split_lines(self.src, start, end);
        let mut nodes = Vec::new();
        let mut idx = 0usize;

        while idx < lines.len() {
            if self.aborted.is_some() {
                break;
            }
            let line_str = &self.src[lines[idx].start..lines[idx].end];
            if line_str.trim().is_empty() {
                idx += 1;
                continue;
            }

            // 1. Fenced code block (``` or ~~~)
            if let Some((fence_char, fence_len, info)) = parse_fence_open(line_str) {
                let block_start = lines[idx].start;
                let mut code = String::new();
                idx += 1;
                let mut block_end = lines[idx - 1].end;
                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    block_end = lines[idx].end;
                    if is_fence_close(cur, fence_char, fence_len) {
                        idx += 1;
                        break;
                    }
                    code.push_str(cur);
                    code.push('\n');
                    idx += 1;
                }
                let range = self.make_range(block_start, block_end);
                self.mark_mask(block_start, block_end);
                self.record_scan_block(range, "code_block");
                let mut node = MarkdownNode::new(NodeKind::CodeBlock);
                node.range = range;
                node.text = code;
                node.info = info;
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                continue;
            }

            // 2. Display math fenced block (`$$` on its own line)
            if line_str.trim() == "$$" {
                let block_start = lines[idx].start;
                let mut para_end = lines[idx].end;
                let mut closed = false;
                let mut j = idx + 1;
                while j < lines.len() {
                    let cur = &self.src[lines[j].start..lines[j].end];
                    para_end = lines[j].end;
                    if cur.trim() == "$$" {
                        closed = true;
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                if closed {
                    let range = self.make_range(block_start, para_end);
                    self.record_scan_block(range, "paragraph");
                    let mut p = MarkdownNode::new(NodeKind::Paragraph);
                    p.range = range;
                    let mut txt = MarkdownNode::new(NodeKind::Text);
                    txt.range = range;
                    txt.text = self.src[block_start..para_end].to_string();
                    p.children.push(txt);
                    if self.bump_node(depth + 1) {
                        nodes.push(p);
                    }
                    idx = j;
                    continue;
                }
            }

            // 3. HTML comment or `<details>` block
            let trimmed_start = line_str.trim_start();
            if trimmed_start.starts_with("<!--") {
                let block_start = lines[idx].start;
                let mut block_end = lines[idx].end;
                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    block_end = lines[idx].end;
                    idx += 1;
                    if cur.contains("-->") {
                        break;
                    }
                }
                let range = self.make_range(block_start, block_end);
                self.mark_mask(block_start, block_end);
                let mut node = MarkdownNode::new(NodeKind::HtmlBlock);
                node.range = range;
                node.raw = self.src[block_start..block_end].to_string();
                node.html_block_kind = HtmlBlockKind::Comment;
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                continue;
            }
            if starts_with_ignore_ascii_case(trimmed_start, "<details") {
                let block_start = lines[idx].start;
                let mut block_end = lines[idx].end;
                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    block_end = lines[idx].end;
                    idx += 1;
                    if contains_ignore_ascii_case(cur, "</details>") {
                        break;
                    }
                }
                let raw = self.src[block_start..block_end].to_string();
                let range = self.make_range(block_start, block_end);
                self.mark_mask(block_start, block_end);
                let mut node = MarkdownNode::new(NodeKind::HtmlBlock);
                node.range = range;
                if let Some((open, summary, body)) = parse_details_block(&raw) {
                    node.html_block_kind = HtmlBlockKind::Details;
                    node.details_open = open;
                    node.details_summary = summary;
                    node.details_body = body;
                } else {
                    node.html_block_kind = HtmlBlockKind::Unsupported;
                    self.warnings.push(format!(
                        "Malformed details block at {}:{}",
                        range.start_line, range.start_column
                    ));
                }
                node.raw = raw;
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                continue;
            }

            // 4. Thematic break (`---`, `***`, `___`)
            if is_thematic_break(line_str) {
                let range = self.make_range(lines[idx].start, lines[idx].end);
                self.record_scan_block(range, "thematic_break");
                let mut node = MarkdownNode::new(NodeKind::ThematicBreak);
                node.range = range;
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                idx += 1;
                continue;
            }

            // 5. ATX Heading (`#`..`######`)
            if let Some((level, content_start, content_end)) =
                parse_atx_heading(line_str, lines[idx].start)
            {
                let range = self.make_range(lines[idx].start, lines[idx].end);
                self.record_scan_block(range, "heading");
                let mut node = MarkdownNode::new(NodeKind::Heading);
                node.range = range;
                node.heading_level = level;
                node.children = self.parse_inlines(content_start, content_end, depth + 2);
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                idx += 1;
                continue;
            }

            // 6. Footnote definition (`[^label]: ...`)
            if let Some((label, body_start_in_line)) = parse_footnote_definition(line_str) {
                self.stats.footnotes_seen = true;
                let block_start = lines[idx].start;
                let mut body = line_str[body_start_in_line..].to_string();
                let mut block_end = lines[idx].end;
                idx += 1;
                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    if cur.starts_with("    ") || cur.starts_with('\t') {
                        body.push('\n');
                        body.push_str(
                            cur.strip_prefix("    ")
                                .or_else(|| cur.strip_prefix('\t'))
                                .unwrap_or(cur),
                        );
                        block_end = lines[idx].end;
                        idx += 1;
                    } else {
                        break;
                    }
                }
                let range = self.make_range(block_start, block_end);
                self.record_scan_block(range, "footnote_definition");
                let mut node = MarkdownNode::new(NodeKind::FootnoteDefinition);
                node.range = range;
                node.footnote_label = label;
                node.raw = self.src[block_start..block_end].to_string();
                let mut p = MarkdownNode::new(NodeKind::Paragraph);
                p.children = parse_inlines_standalone(&body, self.stats);
                node.children.push(p);
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                continue;
            }

            // 7. Blockquote (`> ...`)
            if is_blockquote_line(line_str) {
                let block_start = lines[idx].start;
                let mut inner_lines = Vec::new();
                let mut block_end = lines[idx].end;
                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    if let Some(stripped) = strip_blockquote_prefix(cur) {
                        inner_lines.push(stripped);
                        block_end = lines[idx].end;
                        idx += 1;
                    } else {
                        break;
                    }
                }
                let inner_text = inner_lines.join("\n");
                let range = self.make_range(block_start, block_end);
                self.record_scan_block(range, "block_quote");
                let mut node = MarkdownNode::new(NodeKind::Blockquote);
                node.range = range;
                let sub = parse_nested_markdown(
                    &inner_text,
                    self.limits,
                    self.stats,
                    self.warnings,
                    depth + 1,
                );
                node.children = sub;
                if self.bump_node(depth + 1) {
                    nodes.push(node);
                }
                continue;
            }

            // 8. GFM Pipe Table (header line + delimiter line `| --- |`)
            if idx + 1 < lines.len() {
                let next_line = &self.src[lines[idx + 1].start..lines[idx + 1].end];
                if let Some(alignments) = parse_table_delimiter_row(next_line) {
                    let header_cells = split_table_cells(line_str);
                    if !header_cells.is_empty() && header_cells.len() == alignments.len() {
                        self.stats.tables_seen = true;
                        let block_start = lines[idx].start;
                        let mut table = MarkdownNode::new(NodeKind::Table);
                        table.table_alignments = alignments.clone();

                        let mut header_row = MarkdownNode::new(NodeKind::TableRow);
                        header_row.table_header = true;
                        header_row.range = self.make_range(lines[idx].start, lines[idx].end);
                        for (col, cell_text) in header_cells.iter().enumerate() {
                            let mut cell = MarkdownNode::new(NodeKind::TableCell);
                            cell.table_column = col as i32;
                            cell.children = parse_inlines_standalone(cell_text.trim(), self.stats);
                            header_row.children.push(cell);
                        }
                        table.children.push(header_row);

                        let mut block_end = lines[idx + 1].end;
                        idx += 2;
                        while idx < lines.len() {
                            let row_line = &self.src[lines[idx].start..lines[idx].end];
                            if row_line.trim().is_empty() || !row_line.contains('|') {
                                break;
                            }
                            let row_cells = split_table_cells(row_line);
                            let mut row = MarkdownNode::new(NodeKind::TableRow);
                            row.range = self.make_range(lines[idx].start, lines[idx].end);
                            for col in 0..alignments.len() {
                                let text = row_cells.get(col).map(String::as_str).unwrap_or("");
                                let mut cell = MarkdownNode::new(NodeKind::TableCell);
                                cell.table_column = col as i32;
                                cell.children = parse_inlines_standalone(text.trim(), self.stats);
                                row.children.push(cell);
                            }
                            table.children.push(row);
                            block_end = lines[idx].end;
                            idx += 1;
                        }
                        table.range = self.make_range(block_start, block_end);
                        self.record_scan_block(table.range, "table");
                        if self.bump_node(depth + 1) {
                            nodes.push(table);
                        }
                        continue;
                    }
                }
            }

            // 9. Lists (bullet, task, ordered)
            if let Some(first_marker) = parse_list_marker(line_str) {
                let list_start_offset = lines[idx].start;
                let mut list = MarkdownNode::new(NodeKind::List);
                list.list_kind = first_marker.kind;
                list.list_delimiter = first_marker.delimiter;
                list.list_start = first_marker.start;
                list.tight = true;
                let base_indent = first_marker.indent;
                let mut list_end_offset = lines[idx].end;

                while idx < lines.len() {
                    let cur = &self.src[lines[idx].start..lines[idx].end];
                    let Some(marker) = parse_list_marker(cur) else {
                        break;
                    };
                    if marker.kind != first_marker.kind || marker.indent != base_indent {
                        break;
                    }
                    let item_start = lines[idx].start;
                    let content_indent = marker.content_col;
                    let mut item_text = cur[marker.content_start..].to_string();
                    let (task_state, rest_after_task) = strip_task_prefix(&item_text);
                    if task_state != MarkdownTaskState::None {
                        self.stats.task_lists_seen = true;
                        item_text = rest_after_task.to_string();
                    }
                    let mut item_lines = vec![item_text];
                    let mut item_end = lines[idx].end;
                    idx += 1;
                    let mut had_blank = false;
                    while idx < lines.len() {
                        let next_str = &self.src[lines[idx].start..lines[idx].end];
                        if next_str.trim().is_empty() {
                            if idx + 1 < lines.len() {
                                let peek = &self.src[lines[idx + 1].start..lines[idx + 1].end];
                                let peek_indent = leading_spaces(peek);
                                if peek_indent >= content_indent {
                                    list.tight = false;
                                    item_lines.push(String::new());
                                    idx += 1;
                                    continue;
                                }
                                if let Some(m2) = parse_list_marker(peek)
                                    && m2.kind == first_marker.kind
                                    && m2.indent == base_indent
                                {
                                    list.tight = false;
                                    had_blank = true;
                                    idx += 1;
                                    break;
                                }
                            }
                            break;
                        }
                        let ind = leading_spaces(next_str);
                        if ind >= content_indent {
                            let stripped = strip_leading_spaces(next_str, content_indent);
                            item_lines.push(stripped.to_string());
                            item_end = lines[idx].end;
                            idx += 1;
                        } else {
                            break;
                        }
                    }
                    let _ = had_blank;
                    let mut item = MarkdownNode::new(NodeKind::ListItem);
                    item.task_state = task_state;
                    item.range = self.make_range(item_start, item_end);
                    let joined = item_lines.join("\n");
                    item.children = parse_nested_markdown(
                        &joined,
                        self.limits,
                        self.stats,
                        self.warnings,
                        depth + 2,
                    );
                    list.children.push(item);
                    list_end_offset = item_end;
                }
                list.range = self.make_range(list_start_offset, list_end_offset);
                self.record_scan_block(list.range, "list");
                if self.bump_node(depth + 1) {
                    nodes.push(list);
                }
                continue;
            }

            // 10. Setext heading (`===` or `---` on the following line)
            if idx + 1 < lines.len() {
                let underline = &self.src[lines[idx + 1].start..lines[idx + 1].end];
                if let Some(level) = parse_setext_underline(underline) {
                    let range = self.make_range(lines[idx].start, lines[idx + 1].end);
                    self.record_scan_block(range, "heading");
                    let mut node = MarkdownNode::new(NodeKind::Heading);
                    node.range = range;
                    node.heading_level = level;
                    let trimmed = line_str.trim();
                    let lead = line_str.len() - line_str.trim_start().len();
                    node.children = self.parse_inlines(
                        lines[idx].start + lead,
                        lines[idx].start + lead + trimmed.len(),
                        depth + 2,
                    );
                    if self.bump_node(depth + 1) {
                        nodes.push(node);
                    }
                    idx += 2;
                    continue;
                }
            }

            // 11. Paragraph (collect consecutive non-blank, non-block lines)
            let para_start = lines[idx].start;
            let mut para_end = lines[idx].end;
            idx += 1;
            while idx < lines.len() {
                let cur = &self.src[lines[idx].start..lines[idx].end];
                if cur.trim().is_empty()
                    || parse_fence_open(cur).is_some()
                    || is_thematic_break(cur)
                    || parse_atx_heading(cur, lines[idx].start).is_some()
                    || is_blockquote_line(cur)
                    || parse_list_marker(cur).is_some()
                    || parse_footnote_definition(cur).is_some()
                    || starts_with_ignore_ascii_case(cur.trim_start(), "<details")
                    || cur.trim_start().starts_with("<!--")
                    || cur.trim() == "$$"
                {
                    break;
                }
                para_end = lines[idx].end;
                idx += 1;
            }
            let range = self.make_range(para_start, para_end);
            self.record_scan_block(range, "paragraph");
            let mut para = MarkdownNode::new(NodeKind::Paragraph);
            para.range = range;
            para.children = self.parse_inlines(para_start, para_end, depth + 2);
            if self.bump_node(depth + 1) {
                nodes.push(para);
            }
        }
        nodes
    }

    fn parse_inlines(&mut self, start: usize, end: usize, depth: usize) -> Vec<MarkdownNode> {
        if start >= end || depth > self.limits.max_nesting {
            return Vec::new();
        }
        let mut nodes = parse_inlines_with_offsets(
            self.src,
            start,
            end,
            self.line_starts,
            self.mask,
            self.stats,
            depth,
            self.limits.max_nesting,
        );
        // Merge adjacent text nodes? Keep exact source ranges so inline math splicing stays exact.
        if nodes.is_empty() && start < end {
            let mut txt = MarkdownNode::new(NodeKind::Text);
            txt.range = self.make_range(start, end);
            txt.text = self.src[start..end].to_string();
            nodes.push(txt);
        }
        nodes
    }
}

fn parse_nested_markdown(
    text: &str,
    limits: MarkdownParseLimits,
    stats: &mut ParseStats,
    warnings: &mut Vec<String>,
    depth: usize,
) -> Vec<MarkdownNode> {
    if text.trim().is_empty() || depth > limits.max_nesting {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let line_starts = crate::markdown::math_extract::build_line_starts(bytes);
    let mut mask = vec![false; bytes.len()];
    let mut scan_blocks = Vec::new();
    let mut parser = BlockParser {
        src: text,
        bytes,
        line_starts: &line_starts,
        limits,
        mask: &mut mask,
        scan_blocks: &mut scan_blocks,
        stats,
        warnings,
        node_count: 1,
        aborted: None,
    };
    let mut children = parser.parse_blocks(0, text.len(), depth);
    if let Ok(formulas) = extract_math_regions(
        bytes,
        &mask,
        &line_starts,
        &scan_blocks,
        limits.max_formula_bytes,
        limits.max_formula_count,
    ) && !formulas.is_empty()
    {
        let mut dummy = PreparedDocument::empty("");
        dummy.document.children = children;
        dummy.formulas = formulas;
        normalize_display_math_in_document(&mut dummy, bytes, &line_starts);
        attach_inline_formulas(&mut dummy.document, &dummy.formulas);
        children = dummy.document.children;
    }
    children
}

fn parse_inlines_standalone(text: &str, stats: &mut ParseStats) -> Vec<MarkdownNode> {
    if text.is_empty() {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let line_starts = crate::markdown::math_extract::build_line_starts(bytes);
    let mut mask = vec![false; bytes.len()];
    let mut nodes = parse_inlines_with_offsets(
        text,
        0,
        text.len(),
        &line_starts,
        &mut mask,
        stats,
        0,
        MarkdownParseLimits::IV_DEFAULT.max_nesting,
    );
    if let Ok(formulas) = extract_math_regions(
        bytes,
        &mask,
        &line_starts,
        &[],
        MarkdownParseLimits::IV_DEFAULT.max_formula_bytes,
        MarkdownParseLimits::IV_DEFAULT.max_formula_count,
    ) && !formulas.is_empty()
    {
        let mut root = MarkdownNode::new(NodeKind::Paragraph);
        root.children = nodes;
        attach_inline_formulas(&mut root, &formulas);
        nodes = root.children;
    }
    nodes
}

// ============================================================================
// Block syntax helpers
// ============================================================================

fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    h.len() >= n.len() && h[..n.len()].eq_ignore_ascii_case(n)
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn leading_spaces(s: &str) -> usize {
    let mut cols = 0;
    for b in s.bytes() {
        match b {
            b' ' => cols += 1,
            b'\t' => cols += 4,
            _ => break,
        }
    }
    cols
}

fn strip_leading_spaces(s: &str, max_cols: usize) -> &str {
    let mut cols = 0;
    let mut idx = 0;
    for (i, ch) in s.char_indices() {
        if cols >= max_cols {
            break;
        }
        match ch {
            ' ' => {
                cols += 1;
                idx = i + 1;
            }
            '\t' => {
                cols += 4;
                idx = i + 1;
            }
            _ => break,
        }
    }
    &s[idx..]
}

fn parse_fence_open(line: &str) -> Option<(u8, usize, String)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return None;
    }
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let count = trimmed.bytes().take_while(|&b| b == first).count();
    if count < 3 {
        return None;
    }
    let info = trimmed[count..].trim();
    if first == b'`' && info.contains('`') {
        return None;
    }
    Some((first, count, info.to_string()))
}

fn is_fence_close(line: &str, fence_char: u8, min_len: usize) -> bool {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let count = trimmed.bytes().take_while(|&b| b == fence_char).count();
    count >= min_len && trimmed[count..].trim().is_empty()
}

fn is_thematic_break(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let marker = match trimmed.as_bytes().first() {
        Some(&b) if matches!(b, b'-' | b'*' | b'_') => b,
        _ => return false,
    };
    let mut count = 0usize;
    for b in trimmed.bytes() {
        if b == marker {
            count += 1;
        } else if b != b' ' && b != b'\t' {
            return false;
        }
    }
    count >= 3
}

fn parse_atx_heading(line: &str, line_start: usize) -> Option<(u8, usize, usize)> {
    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    if lead > 3 {
        return None;
    }
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let after_space = rest.trim_start_matches([' ', '\t']);
    let content_lead = rest.len() - after_space.len();
    // Strip optional closing `###` sequence preceded by whitespace.
    let mut content = after_space.trim_end_matches([' ', '\t']);
    let without_trailing_hashes = content.trim_end_matches('#');
    if without_trailing_hashes.len() < content.len()
        && (without_trailing_hashes.is_empty() || without_trailing_hashes.ends_with([' ', '\t']))
    {
        content = without_trailing_hashes.trim_end_matches([' ', '\t']);
    }
    let start = line_start + lead + hashes + content_lead;
    let end = start + content.len();
    Some((hashes as u8, start, end))
}

fn parse_setext_underline(line: &str) -> Option<u8> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b == b'=') {
        Some(1)
    } else if trimmed.len() >= 2 && trimmed.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

fn is_blockquote_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    (line.len() - trimmed.len()) <= 3 && trimmed.starts_with('>')
}

fn strip_blockquote_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let after = trimmed.strip_prefix('>')?;
    Some(after.strip_prefix(' ').unwrap_or(after))
}

struct ListMarker {
    kind: ListKind,
    delimiter: ListDelimiter,
    start: i32,
    indent: usize,
    content_start: usize,
    content_col: usize,
}

fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let indent = leading_spaces(line);
    let trimmed = line.trim_start_matches([' ', '\t']);
    let lead_bytes = line.len() - trimmed.len();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2
        && matches!(bytes[0], b'-' | b'*' | b'+')
        && matches!(bytes[1], b' ' | b'\t')
    {
        return Some(ListMarker {
            kind: ListKind::Bullet,
            delimiter: ListDelimiter::None,
            start: 1,
            indent,
            content_start: lead_bytes + 2,
            content_col: indent + 2,
        });
    }
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && digits + 1 < bytes.len()
        && matches!(bytes[digits], b'.' | b')')
        && matches!(bytes[digits + 1], b' ' | b'\t')
    {
        let start = trimmed[..digits].parse::<i32>().ok()?;
        let delimiter = if bytes[digits] == b'.' {
            ListDelimiter::Period
        } else {
            ListDelimiter::Parenthesis
        };
        return Some(ListMarker {
            kind: ListKind::Ordered,
            delimiter,
            start,
            indent,
            content_start: lead_bytes + digits + 2,
            content_col: indent + digits + 2,
        });
    }
    None
}

fn strip_task_prefix(text: &str) -> (MarkdownTaskState, &str) {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        (MarkdownTaskState::Unchecked, rest)
    } else if let Some(rest) = text
        .strip_prefix("[x] ")
        .or_else(|| text.strip_prefix("[X] "))
    {
        (MarkdownTaskState::Checked, rest)
    } else if text == "[ ]" {
        (MarkdownTaskState::Unchecked, "")
    } else if text == "[x]" || text == "[X]" {
        (MarkdownTaskState::Checked, "")
    } else {
        (MarkdownTaskState::None, text)
    }
}

fn parse_footnote_definition(line: &str) -> Option<(String, usize)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("[^") {
        return None;
    }
    let close = trimmed.find("]:")?;
    if close <= 2 {
        return None;
    }
    let label = trimmed[2..close].trim();
    if label.is_empty() || label.contains(char::is_whitespace) {
        return None;
    }
    let lead = line.len() - trimmed.len();
    let after = lead + close + 2;
    let body_start = if line.as_bytes().get(after) == Some(&b' ') {
        after + 1
    } else {
        after
    };
    Some((label.to_string(), body_start))
}

fn parse_details_open_attr(raw: &str) -> bool {
    let lower = raw.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "open" | "open=\"\"" | "open=''" | "open=\"open\"" | "open='open'"
    )
}

/// Port of `ParseDetailsBlock` (`iv_markdown_parse_convert.cpp:405-443`).
fn parse_details_block(raw: &str) -> Option<(bool, String, String)> {
    let trimmed = raw.trim();
    if !starts_with_ignore_ascii_case(trimmed, "<details")
        || trimmed.len() < 18
        || !trimmed.as_bytes()[trimmed.len() - 10..].eq_ignore_ascii_case(b"</details>")
    {
        return None;
    }
    let opening_end = trimmed.find('>')?;
    let attrs = trimmed[8..opening_end].trim();
    let open = if attrs.is_empty() {
        false
    } else if parse_details_open_attr(attrs) {
        true
    } else {
        return None;
    };
    let inner = trimmed[opening_end + 1..trimmed.len() - 10].trim();
    if contains_ignore_ascii_case(inner, "<details") {
        return None;
    }
    if !starts_with_ignore_ascii_case(inner, "<summary>") {
        return None;
    }
    let lower_inner = inner.to_ascii_lowercase();
    let summary_close = lower_inner.find("</summary>")?;
    if summary_close < 9 {
        return None;
    }
    let summary = inner[9..summary_close].trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let body = inner[summary_close + 10..].trim().to_string();
    Some((open, summary, body))
}

fn split_table_cells(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if let Some(stripped) = s.strip_prefix('|') {
        s = stripped;
    }
    if s.ends_with('|') && !s.ends_with("\\|") {
        s = &s[..s.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_code = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            cur.push('|');
        } else if ch == '`' {
            in_code = !in_code;
            cur.push(ch);
        } else if ch == '|' && !in_code {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur);
    cells
}

fn parse_table_delimiter_row(line: &str) -> Option<Vec<MarkdownTableAlignment>> {
    if !line.contains('-') || !line.contains('|') {
        return None;
    }
    let cells = split_table_cells(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let t = cell.trim();
        if t.is_empty() {
            return None;
        }
        let left = t.starts_with(':');
        let right = t.ends_with(':');
        let core = t
            .strip_prefix(':')
            .unwrap_or(t)
            .strip_suffix(':')
            .unwrap_or(t.strip_prefix(':').unwrap_or(t));
        if core.is_empty() || !core.bytes().all(|b| b == b'-') {
            return None;
        }
        let align = match (left, right) {
            (true, true) => MarkdownTableAlignment::Center,
            (true, false) => MarkdownTableAlignment::Left,
            (false, true) => MarkdownTableAlignment::Right,
            (false, false) => MarkdownTableAlignment::None,
        };
        aligns.push(align);
    }
    Some(aligns)
}

// ============================================================================
// Inline Parser
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn parse_inlines_with_offsets(
    src: &str,
    start: usize,
    end: usize,
    line_starts: &[usize],
    mask: &mut [bool],
    stats: &mut ParseStats,
    depth: usize,
    max_depth: usize,
) -> Vec<MarkdownNode> {
    if start >= end || depth > max_depth {
        return Vec::new();
    }
    let bytes = src.as_bytes();
    let mut nodes = Vec::new();
    let mut text_start = start;
    let mut i = start;

    let flush_text = |nodes: &mut Vec<MarkdownNode>, from: usize, to: usize| {
        if to > from {
            let mut n = MarkdownNode::new(NodeKind::Text);
            n.range = range_for_offsets(line_starts, bytes.len(), from, to);
            n.text = decode_markdown_escapes(&src[from..to]);
            nodes.push(n);
        }
    };

    while i < end {
        let b = bytes[i];

        // Escaped punctuation: leave inside text run so decode_markdown_escapes unescapes it
        if b == b'\\' && i + 1 < end {
            if bytes[i + 1] == b'\n' {
                flush_text(&mut nodes, text_start, i);
                let mut br = MarkdownNode::new(NodeKind::LineBreak);
                br.range = range_for_offsets(line_starts, bytes.len(), i, i + 2);
                br.text = "\n".to_string();
                nodes.push(br);
                i += 2;
                text_start = i;
                continue;
            }
            if is_escapable_ascii(bytes[i + 1]) {
                i += 2;
                continue;
            }
        }

        // Line breaks
        if b == b'\n' {
            let hard = i >= text_start + 2 && bytes[i - 1] == b' ' && bytes[i - 2] == b' ';
            let trim_to = if hard { i - 2 } else { i };
            flush_text(&mut nodes, text_start, trim_to);
            let mut br = MarkdownNode::new(if hard {
                NodeKind::LineBreak
            } else {
                NodeKind::SoftBreak
            });
            br.range = range_for_offsets(line_starts, bytes.len(), i, i + 1);
            br.text = "\n".to_string();
            nodes.push(br);
            i += 1;
            text_start = i;
            continue;
        }

        // Inline code span (`...`)
        if b == b'`' {
            let ticks = bytes[i..end].iter().take_while(|&&c| c == b'`').count();
            if let Some(close_rel) = find_matching_backticks(&bytes[i + ticks..end], ticks) {
                flush_text(&mut nodes, text_start, i);
                let code_start = i + ticks;
                let code_end = code_start + close_rel;
                let full_end = code_end + ticks;
                let mask_len = mask.len();
                for m in &mut mask[i.min(mask_len)..full_end.min(mask_len)] {
                    *m = true;
                }
                let raw = &src[code_start..code_end];
                let content = if raw.len() >= 2
                    && raw.starts_with(' ')
                    && raw.ends_with(' ')
                    && raw.bytes().any(|c| c != b' ')
                {
                    &raw[1..raw.len() - 1]
                } else {
                    raw
                };
                let mut n = MarkdownNode::new(NodeKind::InlineCode);
                n.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                n.text = content.replace('\n', " ");
                nodes.push(n);
                i = full_end;
                text_start = i;
                continue;
            }
            i += ticks;
            continue;
        }

        // Preserve $...$ / $$...$$ verbatim inside Text so math extractor + splicer handle them
        if b == b'$' {
            let is_display = i + 1 < end && bytes[i + 1] == b'$';
            let delim = if is_display { 2 } else { 1 };
            let mut j = i + delim;
            let mut found = None;
            while j < end {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if !is_display && bytes[j] == b'\n' {
                    break;
                }
                if is_display && j + 1 < end && bytes[j] == b'$' && bytes[j + 1] == b'$' {
                    found = Some(j + 2);
                    break;
                }
                if !is_display && bytes[j] == b'$' {
                    found = Some(j + 1);
                    break;
                }
                j += 1;
            }
            if let Some(close_end) = found {
                i = close_end;
                continue;
            }
            i += delim;
            continue;
        }

        // Inline HTML tags (`<sub>`, `</sub>`, `<sup>`, `</sup>`, `<mark>`, `</mark>`, `<u>`, `</u>`, `<br>`, `<br/>`)
        if b == b'<'
            && let Some((tag_len, is_autolink, autolink_url)) = parse_inline_angle(&src[i..end])
        {
            flush_text(&mut nodes, text_start, i);
            let full_end = i + tag_len;
            if is_autolink {
                stats.autolinks_seen = true;
                let mut link = MarkdownNode::new(NodeKind::Link);
                link.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                link.url = autolink_url.clone();
                link.autolink = true;
                let mut lbl = MarkdownNode::new(NodeKind::Text);
                lbl.text = src[i + 1..full_end - 1].to_string();
                link.children.push(lbl);
                nodes.push(link);
            } else {
                let mask_len = mask.len();
                for m in &mut mask[i.min(mask_len)..full_end.min(mask_len)] {
                    *m = true;
                }
                let mut html = MarkdownNode::new(NodeKind::HtmlInline);
                html.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                html.raw = src[i..full_end].to_string();
                nodes.push(html);
            }
            i = full_end;
            text_start = i;
            continue;
        }

        // Footnote reference `[^label]` or Link `[label](url "title")`
        if b == b'[' {
            if i + 2 < end
                && bytes[i + 1] == b'^'
                && let Some(close_rel) = src[i + 2..end].find(']')
            {
                let label = &src[i + 2..i + 2 + close_rel];
                if !label.is_empty() && !label.contains(char::is_whitespace) {
                    let full_end = i + 2 + close_rel + 1;
                    // Make sure it's not followed by `(` (a link whose text starts with ^)
                    if full_end >= end || bytes[full_end] != b'(' {
                        flush_text(&mut nodes, text_start, i);
                        stats.footnotes_seen = true;
                        let mut fn_ref = MarkdownNode::new(NodeKind::FootnoteReference);
                        fn_ref.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                        fn_ref.raw = src[i..full_end].to_string();
                        fn_ref.footnote_label = label.to_string();
                        nodes.push(fn_ref);
                        i = full_end;
                        text_start = i;
                        continue;
                    }
                }
            }
            if let Some((label_end, url, title, full_end)) = parse_markdown_link(src, i, end) {
                flush_text(&mut nodes, text_start, i);
                let mut link = MarkdownNode::new(NodeKind::Link);
                link.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                link.url = decode_markdown_escapes(&url);
                link.title = decode_markdown_escapes(&title);
                link.children = parse_inlines_with_offsets(
                    src,
                    i + 1,
                    label_end,
                    line_starts,
                    mask,
                    stats,
                    depth + 1,
                    max_depth,
                );
                nodes.push(link);
                i = full_end;
                text_start = i;
                continue;
            }
        }

        // Strikethrough `~~...~~`
        if b == b'~'
            && i + 1 < end
            && bytes[i + 1] == b'~'
            && let Some(close_pos) = find_delim_close(bytes, i + 2, end, b"~~")
        {
            flush_text(&mut nodes, text_start, i);
            stats.strikethrough_seen = true;
            let full_end = close_pos + 2;
            let mut n = MarkdownNode::new(NodeKind::Strike);
            n.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
            n.children = parse_inlines_with_offsets(
                src,
                i + 2,
                close_pos,
                line_starts,
                mask,
                stats,
                depth + 1,
                max_depth,
            );
            nodes.push(n);
            i = full_end;
            text_start = i;
            continue;
        }

        // Telegram spoiler `||...||`
        if b == b'|'
            && i + 1 < end
            && bytes[i + 1] == b'|'
            && let Some(close_pos) = find_delim_close(bytes, i + 2, end, b"||")
        {
            flush_text(&mut nodes, text_start, i);
            let full_end = close_pos + 2;
            let mut n = MarkdownNode::new(NodeKind::Spoiler);
            n.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
            n.children = parse_inlines_with_offsets(
                src,
                i + 2,
                close_pos,
                line_starts,
                mask,
                stats,
                depth + 1,
                max_depth,
            );
            nodes.push(n);
            i = full_end;
            text_start = i;
            continue;
        }

        // Strong `**...**` or `__...__`
        if (b == b'*' || b == b'_') && i + 1 < end && bytes[i + 1] == b {
            let delim = if b == b'*' { b"**" } else { b"__" };
            let intraword_ok = b == b'*' || i == start || !bytes[i - 1].is_ascii_alphanumeric();
            if intraword_ok && let Some(close_pos) = find_delim_close(bytes, i + 2, end, delim) {
                let close_ok = b == b'*'
                    || close_pos + 2 >= end
                    || !bytes[close_pos + 2].is_ascii_alphanumeric();
                if close_ok {
                    flush_text(&mut nodes, text_start, i);
                    let full_end = close_pos + 2;
                    let mut n = MarkdownNode::new(NodeKind::Strong);
                    n.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                    n.children = parse_inlines_with_offsets(
                        src,
                        i + 2,
                        close_pos,
                        line_starts,
                        mask,
                        stats,
                        depth + 1,
                        max_depth,
                    );
                    nodes.push(n);
                    i = full_end;
                    text_start = i;
                    continue;
                }
            }
        }

        // Emphasis `*...*` or `_..._`
        if b == b'*' || b == b'_' {
            let intraword_ok = b == b'*' || i == start || !bytes[i - 1].is_ascii_alphanumeric();
            if intraword_ok
                && i + 1 < end
                && !bytes[i + 1].is_ascii_whitespace()
                && let Some(close_pos) = find_single_emph_close(bytes, i + 1, end, b)
            {
                let close_ok = b == b'*'
                    || close_pos + 1 >= end
                    || !bytes[close_pos + 1].is_ascii_alphanumeric();
                if close_ok {
                    flush_text(&mut nodes, text_start, i);
                    let full_end = close_pos + 1;
                    let mut n = MarkdownNode::new(NodeKind::Emphasis);
                    n.range = range_for_offsets(line_starts, bytes.len(), i, full_end);
                    n.children = parse_inlines_with_offsets(
                        src,
                        i + 1,
                        close_pos,
                        line_starts,
                        mask,
                        stats,
                        depth + 1,
                        max_depth,
                    );
                    nodes.push(n);
                    i = full_end;
                    text_start = i;
                    continue;
                }
            }
        }

        // Bare URL autolink (`https://...` or `http://...`)
        if (src[i..end].starts_with("https://") || src[i..end].starts_with("http://"))
            && (i == start || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b'(')
        {
            let mut u_end = i;
            while u_end < end
                && !bytes[u_end].is_ascii_whitespace()
                && !matches!(bytes[u_end], b'<' | b'>' | b'"' | b'\'')
            {
                u_end += 1;
            }
            while u_end > i
                && matches!(
                    bytes[u_end - 1],
                    b'.' | b',' | b';' | b':' | b'!' | b'?' | b')'
                )
            {
                u_end -= 1;
            }
            if u_end > i + 8 {
                flush_text(&mut nodes, text_start, i);
                stats.autolinks_seen = true;
                let url = src[i..u_end].to_string();
                let mut link = MarkdownNode::new(NodeKind::Link);
                link.range = range_for_offsets(line_starts, bytes.len(), i, u_end);
                link.url = url.clone();
                link.autolink = true;
                let mut lbl = MarkdownNode::new(NodeKind::Text);
                lbl.text = url;
                link.children.push(lbl);
                nodes.push(link);
                i = u_end;
                text_start = i;
                continue;
            }
        }

        let ch = src[i..end].chars().next().unwrap_or('\u{FFFD}');
        i += ch.len_utf8();
    }

    flush_text(&mut nodes, text_start, end);
    nodes
}

fn find_matching_backticks(bytes: &[u8], ticks: usize) -> Option<usize> {
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = bytes[i..].iter().take_while(|&&c| c == b'`').count();
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn find_delim_close(bytes: &[u8], start: usize, end: usize, delim: &[u8; 2]) -> Option<usize> {
    if start >= end || bytes[start].is_ascii_whitespace() {
        return None;
    }
    let mut i = start;
    while i + 1 < end {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'`' {
            let ticks = bytes[i..end].iter().take_while(|&&c| c == b'`').count();
            if let Some(close) = find_matching_backticks(&bytes[i + ticks..end], ticks) {
                i = i + ticks + close + ticks;
                continue;
            }
        }
        if bytes[i] == delim[0] && bytes[i + 1] == delim[1] && !bytes[i - 1].is_ascii_whitespace() {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_single_emph_close(bytes: &[u8], start: usize, end: usize, marker: u8) -> Option<usize> {
    let mut i = start;
    while i < end {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'`' {
            let ticks = bytes[i..end].iter().take_while(|&&c| c == b'`').count();
            if let Some(close) = find_matching_backticks(&bytes[i + ticks..end], ticks) {
                i = i + ticks + close + ticks;
                continue;
            }
        }
        if bytes[i] == marker {
            // Skip double marker (`**` / `__`)
            if i + 1 < end && bytes[i + 1] == marker {
                i += 2;
                continue;
            }
            if !bytes[i - 1].is_ascii_whitespace() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn parse_inline_angle(slice: &str) -> Option<(usize, bool, String)> {
    let close = slice.find('>')?;
    if close < 2 {
        return None;
    }
    let inner = &slice[1..close];
    if inner.contains('<') || inner.contains('\n') {
        return None;
    }
    if inner.starts_with("http://")
        || inner.starts_with("https://")
        || inner.starts_with("tg://")
        || inner.starts_with("ton://")
        || inner.starts_with("tonsite://")
    {
        return Some((close + 1, true, inner.to_string()));
    }
    if inner.contains('@') && !inner.contains(' ') && !inner.starts_with('/') {
        return Some((close + 1, true, format!("mailto:{inner}")));
    }
    let lower = inner.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "sub" | "/sub" | "sup" | "/sup" | "mark" | "/mark" | "u" | "/u" | "br" | "br/" | "br /"
    ) {
        return Some((close + 1, false, String::new()));
    }
    None
}

fn parse_markdown_link(
    src: &str,
    start: usize,
    end: usize,
) -> Option<(usize, String, String, usize)> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    let mut bracket_depth = 1usize;
    while i < end {
        if bytes[i] == b'\\' {
            i = if i + 1 < end {
                i + 1 + src[i + 1..end].chars().next().map_or(0, char::len_utf8)
            } else {
                end
            };
            continue;
        }
        if bytes[i] == b'[' {
            bracket_depth += 1;
        } else if bytes[i] == b']' {
            bracket_depth -= 1;
            if bracket_depth == 0 {
                break;
            }
        }
        i += 1;
    }
    if bracket_depth != 0 || i + 1 >= end || bytes[i + 1] != b'(' {
        return None;
    }
    let label_end = i;
    let mut j = i + 2;
    while j < end && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let url_start = j;
    let mut paren_depth = 0usize;
    while j < end {
        if bytes[j] == b'\\' {
            j = if j + 1 < end {
                j + 1 + src[j + 1..end].chars().next().map_or(0, char::len_utf8)
            } else {
                end
            };
            continue;
        }
        if bytes[j] == b'(' {
            paren_depth += 1;
        } else if bytes[j] == b')' {
            if paren_depth == 0 {
                break;
            }
            paren_depth -= 1;
        } else if bytes[j].is_ascii_whitespace() && paren_depth == 0 {
            break;
        }
        j += 1;
    }
    let url = src[url_start..j].trim();
    let url = url
        .strip_prefix('<')
        .and_then(|u| u.strip_suffix('>'))
        .unwrap_or(url)
        .to_string();
    while j < end && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let mut title = String::new();
    if j < end && (bytes[j] == b'"' || bytes[j] == b'\'') {
        let quote = bytes[j];
        j += 1;
        let t_start = j;
        while j < end && bytes[j] != quote {
            if bytes[j] == b'\\' {
                j = if j + 1 < end {
                    j + 1 + src[j + 1..end].chars().next().map_or(0, char::len_utf8)
                } else {
                    end
                };
                continue;
            }
            j += 1;
        }
        if j < end && bytes[j] == quote {
            title = src[t_start..j].to_string();
            j += 1;
        }
        while j < end && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
    }
    if j >= end || bytes[j] != b')' {
        return None;
    }
    Some((label_end, url, title, j + 1))
}

// ============================================================================
// Post-parse Normalization & Semantic Finalization (`iv_markdown_parse_finalize.cpp`)
// ============================================================================

fn range_contains(outer: SourceRange, inner: SourceRange) -> bool {
    outer.available
        && inner.available
        && outer.start_offset <= inner.start_offset
        && outer.end_offset >= inner.end_offset
}

fn normalize_display_math_in_document(
    doc: &mut PreparedDocument,
    source: &[u8],
    line_starts: &[usize],
) {
    let mut display_indices: Vec<usize> = doc
        .formulas
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == MathKind::Display)
        .map(|(i, _)| i)
        .collect();
    if display_indices.is_empty() {
        return;
    }
    display_indices.sort_by_key(|&idx| (doc.formulas[idx].range.start_offset, idx));
    normalize_display_math_children(
        &mut doc.document,
        &doc.formulas,
        &display_indices,
        source,
        line_starts,
    );
}

fn normalize_display_math_children(
    node: &mut MarkdownNode,
    formulas: &[MathFormula],
    display_indices: &[usize],
    source: &[u8],
    line_starts: &[usize],
) {
    let original = std::mem::take(&mut node.children);
    let mut out = Vec::with_capacity(original.len() + display_indices.len());
    for mut child in original {
        let inside: Vec<usize> = display_indices
            .iter()
            .copied()
            .filter(|&idx| range_contains(child.range, formulas[idx].range))
            .collect();
        if inside.is_empty() {
            out.push(child);
            continue;
        }
        if child.kind == NodeKind::Paragraph {
            let mut cursor = child.range.start_offset;
            for f_idx in inside {
                let f = &formulas[f_idx];
                if cursor < f.range.start_offset {
                    let slice_str = String::from_utf8_lossy(&source[cursor..f.range.start_offset])
                        .trim()
                        .to_string();
                    if !slice_str.is_empty() {
                        let mut p = MarkdownNode::new(NodeKind::Paragraph);
                        p.range = range_for_offsets(
                            line_starts,
                            source.len(),
                            cursor,
                            f.range.start_offset,
                        );
                        let mut dummy_stats = ParseStats::default();
                        p.children = parse_inlines_standalone(&slice_str, &mut dummy_stats);
                        out.push(p);
                    }
                }
                let mut dm = MarkdownNode::new(NodeKind::DisplayMath);
                dm.range = f.range;
                dm.text = f.tex.clone();
                dm.formula_index = f_idx as i32;
                out.push(dm);
                cursor = cursor.max(f.range.end_offset);
            }
            if cursor < child.range.end_offset {
                let slice_str = String::from_utf8_lossy(&source[cursor..child.range.end_offset])
                    .trim()
                    .to_string();
                if !slice_str.is_empty() {
                    let mut p = MarkdownNode::new(NodeKind::Paragraph);
                    p.range = range_for_offsets(
                        line_starts,
                        source.len(),
                        cursor,
                        child.range.end_offset,
                    );
                    let mut dummy_stats = ParseStats::default();
                    p.children = parse_inlines_standalone(&slice_str, &mut dummy_stats);
                    out.push(p);
                }
            }
        } else {
            normalize_display_math_children(&mut child, formulas, &inside, source, line_starts);
            out.push(child);
        }
    }
    node.children = out;
}

fn attach_inline_formulas(node: &mut MarkdownNode, formulas: &[MathFormula]) {
    if node.children.is_empty() {
        return;
    }
    let original = std::mem::take(&mut node.children);
    let mut out = Vec::with_capacity(original.len());
    for mut child in original {
        if child.kind == NodeKind::Text && child.range.available {
            let matching: Vec<(usize, &MathFormula)> = formulas
                .iter()
                .enumerate()
                .filter(|(_, f)| f.kind == MathKind::Inline && range_contains(child.range, f.range))
                .collect();
            if matching.is_empty() {
                out.push(child);
                continue;
            }
            // Split the text around each `$...$` substring by matching tex delimiters.
            let mut remaining = child.text.as_str();
            for (f_idx, f) in matching {
                let needle = format!("${}$", f.tex);
                if let Some(pos) = remaining.find(&needle) {
                    if pos > 0 {
                        let mut t = MarkdownNode::new(NodeKind::Text);
                        t.text = remaining[..pos].to_string();
                        out.push(t);
                    }
                    let mut im = MarkdownNode::new(NodeKind::InlineMath);
                    im.range = f.range;
                    im.text = f.tex.clone();
                    im.formula_index = f_idx as i32;
                    out.push(im);
                    remaining = &remaining[pos + needle.len()..];
                }
            }
            if !remaining.is_empty() {
                let mut t = MarkdownNode::new(NodeKind::Text);
                t.text = remaining.to_string();
                out.push(t);
            }
        } else {
            attach_inline_formulas(&mut child, formulas);
            out.push(child);
        }
    }
    node.children = out;
}

fn finalize_document_semantics(doc: &mut PreparedDocument) {
    let mut heading_counts: HashMap<String, usize> = HashMap::new();
    assign_heading_anchors(&mut doc.document, &mut heading_counts, &mut doc.warnings);

    let mut fn_defs: HashMap<String, usize> = HashMap::new();
    let mut next_ordinal = 1usize;
    assign_footnote_definitions(
        &mut doc.document,
        &mut fn_defs,
        &mut next_ordinal,
        &mut doc.warnings,
    );
    assign_footnote_references(&mut doc.document, &fn_defs, &mut doc.warnings);

    let mut anchors = Vec::new();
    collect_anchor_ids(&doc.document, &mut anchors);
    validate_local_fragments(&doc.document, &anchors, &mut doc.warnings);
}

fn assign_heading_anchors(
    node: &mut MarkdownNode,
    counts: &mut HashMap<String, usize>,
    warnings: &mut Vec<String>,
) {
    if node.kind == NodeKind::Heading {
        let base = anchor_id_base_from_text(&node.plain_text());
        let entry = counts.entry(base.clone()).or_insert(0);
        *entry += 1;
        if *entry == 1 {
            node.anchor_id = base;
        } else {
            node.anchor_id = format!("{base}-{}", *entry);
            warnings.push(format!(
                "Duplicate heading anchor \"{base}\" remapped to \"{}\"",
                node.anchor_id
            ));
        }
    }
    for child in &mut node.children {
        assign_heading_anchors(child, counts, warnings);
    }
}

fn assign_footnote_definitions(
    node: &mut MarkdownNode,
    defs: &mut HashMap<String, usize>,
    next_ordinal: &mut usize,
    warnings: &mut Vec<String>,
) {
    if node.kind == NodeKind::FootnoteDefinition {
        if node.footnote_label.is_empty() {
            warnings.push(format!(
                "Footnote definition without label at {}:{}",
                node.range.start_line, node.range.start_column
            ));
        } else if let Some(&existing) = defs.get(&node.footnote_label) {
            node.footnote_ordinal = existing;
            node.anchor_id = format!("fn-{existing}");
            warnings.push(format!(
                "Duplicate footnote definition \"{}\"",
                node.footnote_label
            ));
        } else {
            let ord = *next_ordinal;
            *next_ordinal += 1;
            node.footnote_ordinal = ord;
            node.anchor_id = format!("fn-{ord}");
            defs.insert(node.footnote_label.clone(), ord);
        }
    }
    for child in &mut node.children {
        assign_footnote_definitions(child, defs, next_ordinal, warnings);
    }
}

fn assign_footnote_references(
    node: &mut MarkdownNode,
    defs: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
) {
    if node.kind == NodeKind::FootnoteReference {
        if node.footnote_label.is_empty() {
            warnings.push(format!(
                "Footnote reference without label at {}:{}",
                node.range.start_line, node.range.start_column
            ));
        } else if let Some(&ord) = defs.get(&node.footnote_label) {
            node.footnote_ordinal = ord;
        } else {
            warnings.push(format!(
                "Unresolved footnote reference \"{}\"",
                node.footnote_label
            ));
        }
    }
    for child in &mut node.children {
        assign_footnote_references(child, defs, warnings);
    }
}

fn collect_anchor_ids(node: &MarkdownNode, anchors: &mut Vec<String>) {
    if !node.anchor_id.is_empty()
        && matches!(node.kind, NodeKind::Heading | NodeKind::FootnoteDefinition)
    {
        anchors.push(node.anchor_id.clone());
    }
    for child in &node.children {
        collect_anchor_ids(child, anchors);
    }
}

fn validate_local_fragments(node: &MarkdownNode, anchors: &[String], warnings: &mut Vec<String>) {
    if node.kind == NodeKind::Link
        && let Some(frag_raw) = node.url.strip_prefix('#')
    {
        let frag = normalize_fragment_id(frag_raw);
        if frag.is_empty() || !anchors.contains(&frag) {
            warnings.push(format!("Unresolved local fragment \"{}\"", node.url));
        }
    }
    for child in &node.children {
        validate_local_fragments(child, anchors, warnings);
    }
}

fn count_nodes(node: &MarkdownNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

fn first_heading_title(node: &MarkdownNode) -> Option<String> {
    if node.kind == NodeKind::Heading {
        let t = node.plain_text().trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    for child in &node.children {
        if let Some(t) = first_heading_title(child) {
            return Some(t);
        }
    }
    None
}

// ============================================================================
// AST -> RichPage Conversion (`iv_markdown_prepare_blocks.cpp` / `_inline.cpp`)
// ============================================================================

/// Converts a parsed [`PreparedDocument`] into an editable [`RichPage`].
#[must_use]
pub fn prepared_document_to_rich_page(doc: &PreparedDocument) -> RichPage {
    let mut page = RichPage::default();
    convert_block_children(&doc.document.children, &mut page.blocks);
    page
}

fn convert_block_children(nodes: &[MarkdownNode], out: &mut Vec<RichBlock>) {
    for node in nodes {
        match node.kind {
            NodeKind::Heading => {
                let mut b = RichBlock::new(RichBlockKind::Heading);
                b.heading_level = node.heading_level.clamp(1, 6);
                b.anchor_id = node.anchor_id.clone();
                b.text = RichTextNode::from_text(inlines_to_text_with_entities(&node.children));
                out.push(b);
            }
            NodeKind::Paragraph => {
                let text = inlines_to_text_with_entities(&node.children);
                if !text.is_empty() {
                    let mut b = RichBlock::new(RichBlockKind::Paragraph);
                    b.text = RichTextNode::from_text(text);
                    out.push(b);
                }
            }
            NodeKind::CodeBlock => {
                let mut b = RichBlock::new(RichBlockKind::Code);
                let mut code = node.text.clone();
                if code.ends_with("\r\n") {
                    code.truncate(code.len() - 2);
                } else if code.ends_with('\n') || code.ends_with('\r') {
                    code.pop();
                }
                b.text = RichTextNode::plain(code);
                b.language = first_info_token(&node.info);
                out.push(b);
            }
            NodeKind::ThematicBreak => {
                out.push(RichBlock::new(RichBlockKind::Divider));
            }
            NodeKind::DisplayMath => {
                let mut b = RichBlock::new(RichBlockKind::Math);
                b.formula = trim_math_source(&node.text).to_string();
                out.push(b);
            }
            NodeKind::Blockquote => {
                let mut b = RichBlock::new(RichBlockKind::Quote);
                let mut nested = Vec::new();
                convert_block_children(&node.children, &mut nested);
                if nested.len() == 1 && nested[0].kind == RichBlockKind::Paragraph {
                    let first = nested.remove(0);
                    b.text = first.text;
                } else {
                    b.blocks = nested;
                }
                out.push(b);
            }
            NodeKind::List => {
                let mut b = RichBlock::new(RichBlockKind::List);
                b.list_kind = match node.list_kind {
                    ListKind::Bullet => RichListKind::Bullet,
                    ListKind::Ordered => RichListKind::Ordered,
                };
                if b.list_kind == RichListKind::Ordered && node.list_start != 1 {
                    b.ordered_list = OrderedListData {
                        reversed: false,
                        start: Some(node.list_start),
                        list_type: None,
                    };
                }
                for item_node in &node.children {
                    if item_node.kind != NodeKind::ListItem {
                        continue;
                    }
                    let mut item = RichListItem {
                        task_state: match item_node.task_state {
                            MarkdownTaskState::None => RichTaskState::None,
                            MarkdownTaskState::Unchecked => RichTaskState::Unchecked,
                            MarkdownTaskState::Checked => RichTaskState::Checked,
                        },
                        ..RichListItem::default()
                    };
                    let mut item_blocks = Vec::new();
                    convert_block_children(&item_node.children, &mut item_blocks);
                    if item_blocks.len() == 1 && item_blocks[0].kind == RichBlockKind::Paragraph {
                        let p = item_blocks.remove(0);
                        item.text = p.text;
                    } else {
                        item.blocks = item_blocks;
                    }
                    b.list_items.push(item);
                }
                out.push(b);
            }
            NodeKind::Table => {
                let mut b = RichBlock::new(RichBlockKind::Table);
                b.bordered = true;
                for row_node in &node.children {
                    if row_node.kind != NodeKind::TableRow {
                        continue;
                    }
                    let mut cells = Vec::with_capacity(row_node.children.len());
                    for (col, cell_node) in row_node.children.iter().enumerate() {
                        let align = node
                            .table_alignments
                            .get(col)
                            .copied()
                            .unwrap_or(MarkdownTableAlignment::None);
                        cells.push(RichTableCell {
                            text: RichTextNode::from_text(inlines_to_text_with_entities(
                                &cell_node.children,
                            )),
                            colspan: 1,
                            rowspan: 1,
                            header: row_node.table_header,
                            alignment: match align {
                                MarkdownTableAlignment::Center => RichTableAlignment::Center,
                                MarkdownTableAlignment::Right => RichTableAlignment::Right,
                                MarkdownTableAlignment::Left | MarkdownTableAlignment::None => {
                                    RichTableAlignment::Left
                                }
                            },
                            vertical_alignment: crate::rich_page::RichTableVerticalAlignment::Top,
                        });
                    }
                    b.table_rows.push(RichTableRow { cells });
                }
                out.push(b);
            }
            NodeKind::HtmlBlock if node.html_block_kind == HtmlBlockKind::Details => {
                let mut b = RichBlock::new(RichBlockKind::Details);
                b.open = node.details_open;
                b.text = RichTextNode::plain(node.details_summary.clone());
                if !node.details_body.is_empty() {
                    let sub = parse_markdown_for_iv(
                        node.details_body.as_bytes(),
                        ParseOptions::default(),
                    );
                    if sub.ok {
                        convert_block_children(&sub.document.document.children, &mut b.blocks);
                    } else {
                        b.blocks
                            .push(RichBlock::paragraph(node.details_body.clone()));
                    }
                }
                out.push(b);
            }
            NodeKind::FootnoteDefinition => {
                // Represented as a footer paragraph anchored to `fn-<ordinal>`
                let mut b = RichBlock::new(RichBlockKind::Footer);
                b.anchor_id = node.anchor_id.clone();
                let mut text = TextWithEntities::simple(format!("[{}] ", node.footnote_label));
                let mut inner_blocks = Vec::new();
                convert_block_children(&node.children, &mut inner_blocks);
                for (idx, ib) in inner_blocks.into_iter().enumerate() {
                    if idx > 0 {
                        text.push_str("\n");
                    }
                    text.append(ib.text.text);
                }
                b.text = RichTextNode::from_text(text);
                out.push(b);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RawInlineTag {
    None,
    SubOpen,
    SubClose,
    SupOpen,
    SupClose,
    MarkOpen,
    MarkClose,
    UnderlineOpen,
    UnderlineClose,
}

fn parse_raw_inline_tag(node: &MarkdownNode) -> RawInlineTag {
    if node.kind != NodeKind::HtmlInline {
        return RawInlineTag::None;
    }
    match node.raw.to_ascii_lowercase().as_str() {
        "<sub>" => RawInlineTag::SubOpen,
        "</sub>" => RawInlineTag::SubClose,
        "<sup>" => RawInlineTag::SupOpen,
        "</sup>" => RawInlineTag::SupClose,
        "<mark>" => RawInlineTag::MarkOpen,
        "</mark>" => RawInlineTag::MarkClose,
        "<u>" => RawInlineTag::UnderlineOpen,
        "</u>" => RawInlineTag::UnderlineClose,
        _ => RawInlineTag::None,
    }
}

fn matching_closing_tag(tag: RawInlineTag) -> RawInlineTag {
    match tag {
        RawInlineTag::SubOpen => RawInlineTag::SubClose,
        RawInlineTag::SupOpen => RawInlineTag::SupClose,
        RawInlineTag::MarkOpen => RawInlineTag::MarkClose,
        RawInlineTag::UnderlineOpen => RawInlineTag::UnderlineClose,
        _ => RawInlineTag::None,
    }
}

fn entity_for_raw_tag(tag: RawInlineTag) -> Option<EntityType> {
    match tag {
        RawInlineTag::SubOpen => Some(EntityType::Subscript),
        RawInlineTag::SupOpen => Some(EntityType::Superscript),
        RawInlineTag::MarkOpen => Some(EntityType::Marked),
        RawInlineTag::UnderlineOpen => Some(EntityType::Underline),
        _ => None,
    }
}

/// Linear stack-based matcher ported verbatim from `ComputeMatchingRawInlineTags`
/// (`iv_markdown_prepare_inline.cpp:295-316`).
fn compute_matching_raw_inline_tags(
    nodes: &[MarkdownNode],
    from: usize,
    till: usize,
) -> Vec<Option<usize>> {
    let mut matches = vec![None; nodes.len()];
    let mut pending: Vec<(usize, RawInlineTag)> = Vec::new();
    for (i, node) in nodes.iter().enumerate().take(till).skip(from) {
        let tag = parse_raw_inline_tag(node);
        match tag {
            RawInlineTag::None => {}
            RawInlineTag::SubOpen
            | RawInlineTag::SupOpen
            | RawInlineTag::MarkOpen
            | RawInlineTag::UnderlineOpen => pending.push((i, tag)),
            _ => {
                if let Some(&(open_idx, open_tag)) = pending.last()
                    && matching_closing_tag(open_tag) == tag
                {
                    matches[open_idx] = Some(i);
                    pending.pop();
                } else {
                    pending.clear();
                }
            }
        }
    }
    matches
}

fn inlines_to_text_with_entities(nodes: &[MarkdownNode]) -> TextWithEntities {
    let mut out = TextWithEntities::default();
    let matches = compute_matching_raw_inline_tags(nodes, 0, nodes.len());
    append_inline_range(nodes, 0, nodes.len(), &matches, &mut out);
    out.sort_entities();
    out
}

fn append_inline_range(
    nodes: &[MarkdownNode],
    from: usize,
    till: usize,
    matches: &[Option<usize>],
    out: &mut TextWithEntities,
) {
    let mut i = from;
    while i < till {
        let node = &nodes[i];
        let tag = parse_raw_inline_tag(node);
        if let Some(kind) = entity_for_raw_tag(tag)
            && let Some(close_idx) = matches.get(i).copied().flatten()
            && close_idx > i
            && close_idx < till
        {
            let entity_from = out.utf16_len();
            append_inline_range(nodes, i + 1, close_idx, matches, out);
            let len = out.utf16_len().saturating_sub(entity_from);
            if len > 0 {
                out.entities.push(EntityInText::new(kind, entity_from, len));
            }
            i = close_idx + 1;
            continue;
        }
        if node.kind == NodeKind::HtmlInline
            && matches!(
                node.raw.to_ascii_lowercase().as_str(),
                "<br>" | "<br/>" | "<br />"
            )
        {
            out.push_str("\n");
            if i + 1 < till && nodes[i + 1].kind == NodeKind::SoftBreak {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        append_inline_node(node, out);
        i += 1;
    }
}

fn append_inline_node(node: &MarkdownNode, out: &mut TextWithEntities) {
    let from = out.utf16_len();
    match node.kind {
        NodeKind::Text => out.push_str(&node.text),
        NodeKind::SoftBreak | NodeKind::LineBreak => out.push_str("\n"),
        NodeKind::InlineMath => {
            let trimmed = trim_math_source(&node.text).to_string();
            let payload = serialize_inline_text_object_entity(&InlineTextObjectEntity::Formula(
                InlineTextObjectFormulaData {
                    copy_source: inline_formula_copy_source(&trimmed),
                    trimmed_tex: trimmed,
                },
            ));
            out.text.push(OBJECT_REPLACEMENT_CHAR);
            out.entities.push(EntityInText::with_data(
                EntityType::CustomEmoji,
                from,
                1,
                payload,
            ));
        }
        NodeKind::Emphasis => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::Italic, from, len));
            }
        }
        NodeKind::Strong => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::Bold, from, len));
            }
        }
        NodeKind::Strike => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::StrikeOut, from, len));
            }
        }
        NodeKind::Spoiler => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::Spoiler, from, len));
            }
        }
        NodeKind::InlineCode => {
            out.push_str(&node.text);
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::Code, from, len));
            }
        }
        NodeKind::Link => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
            if out.utf16_len() == from && !node.url.is_empty() {
                out.push_str(&node.url);
            }
            let len = out.utf16_len().saturating_sub(from);
            if len > 0 && !node.url.is_empty() {
                out.entities.push(EntityInText::with_data(
                    EntityType::CustomUrl,
                    from,
                    len,
                    node.url.clone(),
                ));
            }
        }
        NodeKind::FootnoteReference => {
            let label = if !node.footnote_label.is_empty() {
                node.footnote_label.as_str()
            } else {
                "?"
            };
            let display = format!("[{label}]");
            out.push_str(&display);
            let len = display.encode_utf16().count();
            if node.footnote_ordinal > 0 {
                out.entities
                    .push(EntityInText::new(EntityType::Superscript, from, len));
                out.entities.push(EntityInText::with_data(
                    EntityType::CustomUrl,
                    from,
                    len,
                    format!("#fn-{}", node.footnote_ordinal),
                ));
            }
        }
        NodeKind::HtmlInline | NodeKind::Unsupported => {
            if !node.raw.is_empty() {
                out.push_str(&node.raw);
            } else if !node.text.is_empty() {
                out.push_str(&node.text);
            }
        }
        _ => {
            let m = compute_matching_raw_inline_tags(&node.children, 0, node.children.len());
            append_inline_range(&node.children, 0, node.children.len(), &m, out);
        }
    }
}
