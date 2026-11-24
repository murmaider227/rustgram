//! Port of `iv/markdown/iv_markdown_document.h`, `iv/markdown/iv_markdown_common.h`,
//! `iv/markdown/iv_markdown_parse.h`, and `iv/markdown/iv_markdown_prepare.cpp` (limit constants).
//!
//! Defines the Markdown AST (`MarkdownNode`), math formula metadata (`MathFormula`),
//! parse limits, and prepare limits used by Rustgram's native Markdown renderer and rich editor.

use serde::{Deserialize, Serialize};

/// Exact parsing limits from `Iv::Markdown::ParseLimitsForIv()` (`iv_markdown_parse.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownParseLimits {
    /// Maximum source size in bytes (`4 MiB`).
    pub max_source_bytes: usize,
    /// Maximum parsed AST nodes (`100_000`).
    pub max_cmark_nodes: usize,
    /// Maximum AST nesting depth (`128`).
    pub max_nesting: usize,
    /// Maximum single formula size in bytes (`64 KiB`).
    pub max_formula_bytes: usize,
    /// Maximum formula count per document (`10_000`).
    pub max_formula_count: usize,
}

impl MarkdownParseLimits {
    /// Default Instant View Markdown parse limits matching `ParseLimitsForIv()`.
    pub const IV_DEFAULT: Self = Self {
        max_source_bytes: 4 * 1024 * 1024,
        max_cmark_nodes: 100_000,
        max_nesting: 128,
        max_formula_bytes: 64 * 1024,
        max_formula_count: 10_000,
    };
}

impl Default for MarkdownParseLimits {
    fn default() -> Self {
        Self::IV_DEFAULT
    }
}

/// Table render limits (`Iv::Markdown::MarkdownPrepareTableRenderLimits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownPrepareTableRenderLimits {
    /// Maximum rendered rows.
    pub max_rows: usize,
    /// Maximum rendered columns.
    pub max_columns: usize,
    /// Maximum total rendered cells.
    pub max_cells: usize,
}

/// Prepare limits matching `Iv::Markdown::PrepareLimitsForIv()` (`iv_markdown_prepare.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownPrepareLimits {
    /// Native Instant View table render limits (`128 x 20`, `1024` cells).
    pub table_render: MarkdownPrepareTableRenderLimits,
    /// Markdown article table render limits (`1000 x 50`, `50_000` cells).
    pub markdown_table_render: MarkdownPrepareTableRenderLimits,
    /// Visual nesting cap for lists (`16`).
    pub visual_list_depth: usize,
    /// Visual nesting cap for blockquotes (`16`).
    pub visual_quote_depth: usize,
    /// Soft cap on prepared blocks (`4096`).
    pub max_prepared_blocks: usize,
}

impl MarkdownPrepareLimits {
    /// Default limits matching `PrepareLimitsForIv()`.
    pub const IV_DEFAULT: Self = Self {
        table_render: MarkdownPrepareTableRenderLimits {
            max_rows: 128,
            max_columns: 20,
            max_cells: 1024,
        },
        markdown_table_render: MarkdownPrepareTableRenderLimits {
            max_rows: 1_000,
            max_columns: 50,
            max_cells: 50_000,
        },
        visual_list_depth: 16,
        visual_quote_depth: 16,
        max_prepared_blocks: 4096,
    };
}

impl Default for MarkdownPrepareLimits {
    fn default() -> Self {
        Self::IV_DEFAULT
    }
}

/// Options passed to the Markdown parser (`Iv::Markdown::ParseOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParseOptions {
    /// Optional logical source name (used in diagnostics / document title fallback).
    pub source_name: String,
    /// Optional custom limits (defaults to [`MarkdownParseLimits::IV_DEFAULT`]).
    pub limits: MarkdownParseLimits,
}

/// Kind of node in a parsed Markdown document (`Iv::Markdown::NodeKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum NodeKind {
    /// Root document container.
    Document,
    /// Body paragraph.
    Paragraph,
    /// Heading (levels 1..=6).
    Heading,
    /// Plain text leaf.
    Text,
    /// Italic / emphasis span.
    Emphasis,
    /// Bold / strong span.
    Strong,
    /// Strikethrough span (`~~...~~`).
    Strike,
    /// Inline monospace code span (`` `...` ``).
    InlineCode,
    /// Fenced or indented code block.
    CodeBlock,
    /// Hyperlink (`[label](url)` or autolink).
    Link,
    /// Bullet or ordered list container.
    List,
    /// Single list item.
    ListItem,
    /// Blockquote container.
    Blockquote,
    /// Horizontal thematic break (`---`).
    ThematicBreak,
    /// GFM pipe table.
    Table,
    /// GFM table row.
    TableRow,
    /// GFM table cell.
    TableCell,
    /// Raw inline HTML tag (e.g. `<sub>`, `<sup>`, `<mark>`, `<u>`, `<br>`).
    HtmlInline,
    /// Raw HTML block (comment, `<details>`, or unsupported HTML).
    HtmlBlock,
    /// Footnote reference (`[^label]`).
    FootnoteReference,
    /// Footnote definition (`[^label]: ...`).
    FootnoteDefinition,
    /// Display math block (`$$...$$`).
    DisplayMath,
    /// Inline math formula (`$...$`).
    InlineMath,
    /// Soft line break.
    SoftBreak,
    /// Explicit hard line break (`\` or `  \n` or `<br>`).
    LineBreak,
    /// Telegram spoiler span (`||...||`).
    Spoiler,
    /// Unsupported node.
    #[default]
    Unsupported,
}

