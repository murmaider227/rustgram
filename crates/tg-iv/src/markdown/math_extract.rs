//! Port of `iv/markdown/iv_markdown_math.h` and `iv/markdown/iv_markdown_math.cpp`.
//!
//! Scans Markdown source for `$...$` inline math and `$$...$$` display math regions while
//! skipping masked regions (code blocks, inline code, HTML), backslash escapes, currency
//! amounts (`$100`), and ordinary prose spans.

use crate::markdown::document::{MathFormula, MathKind, SourceRange};

/// Block range hint used to tag extracted formulas with their enclosing block kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathScanBlock {
    /// Block span in normalized UTF-8 source.
    pub range: SourceRange,
    /// Block kind label (e.g. `"paragraph"`, `"block_quote"`).
    pub node_kind: String,
}

/// Computes the byte offset where each 1-based line begins (`BuildLineStarts`).
#[must_use]
pub fn build_line_starts(source: &[u8]) -> Vec<usize> {
    let mut result = vec![0usize];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            result.push(i + 1);
        }
    }
    result
}

fn offset_to_position(line_starts: &[usize], offset: usize) -> (usize, usize) {
    if line_starts.is_empty() {
        return (1, offset + 1);
    }
    let idx = line_starts.partition_point(|&start| start <= offset);
    if idx == 0 {
        (1, offset + 1)
    } else {
        let line_idx = idx - 1;
        (
            line_idx + 1,
            offset.saturating_sub(line_starts[line_idx]) + 1,
        )
    }
}

/// Converts 1-based `(line, column)` bounds into a [`SourceRange`] (`RangeFromLineColumns`).
#[must_use]
pub fn range_from_line_columns(
    line_starts: &[usize],
    source_size: usize,
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
) -> SourceRange {
    let mut result = SourceRange {
        available: false,
        start_line,
        start_column,
        end_line,
        end_column,
        start_offset: 0,
        end_offset: 0,
    };
    let lines_count = line_starts.len();
    if start_line == 0 || end_line == 0 || start_line > lines_count || end_line > lines_count {
        return result;
    }
    let start_offset = line_starts[start_line - 1]
        .saturating_add(start_column.saturating_sub(1))
        .min(source_size);
    let end_offset = line_starts[end_line - 1]
        .saturating_add(end_column)
        .min(source_size);
    result.available = true;
    result.start_offset = start_offset;
    result.end_offset = end_offset;
    result
}

/// Converts 0-based byte offsets `[start_offset, end_offset)` into a [`SourceRange`] (`RangeForOffsets`).
#[must_use]
pub fn range_for_offsets(
    line_starts: &[usize],
    source_size: usize,
    start_offset: usize,
    end_offset: usize,
) -> SourceRange {
    let (start_line, start_column) = offset_to_position(line_starts, start_offset);
    let last_offset = end_offset.saturating_sub(1).max(start_offset);
    let (end_line, end_column) = offset_to_position(line_starts, last_offset);
    range_from_line_columns(
        line_starts,
        source_size,
        start_line,
        start_column,
        end_line,
        end_column,
    )
}

fn find_parent_block(blocks: &[MathScanBlock], line: usize, column: usize) -> String {
    let mut best = "document".to_string();
    let mut best_span = usize::MAX;
    for block in blocks {
        let r = &block.range;
        if !r.available {
            continue;
        }
        if line < r.start_line || line > r.end_line {
            continue;
        }
        if line == r.start_line && column < r.start_column {
            continue;
        }
        if line == r.end_line && column > r.end_column {
            continue;
        }
        let span = (r.end_line - r.start_line)
            .saturating_mul(1000)
            .saturating_add(r.end_column.saturating_sub(r.start_column));
        if span < best_span {
            best_span = span;
            best = block.node_kind.clone();
        }
    }
    best
}

/// Returns true if an inline `$...$` span looks like a currency figure (`LooksLikeCurrency`).
#[must_use]
pub fn looks_like_currency(content: &[u8]) -> bool {
    if content.is_empty() {
        return true;
    }
    let mut has_digit = false;
    for &ch in content {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(ch, b'.' | b',' | b' ') {
            return false;
        }
    }
    has_digit
}

