//! Native Markdown + Math subsystem (`Telegram/SourceFiles/iv/markdown/*`).
//!
//! - [`document`]: Markdown AST, formula records, parse/prepare limits
//! - [`validate`]: Source validation (size, BOM, binary check, UTF-8)
//! - [`math_extract`]: `$...$` / `$$...$$` formula extraction with currency & prose heuristics
//! - [`parser`]: Pure-Rust GFM + Math + Telegram Markdown parser & `PreparedDocument -> RichPage`
//! - [`serializer`]: Canonical `RichPage -> Markdown` serializer
//! - [`formula`]: Pure-Rust MicroTeX-equivalent math box/glyph layout engine & LRU cache

pub mod document;
pub mod formula;
pub mod math_extract;
pub mod parser;
pub mod serializer;
pub mod validate;

pub use document::{
    HtmlBlockKind, ListDelimiter, ListKind, MarkdownNode, MarkdownParseLimits,
    MarkdownPrepareLimits, MarkdownPrepareTableRenderLimits, MarkdownSourceValidationResult,
    MarkdownTableAlignment, MarkdownTaskState, MathFormula, MathKind, NodeKind, ParseOptions,
    ParseResult, ParseStats, PreparedDocument, SourceRange, ValidatedMarkdownSource,
};
pub use formula::{
    DEFAULT_FORMULA_CACHE_BUDGET_BYTES, FORMULA_EXACT_METRIC_SCALE, FormulaCache, FormulaCacheKey,
    FormulaCacheMutation, FormulaDebugCounters, FormulaExactMetrics, FormulaLayout,
    FormulaLayoutRequest, FormulaPolyline, FormulaRule, MAX_FORMULA_TEX_LENGTH, MathFontStyle,
    MathLayoutEngine, MeasuredFormula, PositionedGlyphRun, RenderedFormula, layout_formula,
    measure_formula, rewrite_full_width_environments,
};
pub use math_extract::{
    MathScanBlock, build_line_starts, extract_math_regions, has_strong_math_signal,
    looks_like_currency, looks_like_prose, range_for_offsets, range_from_line_columns,
    strip_blockquote_markers,
};
pub use parser::{
    anchor_id_base_from_text, decode_markdown_escapes, first_info_token, markdown_to_rich_page,
    parse_markdown_for_iv, parse_validated_markdown, prepared_document_to_rich_page,
};
pub use serializer::{rich_page_to_markdown, serialize_blocks_to_markdown, serialize_inline_text};
pub use validate::{looks_binary, validate_markdown_source_for_iv};