/// Inline vs display math (`Iv::Markdown::MathKind`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub enum MathKind {
    /// Inline formula (`$...$`).
    Inline,
    /// Display block formula (`$$...$$`).
    #[default]
    Display,
}

/// Bullet vs ordered list (`Iv::Markdown::ListKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ListKind {
    /// Bullet / unordered list.
    #[default]
    Bullet,
    /// Ordered list.
    Ordered,
}

/// Ordered list delimiter style (`Iv::Markdown::ListDelimiter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ListDelimiter {
    /// No delimiter (bullet lists).
    #[default]
    None,
    /// Period delimiter (`1.`).
    Period,
    /// Closing parenthesis delimiter (`1)`).
    Parenthesis,
}

/// Task checkbox state (`Iv::Markdown::TaskState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkdownTaskState {
    /// Not a task item.
    #[default]
    None,
    /// Unchecked (`[ ]`).
    Unchecked,
    /// Checked (`[x]` / `[X]`).
    Checked,
}

/// Column alignment in a GFM Markdown table (`Iv::Markdown::TableAlignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkdownTableAlignment {
    /// Unspecified alignment.
    #[default]
    None,
    /// Left-aligned (`:---`).
    Left,
    /// Center-aligned (`:---:`).
    Center,
    /// Right-aligned (`---:`).
    Right,
}

/// Classification of an HTML block node (`Iv::Markdown::HtmlBlockKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HtmlBlockKind {
    /// Not an HTML block.
    #[default]
    None,
    /// HTML comment (`<!-- ... -->`).
    Comment,
    /// Valid `<details><summary>...</summary>...</details>` disclosure block.
    Details,
    /// Unsupported HTML block.
    Unsupported,
}

/// 1-based line/column and 0-based byte offset span in normalized UTF-8 source (`Iv::Markdown::SourceRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceRange {
    /// Whether this range is populated.
    pub available: bool,
    /// 1-based start line.
    pub start_line: usize,
    /// 1-based start column (bytes).
    pub start_column: usize,
    /// 1-based end line.
    pub end_line: usize,
    /// 1-based end column (inclusive, bytes).
    pub end_column: usize,
    /// 0-based inclusive start byte offset.
    pub start_offset: usize,
    /// 0-based exclusive end byte offset.
    pub end_offset: usize,
}

/// Parsed Markdown AST node (`Iv::Markdown::MarkdownNode`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MarkdownNode {
    /// Node discriminant.
    pub kind: NodeKind,
    /// Source span in the normalized UTF-8 document.
    pub range: SourceRange,
    /// Leaf text content (text, code, math source, breaks).
    pub text: String,
    /// Target URL (for `Link`).
    pub url: String,
    /// Optional title (for `Link`).
    pub title: String,
    /// Fence info string (for `CodeBlock`).
    pub info: String,
    /// Raw literal content (HTML, footnote raw text).
    pub raw: String,
    /// Assigned anchor ID (for `Heading` and `FootnoteDefinition`).
    pub anchor_id: String,
    /// Footnote label (without `[^` / `]`).
    pub footnote_label: String,
    /// Summary text of a `<details>` block.
    pub details_summary: String,
    /// Raw Markdown body of a `<details>` block.
    pub details_body: String,
    /// Description of an unsupported construct.
    pub unsupported_kind: String,
    /// Child AST nodes.
    pub children: Vec<MarkdownNode>,
    /// Column alignments (for `Table`).
    pub table_alignments: Vec<MarkdownTableAlignment>,
    /// Heading level `1..=6` (for `Heading`).
    pub heading_level: u8,
    /// Starting ordinal (for ordered `List`).
    pub list_start: i32,
    /// 0-based column index (for `TableCell`).
    pub table_column: i32,
    /// 0-based index into `PreparedDocument::formulas` (`-1` if none).
    pub formula_index: i32,
    /// 1-based footnote ordinal (`0` if unresolved).
    pub footnote_ordinal: usize,
    /// Bullet vs ordered (for `List`).
    pub list_kind: ListKind,
    /// Delimiter kind (for ordered `List`).
    pub list_delimiter: ListDelimiter,
    /// Task state (for `ListItem`).
    pub task_state: MarkdownTaskState,
    /// HTML block classification.
    pub html_block_kind: HtmlBlockKind,
    /// Tight list flag.
    pub tight: bool,
    /// True if a `Link` was produced by an autolink.
    pub autolink: bool,
    /// True if a `TableRow` is the header row.
    pub table_header: bool,
    /// True if a `<details>` block has the `open` attribute.
    pub details_open: bool,
}