/// Returns true if `content` contains a strong TeX math indicator (`HasStrongMathSignal`).
#[must_use]
pub fn has_strong_math_signal(content: &[u8]) -> bool {
    let has_visible_neighbors = |pos: usize| -> bool {
        let left = content[..pos]
            .iter()
            .rposition(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'));
        let right = content[pos + 1..]
            .iter()
            .position(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'));
        left.is_some() && right.is_some()
    };
    for (i, &ch) in content.iter().enumerate() {
        if ch == b'\\' && i + 1 < content.len() {
            let next = content[i + 1];
            if next.is_ascii_alphabetic() || matches!(next, b',' | b':' | b';' | b'!') {
                return true;
            }
        } else if matches!(ch, b'^' | b'_' | b'+' | b'=' | b'<' | b'>') && has_visible_neighbors(i)
        {
            return true;
        }
    }
    false
}

/// Returns true if `content` resembles ordinary English prose rather than math (`LooksLikeProse`).
#[must_use]
pub fn looks_like_prose(content: &[u8]) -> bool {
    let mut alpha_words = 0usize;
    let mut longest_run = 0usize;
    let mut i = 0usize;
    while i < content.len() {
        while i < content.len() && !content[i].is_ascii_alphabetic() {
            i += 1;
        }
        let start = i;
        while i < content.len() && content[i].is_ascii_alphabetic() {
            i += 1;
        }
        let len = i - start;
        if len >= 2 && (start == 0 || content[start - 1] != b'\\') {
            alpha_words += 1;
            longest_run = longest_run.max(len);
        }
    }
    alpha_words >= 2 || longest_run >= 5
}

/// Strips leading `>` blockquote markers from continuation lines inside `$$...$$` (`StripBlockquoteMarkers`).
#[must_use]
pub fn strip_blockquote_markers(content: &[u8]) -> Vec<u8> {
    let mut cleaned = Vec::with_capacity(content.len());
    let mut pos = 0usize;
    while pos < content.len() {
        let mut eol = pos;
        while eol < content.len() && content[eol] != b'\n' {
            eol += 1;
        }
        let mut start = pos;
        if pos > 0 {
            while start < eol {
                while start < eol && matches!(content[start], b' ' | b'\t') {
                    start += 1;
                }
                if start < eol && content[start] == b'>' {
                    start += 1;
                    continue;
                }
                break;
            }
            if start < eol && content[start] == b' ' {
                start += 1;
            }
        }
        cleaned.extend_from_slice(&content[start..eol]);
        if eol < content.len() {
            cleaned.push(b'\n');
        }
        pos = eol + 1;
    }
    cleaned
}

fn is_masked(mask: &[bool], offset: usize) -> bool {
    mask.get(offset).copied().unwrap_or(false)
}

/// Extracts all `$...$` and `$$...$$` math regions from `source` (`ExtractMathRegions`).
///
/// Returns `Err("formula-too-large")` or `Err("too-many-formulas")` when limits are exceeded.
pub fn extract_math_regions(
    source: &[u8],
    mask: &[bool],
    line_starts: &[usize],
    blocks: &[MathScanBlock],
    max_formula_bytes: usize,
    max_formula_count: usize,
) -> Result<Vec<MathFormula>, String> {
    let mut formulas = Vec::new();
    let size = source.len();
    let mut i = 0usize;
    while i < size {
        if is_masked(mask, i) || source[i] != b'$' {
            i += 1;
            continue;
        }
        if i > 0 && source[i - 1] == b'\\' {
            i += 1;
            continue;
        }
        let display = i + 1 < size && source[i + 1] == b'$' && !is_masked(mask, i + 1);
        let delim_size = if display { 2 } else { 1 };
        let content_start = i + delim_size;
        let mut j = content_start;
        let mut closing = None;
        while j < size {
            if is_masked(mask, j) {
                j += 1;
                continue;
            }
            if source[j] == b'\\' {
                j = j.saturating_add(2);
                continue;
            }
            if !display && source[j] == b'\n' {
                break;
            }
            if !display && source[j] == b'$' {
                closing = Some(j);
                break;
            }
            if display
                && source[j] == b'$'
                && j + 1 < size
                && source[j + 1] == b'$'
                && !is_masked(mask, j + 1)
            {
                closing = Some(j);
                break;
            }
            j += 1;
        }
        let Some(close_pos) = closing else {
            i += 1;
            continue;
        };
        let raw_content = &source[content_start..close_pos];
        if !display && looks_like_currency(raw_content) {
            i += 1;
            continue;
        }
        if !display && !has_strong_math_signal(raw_content) && looks_like_prose(raw_content) {
            i += 1;
            continue;
        }
        let content = if display {
            strip_blockquote_markers(raw_content)
        } else {
            raw_content.to_vec()
        };
        if content.len() > max_formula_bytes {
            return Err("formula-too-large".to_string());
        }
        if formulas.len() >= max_formula_count {
            return Err("too-many-formulas".to_string());
        }
        let (start_line, start_column) = offset_to_position(line_starts, i);
        let range = range_for_offsets(line_starts, size, i, close_pos + delim_size);
        let tex = String::from_utf8_lossy(&content).into_owned();
        formulas.push(MathFormula {
            index: formulas.len() + 1,
            kind: if display {
                MathKind::Display
            } else {
                MathKind::Inline
            },
            tex,
            range,
            parent_node_kind: find_parent_block(blocks, start_line, start_column),
        });
        i = close_pos + delim_size;
    }
    Ok(formulas)
}