impl MarkdownNode {
    /// Creates an empty node of the given [`NodeKind`].
    #[must_use]
    pub fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            table_column: -1,
            formula_index: -1,
            ..Self::default()
        }
    }

    /// Recursively collects plain text from this node and its descendants.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        self.collect_plain(&mut out);
        out
    }

    fn collect_plain(&self, out: &mut String) {
        out.push_str(&self.text);
        for child in &self.children {
            child.collect_plain(out);
        }
    }
}

/// Extracted TeX math formula region (`Iv::Markdown::MathFormula`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MathFormula {
    /// 1-based formula ordinal in the document.
    pub index: usize,
    /// Inline vs display math.
    pub kind: MathKind,
    /// Extracted TeX source (without `$` / `$$` delimiters).
    pub tex: String,
    /// Source span including delimiters.
    pub range: SourceRange,
    /// Name of the narrowest enclosing block kind (e.g. `"paragraph"`, `"block_quote"`).
    pub parent_node_kind: String,
}

/// Statistics captured during Markdown parsing (`Iv::Markdown::ParseStats`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParseStats {
    /// Total raw nodes encountered.
    pub cmark_node_count: usize,
    /// Total converted AST nodes after normalization.
    pub converted_node_count: usize,
    /// Maximum AST depth observed.
    pub max_depth: usize,
    /// Number of inline `$...$` formulas extracted.
    pub inline_formula_count: usize,
    /// Number of display `$$...$$` formulas extracted.
    pub display_formula_count: usize,
    /// Whether any GFM table was parsed.
    pub tables_seen: bool,
    /// Whether any GFM task list was parsed.
    pub task_lists_seen: bool,
    /// Whether any strikethrough span was parsed.
    pub strikethrough_seen: bool,
    /// Whether any autolink was parsed.
    pub autolinks_seen: bool,
    /// Whether any footnote reference/definition was parsed.
    pub footnotes_seen: bool,
}

/// Parsed Markdown document (`Iv::Markdown::PreparedDocument`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreparedDocument {
    /// Source identifier.
    pub source_name: String,
    /// Document title (extracted from the first heading or `source_name`).
    pub title: String,
    /// Validated UTF-8 source text.
    pub source_text: String,
    /// Root `NodeKind::Document` AST node.
    pub document: MarkdownNode,
    /// All extracted inline and display math formulas.
    pub formulas: Vec<MathFormula>,
    /// Parse statistics.
    pub stats: ParseStats,
    /// Non-fatal semantic warnings (duplicate anchors, unresolved fragments/footnotes, malformed HTML).
    pub warnings: Vec<String>,
    /// True if the document has no content.
    pub empty: bool,
}

impl PreparedDocument {
    /// Creates an empty [`PreparedDocument`] (`Iv::Markdown::EmptyDocument`).
    #[must_use]
    pub fn empty(source_name: impl Into<String>) -> Self {
        let name = source_name.into();
        Self {
            title: name.clone(),
            source_name: name,
            document: MarkdownNode::new(NodeKind::Document),
            empty: true,
            ..Self::default()
        }
    }
}

/// Result of parsing Markdown (`Iv::Markdown::ParseResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseResult {
    /// Parsed document (empty when `ok == false`).
    pub document: PreparedDocument,
    /// Machine-readable error code when `ok == false`.
    pub error: String,
    /// True on success.
    pub ok: bool,
}

/// Validated UTF-8 Markdown source (`Iv::Markdown::ValidatedMarkdownSource`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidatedMarkdownSource {
    /// BOM-stripped UTF-8 bytes.
    pub normalized: Vec<u8>,
    /// Decoded UTF-8 string.
    pub decoded: String,
    /// Byte offsets where each 1-based line begins.
    pub line_starts: Vec<usize>,
    /// Logical source name.
    pub source_name: String,
}

/// Output of [`crate::markdown::validate::validate_markdown_source_for_iv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownSourceValidationResult {
    /// Validated source (meaningful when `ok == true`).
    pub source: ValidatedMarkdownSource,
    /// Machine-readable error code when `ok == false`.
    pub error: String,
    /// True when validation succeeded.
    pub ok: bool,
}
