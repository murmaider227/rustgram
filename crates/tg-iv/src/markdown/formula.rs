//! Port of `iv/markdown/iv_markdown_microtex.h`, `iv/markdown/iv_markdown_microtex.cpp`,
//! `iv/markdown/iv_markdown_math_renderer.h`, and `iv/markdown/iv_markdown_math_renderer.cpp`,
//! paired with a pure-Rust TeX math box/glyph layout engine (MicroTeX-equivalent).
//!
//! Produces positioned glyph runs ([`PositionedGlyphRun`]), horizontal/vertical rules
//! ([`FormulaRule`]), and vector polylines ([`FormulaPolyline`]) in logical pixels so any
//! 2D renderer (such as `egui::Painter`) can draw crisp mathematical typography with no C++
//! MicroTeX dependency.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::markdown::document::MathKind;

/// Fixed-point subpixel scale factor matching `kFormulaExactMetricScale` (`iv_markdown_document.h`).
pub const FORMULA_EXACT_METRIC_SCALE: i32 = 64;

/// Maximum accepted TeX source length in chars matching `kMaxFormulaTexLength` (`iv_markdown_microtex.cpp`).
pub const MAX_FORMULA_TEX_LENGTH: usize = 32 * 1024;

/// Default LRU formula cache budget matching `FormulaCache::_budgetBytes` (`32 MiB`).
pub const DEFAULT_FORMULA_CACHE_BUDGET_BYTES: usize = 32 * 1024 * 1024;

// Knuth TeXbook Appendix G / Computer Modern Math typographic ratios (in em):
const MATH_AXIS_RATIO: f32 = 0.25;
const RULE_THICKNESS_RATIO: f32 = 0.045;
const SCRIPT_SCALE: f32 = 0.70;
const SCRIPTSCRIPT_SCALE: f32 = 0.50;
const MIN_FONT_SIZE: f32 = 6.0;
const ASCENT_RATIO: f32 = 0.75;
const DEPTH_RATIO: f32 = 0.22;
const FRAC_GAP_DISPLAY: f32 = 0.14;
const FRAC_GAP_TEXT: f32 = 0.09;
const FRAC_PAD_X: f32 = 0.12;
const SUP_SHIFT_DISPLAY: f32 = 0.42;
const SUP_SHIFT_TEXT: f32 = 0.36;
const SUB_SHIFT_BASE: f32 = 0.24;
const BIG_OP_DISPLAY_SCALE: f32 = 1.45;
const BIG_OP_TEXT_SCALE: f32 = 1.15;
const MATRIX_COL_GAP: f32 = 0.80;
const MATRIX_ROW_GAP: f32 = 0.30;
const THIN_SPACE: f32 = 0.167;
const MED_SPACE: f32 = 0.222;
const THICK_SPACE: f32 = 0.278;

/// Font style hint for a positioned math glyph run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MathFontStyle {
    /// Upright Roman math text (digits, operator names, `\mathrm`, `\text`).
    #[default]
    Roman,
    /// Italic math variables (`x`, `y`, lowercase Greek).
    Italic,
    /// Bold upright (`\mathbf`).
    Bold,
    /// Bold italic.
    BoldItalic,
    /// Sans-serif (`\mathsf`).
    SansSerif,
    /// Monospace (`\mathtt`).
    Monospace,
    /// Calligraphic / script (`\mathcal`).
    Script,
    /// Blackboard bold (`\mathbb`).
    Blackboard,
    /// Fraktur (`\mathfrak`).
    Fraktur,
    /// Mathematical symbols & large operators (`∑`, `∫`, `∏`, delimiters).
    Symbol,
}

/// A positioned text run emitted by the math layout engine.
///
/// Coordinates are in logical pixels relative to the formula's top-left corner `(0, 0)`:
/// `y` is the typographic **baseline** of the run (`y - ascent` is the top edge, `y + depth`
/// is the bottom edge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionedGlyphRun {
    /// Left X coordinate in logical pixels.
    pub x: f32,
    /// Baseline Y coordinate in logical pixels.
    pub y: f32,
    /// Advance width in logical pixels.
    pub width: f32,
    /// Ascent above baseline (`>= 0`).
    pub ascent: f32,
    /// Descent/depth below baseline (`>= 0`).
    pub depth: f32,
    /// UTF-8 glyph / symbol text to draw.
    pub text: String,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Font style variant.
    pub style: MathFontStyle,
}

/// A filled axis-aligned rectangle rule (fraction bars, radical vincula, overlines, underlines).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FormulaRule {
    /// Left X coordinate in logical pixels.
    pub x: f32,
    /// Top Y coordinate in logical pixels.
    pub y: f32,
    /// Width in logical pixels.
    pub width: f32,
    /// Thickness / height in logical pixels.
    pub height: f32,
}

/// A stroked polyline path (radical checkmark `√`, tall angle brackets, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaPolyline {
    /// Vertices `(x, y)` in logical pixels relative to formula origin `(0, 0)`.
    pub points: Vec<(f32, f32)>,
    /// Stroke thickness in logical pixels.
    pub stroke_width: f32,
}

/// Complete pure-Rust box/glyph layout ready to be painted by `egui::Painter` or any 2D canvas.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FormulaLayout {
    /// Total logical width in pixels.
    pub width: f32,
    /// Total logical height in pixels (`ascent + depth`).
    pub height: f32,
    /// Distance from top edge (`y = 0`) to the main math baseline.
    pub ascent: f32,
    /// Distance from the main math baseline to the bottom edge.
    pub depth: f32,
    /// Positioned text / symbol runs.
    pub glyph_runs: Vec<PositionedGlyphRun>,
    /// Filled horizontal/vertical rules (fraction bars, vincula, etc.).
    pub rules: Vec<FormulaRule>,
    /// Stroked polylines (radical checkmarks, stretched angle delimiters, etc.).
    pub polylines: Vec<FormulaPolyline>,
}

/// Fixed-point exact formula metrics scaled by [`FORMULA_EXACT_METRIC_SCALE`] (`64`),
/// matching `Iv::Markdown::FormulaExactMetrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormulaExactMetrics {
    /// Scaled width (`width * 64`).
    pub scaled_width: i32,
    /// Scaled height (`height * 64`).
    pub scaled_height: i32,
    /// Scaled ascent (`ascent * 64`).
    pub scaled_ascent: i32,
}

/// Measurement result matching `Iv::Markdown::MeasuredFormula`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MeasuredFormula {
    /// Rounded logical width in pixels.
    pub logical_width: i32,
    /// Rounded logical height in pixels.
    pub logical_height: i32,
    /// Rounded logical depth below baseline in pixels.
    pub logical_depth: i32,
    /// Exact `1/64 px` metrics.
    pub exact: FormulaExactMetrics,
    /// Plain-text fallback when rendering fails.
    pub fallback_text: String,
    /// Error code when `success == false`.
    pub error: String,
    /// True if layout succeeded within caps.
    pub success: bool,
    /// True if dimensions exceeded render caps.
    pub overflow: bool,
    /// True if rejected for exceeding size caps.
    pub too_large: bool,
}

/// Full rendered layout result matching `Iv::Markdown::RenderedFormula`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderedFormula {
    /// Positioned glyph runs, rules, and polylines (`Some` when `measured.success`).
    pub layout: Option<FormulaLayout>,
    /// Measurement & error metadata.
    pub measured: MeasuredFormula,
}

/// Layout request matching `MicrotexMeasureRequest` / `MicrotexRenderRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaLayoutRequest {
    /// TeX source (outer whitespace is trimmed automatically).
    pub trimmed_tex: String,
    /// Display vs inline math style.
    pub kind: MathKind,
    /// Base font size in logical pixels (`> 0`).
    pub text_size: i32,
    /// Maximum allowed logical width in pixels (`> 0`).
    pub render_width_cap: i32,
    /// Maximum allowed logical height in pixels (`> 0`).
    pub render_height_cap: i32,
}

impl FormulaLayoutRequest {
    /// Convenience constructor with generous default caps (`2048 x 1024`).
    #[must_use]
    pub fn new(tex: impl Into<String>, kind: MathKind, text_size: i32) -> Self {
        Self {
            trimmed_tex: tex.into(),
            kind,
            text_size,
            render_width_cap: 2048,
            render_height_cap: 1024,
        }
    }
}

/// Rewrites full-width outer alignment environments into natural-width inner equivalents,
/// matching `RewriteFullWidthEnvironments` (`iv_markdown_microtex.cpp:111-139`).
#[must_use]
pub fn rewrite_full_width_environments(tex: &str) -> String {
    if !tex.contains("\\begin{") {
        return tex.to_string();
    }
    const REWRITES: [(&str, &str); 12] = [
        ("align*", "aligned"),
        ("align", "aligned"),
        ("flalign*", "aligned"),
        ("flalign", "aligned"),
        ("alignat*", "alignedat"),
        ("alignat", "alignedat"),
        ("gather*", "gathered"),
        ("gather", "gathered"),
        ("multline*", "gathered"),
        ("multline", "gathered"),
        ("equation*", "gathered"),
        ("equation", "gathered"),
    ];
    let mut out = tex.to_string();
    for (from, to) in REWRITES {
        out = out.replace(&format!("\\begin{{{from}}}"), &format!("\\begin{{{to}}}"));
        out = out.replace(&format!("\\end{{{from}}}"), &format!("\\end{{{to}}}"));
    }
    out
}

fn rounded_logical_metric(scaled: i32) -> i32 {
    let clamped = scaled.max(0) as i64;
    let rounded = (clamped + i64::from(FORMULA_EXACT_METRIC_SCALE) - 1)
        / i64::from(FORMULA_EXACT_METRIC_SCALE);
    rounded.min(i64::from(i32::MAX)) as i32
}

fn layout_to_measured(layout: &FormulaLayout, request: &FormulaLayoutRequest) -> MeasuredFormula {
    let scale = FORMULA_EXACT_METRIC_SCALE as f32;
    let scaled_width = (layout.width.max(0.0) * scale).round() as i32;
    let scaled_height = (layout.height.max(0.0) * scale).round() as i32;
    let scaled_ascent = ((layout.ascent.max(0.0) * scale).round() as i32).clamp(0, scaled_height);
    let logical_width = rounded_logical_metric(scaled_width);
    let logical_height = rounded_logical_metric(scaled_height);
    let scaled_depth = (scaled_height - scaled_ascent).max(0);
    let logical_depth = rounded_logical_metric(scaled_depth).clamp(0, logical_height);

    let mut measured = MeasuredFormula {
        logical_width,
        logical_height,
        logical_depth,
        exact: FormulaExactMetrics {
            scaled_width,
            scaled_height,
            scaled_ascent,
        },
        fallback_text: request.trimmed_tex.trim().to_string(),
        error: String::new(),
        success: true,
        overflow: false,
        too_large: false,
    };

    if logical_width <= 0 || logical_height <= 0 {
        measured.success = false;
        measured.error = "invalid-render-size".to_string();
        return measured;
    }
    if logical_width > request.render_width_cap || logical_height > request.render_height_cap {
        measured.success = false;
        measured.error = "logical-size-cap-exceeded".to_string();
        measured.overflow = true;
        measured.too_large = true;
    }
    measured
}

/// Validates and lays out a TeX formula, returning its [`RenderedFormula`].
#[must_use]
pub fn layout_formula(request: &FormulaLayoutRequest) -> RenderedFormula {
    let fail = |code: &str, too_large: bool| RenderedFormula {
        layout: None,
        measured: MeasuredFormula {
            fallback_text: request.trimmed_tex.trim().to_string(),
            error: code.to_string(),
            success: false,
            overflow: too_large,
            too_large,
            ..MeasuredFormula::default()
        },
    };
    if request.text_size <= 0 {
        return fail("invalid-text-size", false);
    }
    if request.render_width_cap <= 0 {
        return fail("invalid-render-width", false);
    }
    if request.render_height_cap <= 0 {
        return fail("invalid-render-height", false);
    }
    let trimmed = request.trimmed_tex.trim();
    if trimmed.is_empty() {
        return fail("empty-tex", false);
    }
    if trimmed.len() > MAX_FORMULA_TEX_LENGTH {
        return fail("tex-too-long", false);
    }

    let rewritten = rewrite_full_width_environments(trimmed);
    let tokens = tokenize_tex(&rewritten);
    let mut parser = TexParser::new(&tokens);
    let ast = parser.parse_sequence(None);
    if ast.is_empty() {
        return fail("empty-render", false);
    }

    let style = match request.kind {
        MathKind::Display => TeXStyle::Display,
        MathKind::Inline => TeXStyle::Text,
    };
    let layout = build_formula_layout(&ast, style, request.text_size as f32);
    let measured = layout_to_measured(&layout, request);
    if measured.success {
        RenderedFormula {
            layout: Some(layout),
            measured,
        }
    } else {
        RenderedFormula {
            layout: None,
            measured,
        }
    }
}

/// Measures a TeX formula without retaining the layout vectors (`MeasureWithMicrotex`).
#[must_use]
pub fn measure_formula(request: &FormulaLayoutRequest) -> MeasuredFormula {
    layout_formula(request).measured
}

// ============================================================================
// LRU Formula Cache & Engine (`iv_markdown_math_renderer.cpp`)
// ============================================================================

/// Lookup key for [`FormulaCache`] (`Iv::Markdown::FormulaCacheKey`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormulaCacheKey {
    /// Trimmed TeX source.
    pub trimmed_tex: String,
    /// Inline vs display math.
    pub kind: MathKind,
    /// Base font size in logical pixels.
    pub text_size: i32,
    /// Width cap in logical pixels.
    pub render_width_cap: i32,
    /// Height cap in logical pixels.
    pub render_height_cap: i32,
}

/// Debug counters tracking cache hits/misses/evictions (`Iv::Markdown::FormulaDebugCounters`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormulaDebugCounters {
    /// Successful layouts computed.
    pub rendered: usize,
    /// Failed layout attempts.
    pub failed: usize,
    /// Cache hits.
    pub hits: usize,
    /// Cache misses.
    pub misses: usize,
    /// Evicted entry count.
    pub evicted_entries: usize,
    /// Evicted bytes estimate.
    pub evicted_bytes: usize,
    /// Current entry count in cache.
    pub cache_entries: usize,
    /// Current estimated cache memory in bytes.
    pub cache_bytes: usize,
}

/// Eviction summary returned by [`FormulaCache::put`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormulaCacheMutation {
    /// Number of entries evicted.
    pub evicted_entries: usize,
    /// Bytes reclaimed.
    pub evicted_bytes: usize,
}

#[derive(Debug)]
struct CacheEntry {
    value: RenderedFormula,
    size_bytes: usize,
}

/// Bounded LRU cache of laid-out formulas (`Iv::Markdown::FormulaCache`).
#[derive(Debug)]
pub struct FormulaCache {
    entries: BTreeMap<FormulaCacheKey, CacheEntry>,
    lru: VecDeque<FormulaCacheKey>,
    budget_bytes: usize,
    size_bytes: usize,
}

impl Default for FormulaCache {
    fn default() -> Self {
        Self::with_budget(DEFAULT_FORMULA_CACHE_BUDGET_BYTES)
    }
}

impl FormulaCache {
    /// Creates a cache with a custom byte budget.
    #[must_use]
    pub fn with_budget(budget_bytes: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lru: VecDeque::new(),
            budget_bytes,
            size_bytes: 0,
        }
    }

    /// Looks up a cached formula and refreshes its LRU recency.
    pub fn find(&mut self, key: &FormulaCacheKey) -> Option<&RenderedFormula> {
        if self.entries.contains_key(key) {
            if let Some(pos) = self.lru.iter().position(|k| k == key) {
                self.lru.remove(pos);
            }
            self.lru.push_front(key.clone());
            self.entries.get(key).map(|e| &e.value)
        } else {
            None
        }
    }

    /// Inserts or replaces an entry and evicts oldest entries until within budget.
    pub fn put(&mut self, key: FormulaCacheKey, value: RenderedFormula) -> FormulaCacheMutation {
        let est = estimate_formula_bytes(&key, &value);
        if let Some(old) = self.entries.remove(&key) {
            self.size_bytes = self.size_bytes.saturating_sub(old.size_bytes);
            if let Some(pos) = self.lru.iter().position(|k| k == &key) {
                self.lru.remove(pos);
            }
        }
        self.size_bytes = self.size_bytes.saturating_add(est);
        self.entries.insert(
            key.clone(),
            CacheEntry {
                value,
                size_bytes: est,
            },
        );
        self.lru.push_front(key);
        self.evict_to_budget()
    }

    /// Current estimated memory footprint in bytes.
    #[must_use]
    pub const fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    /// Current number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if no entries are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.size_bytes = 0;
    }

    fn evict_to_budget(&mut self) -> FormulaCacheMutation {
        let mut mutation = FormulaCacheMutation::default();
        while self.size_bytes > self.budget_bytes {
            let Some(oldest) = self.lru.pop_back() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.size_bytes = self.size_bytes.saturating_sub(removed.size_bytes);
                mutation.evicted_entries += 1;
                mutation.evicted_bytes += removed.size_bytes;
            }
        }
        mutation
    }
}

fn estimate_formula_bytes(key: &FormulaCacheKey, value: &RenderedFormula) -> usize {
    let mut total = key.trimmed_tex.len() + value.measured.fallback_text.len() + 128;
    if let Some(layout) = &value.layout {
        total += layout.glyph_runs.len() * 64;
        for run in &layout.glyph_runs {
            total += run.text.len();
        }
        total += layout.rules.len() * 24;
        for poly in &layout.polylines {
            total += 24 + poly.points.len() * 8;
        }
    }
    total
}

/// Caching formula layout engine (`Iv::Markdown::MathRenderer`).
#[derive(Debug, Default)]
pub struct MathLayoutEngine {
    cache: FormulaCache,
    counters: FormulaDebugCounters,
}

impl MathLayoutEngine {
    /// Creates an engine with a custom cache budget in bytes.
    #[must_use]
    pub fn with_cache_budget(budget_bytes: usize) -> Self {
        Self {
            cache: FormulaCache::with_budget(budget_bytes),
            counters: FormulaDebugCounters::default(),
        }
    }

    /// Measures a formula (using the cache when already laid out).
    pub fn measure(&mut self, request: &FormulaLayoutRequest) -> MeasuredFormula {
        self.render(request).measured
    }

    /// Lays out a formula with LRU caching.
    pub fn render(&mut self, request: &FormulaLayoutRequest) -> RenderedFormula {
        let key = FormulaCacheKey {
            trimmed_tex: request.trimmed_tex.trim().to_string(),
            kind: request.kind,
            text_size: request.text_size,
            render_width_cap: request.render_width_cap,
            render_height_cap: request.render_height_cap,
        };
        if let Some(cached) = self.cache.find(&key) {
            self.counters.hits += 1;
            return cached.clone();
        }
        self.counters.misses += 1;
        let rendered = layout_formula(request);
        if rendered.measured.success {
            self.counters.rendered += 1;
        } else {
            self.counters.failed += 1;
        }
        let mutation = self.cache.put(key, rendered.clone());
        self.counters.evicted_entries += mutation.evicted_entries;
        self.counters.evicted_bytes += mutation.evicted_bytes;
        self.counters.cache_entries = self.cache.len();
        self.counters.cache_bytes = self.cache.size_bytes();
        rendered
    }

    /// Clears the cache and optionally resets debug counters.
    pub fn clear_cache(&mut self, reset_counters: bool) {
        self.cache.clear();
        if reset_counters {
            self.counters = FormulaDebugCounters::default();
        } else {
            self.counters.cache_entries = 0;
            self.counters.cache_bytes = 0;
        }
    }

    /// Returns current debug counters.
    #[must_use]
    pub const fn debug_counters(&self) -> FormulaDebugCounters {
        self.counters
    }
}

// ============================================================================
// Pure-Rust TeX Math Lexer & Parser
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Command(String),
    Char(char),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Caret,
    Underscore,
    Amp,
    NewRow,
    Space,
}

fn tokenize_tex(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '%' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '{' => {
                tokens.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                tokens.push(Token::RBrace);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                i += 1;
            }
            '_' => {
                tokens.push(Token::Underscore);
                i += 1;
            }
            '&' => {
                tokens.push(Token::Amp);
                i += 1;
            }
            ' ' | '\t' | '\n' | '\r' => {
                if !matches!(tokens.last(), Some(Token::Space)) {
                    tokens.push(Token::Space);
                }
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                let next = chars[i];
                if next == '\\' {
                    tokens.push(Token::NewRow);
                    i += 1;
                } else if next.is_ascii_alphabetic() {
                    let start = i;
                    while i < chars.len() && chars[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let cmd: String = chars[start..i].iter().collect();
                    tokens.push(Token::Command(cmd));
                } else {
                    tokens.push(Token::Command(next.to_string()));
                    i += 1;
                }
            }
            other => {
                tokens.push(Token::Char(other));
                i += 1;
            }
        }
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomClass {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitsMode {
    Default,
    Limits,
    NoLimits,
}

#[derive(Debug, Clone)]
enum MathAtom {
    Symbol {
        text: String,
        style: MathFontStyle,
        class: AtomClass,
    },
    BigOp {
        symbol: String,
        is_integral: bool,
        limits_mode: LimitsMode,
    },
    Space(f32),
    Group(Vec<MathAtom>),
    Styled {
        style: MathFontStyle,
        body: Vec<MathAtom>,
    },
    StyleSwitch(TeXStyle),
    Frac {
        num: Vec<MathAtom>,
        den: Vec<MathAtom>,
        rule: bool,
        forced_style: Option<TeXStyle>,
    },
    Sqrt {
        index: Option<Vec<MathAtom>>,
        radicand: Vec<MathAtom>,
    },
    Scripts {
        base: Box<MathAtom>,
        sub: Option<Vec<MathAtom>>,
        sup: Option<Vec<MathAtom>>,
    },
    Accent {
        accent: String,
        body: Vec<MathAtom>,
        is_wide: bool,
    },
    Overline(Vec<MathAtom>),
    Underline(Vec<MathAtom>),
    Delimited {
        left: String,
        right: String,
        body: Vec<MathAtom>,
    },
    Matrix {
        left: String,
        right: String,
        rows: Vec<Vec<Vec<MathAtom>>>,
        small: bool,
        left_aligned: bool,
    },
}

const MAX_TEX_NESTING_DEPTH: usize = 64;

struct TexParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
}

impl<'a> TexParser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(Token::Space)) {
            self.pos += 1;
        }
    }

    fn parse_sequence(&mut self, stop_env: Option<&str>) -> Vec<MathAtom> {
        if self.depth >= MAX_TEX_NESTING_DEPTH {
            // Advance past remaining tokens in this scope to prevent infinite loops.
            while let Some(tok) = self.peek() {
                match tok {
                    Token::RBrace | Token::Amp | Token::NewRow => break,
                    Token::Command(c) if c == "right" => break,
                    Token::Command(c) if c == "end" && stop_env.is_some() => break,
                    _ => self.pos += 1,
                }
            }
            return Vec::new();
        }
        self.depth += 1;
        let mut items = Vec::new();
        while let Some(tok) = self.peek().cloned() {
            match tok {
                Token::RBrace | Token::Amp | Token::NewRow => break,
                Token::Command(ref c) if c == "right" => break,
                Token::Command(ref c) if c == "end" && stop_env.is_some() => break,
                Token::Command(ref c) if c == "over" || c == "choose" => {
                    let is_over = c == "over";
                    self.pos += 1;
                    let num = std::mem::take(&mut items);
                    let den = self.parse_sequence(stop_env);
                    let frac = MathAtom::Frac {
                        num,
                        den,
                        rule: is_over,
                        forced_style: None,
                    };
                    if is_over {
                        items.push(frac);
                    } else {
                        items.push(MathAtom::Delimited {
                            left: "(".to_string(),
                            right: ")".to_string(),
                            body: vec![frac],
                        });
                    }
                    break;
                }
                Token::Space => {
                    self.pos += 1;
                }
                _ => {
                    if let Some(atom) = self.parse_atom_with_scripts() {
                        items.push(atom);
                    }
                }
            }
        }
        self.depth -= 1;
        items
    }

    fn parse_atom_with_scripts(&mut self) -> Option<MathAtom> {
        let mut base = self.parse_primary()?;
        let mut sub: Option<Vec<MathAtom>> = None;
        let mut sup: Option<Vec<MathAtom>> = None;
        let mut limits_override: Option<LimitsMode> = None;

        loop {
            self.skip_spaces();
            match self.peek() {
                Some(Token::Command(c)) if c == "limits" => {
                    self.pos += 1;
                    limits_override = Some(LimitsMode::Limits);
                }
                Some(Token::Command(c)) if c == "nolimits" => {
                    self.pos += 1;
                    limits_override = Some(LimitsMode::NoLimits);
                }
                Some(Token::Underscore) => {
                    self.pos += 1;
                    sub = Some(self.parse_arg());
                }
                Some(Token::Caret) => {
                    self.pos += 1;
                    sup = Some(self.parse_arg());
                }
                Some(Token::Char('\'')) => {
                    let mut primes = String::new();
                    while matches!(self.peek(), Some(Token::Char('\''))) {
                        primes.push('′');
                        self.pos += 1;
                    }
                    let prime_atom = MathAtom::Symbol {
                        text: primes,
                        style: MathFontStyle::Symbol,
                        class: AtomClass::Ord,
                    };
                    match &mut sup {
                        Some(existing) => existing.push(prime_atom),
                        None => sup = Some(vec![prime_atom]),
                    }
                }
                _ => break,
            }
        }

        if let Some(mode) = limits_override
            && let MathAtom::BigOp {
                ref mut limits_mode,
                ..
            } = base
        {
            *limits_mode = mode;
        }

        if sub.is_some() || sup.is_some() {
            Some(MathAtom::Scripts {
                base: Box::new(base),
                sub,
                sup,
            })
        } else {
            Some(base)
        }
    }

    fn parse_arg(&mut self) -> Vec<MathAtom> {
        self.skip_spaces();
        match self.peek() {
            Some(Token::LBrace) => {
                self.pos += 1;
                let seq = self.parse_sequence(None);
                if matches!(self.peek(), Some(Token::RBrace)) {
                    self.pos += 1;
                }
                seq
            }
            _ => self.parse_primary().into_iter().collect(),
        }
    }

    fn parse_raw_group_text(&mut self) -> String {
        self.skip_spaces();
        if !matches!(self.peek(), Some(Token::LBrace)) {
            return match self.next() {
                Some(Token::Char(c)) => c.to_string(),
                Some(Token::Command(s)) => s.clone(),
                _ => String::new(),
            };
        }
        self.pos += 1;
        let mut out = String::new();
        let mut depth = 1usize;
        while let Some(tok) = self.next() {
            match tok {
                Token::LBrace => {
                    depth += 1;
                    out.push('{');
                }
                Token::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push('}');
                }
                Token::Char(c) => out.push(*c),
                Token::LBracket => out.push('['),
                Token::RBracket => out.push(']'),
                Token::Space => out.push(' '),
                Token::Amp => out.push('&'),
                Token::Caret => out.push('^'),
                Token::Underscore => out.push('_'),
                Token::NewRow => out.push_str("\\\\"),
                Token::Command(c) => {
                    if let Some((sym, _, _)) = lookup_tex_symbol(c) {
                        out.push_str(sym);
                    } else {
                        out.push_str(c);
                    }
                }
            }
        }
        out
    }

    fn parse_delimiter(&mut self) -> String {
        self.skip_spaces();
        match self.next().cloned() {
            Some(Token::Char('.')) => String::new(),
            Some(Token::Char(c)) => c.to_string(),
            Some(Token::LBracket) => "[".to_string(),
            Some(Token::RBracket) => "]".to_string(),
            Some(Token::Command(c)) => match c.as_str() {
                "{" | "lbrace" => "{".to_string(),
                "}" | "rbrace" => "}".to_string(),
                "|" | "Vert" | "lVert" | "rVert" => "‖".to_string(),
                "vert" | "lvert" | "rvert" => "|".to_string(),
                "langle" => "⟨".to_string(),
                "rangle" => "⟩".to_string(),
                "lfloor" => "⌊".to_string(),
                "rfloor" => "⌋".to_string(),
                "lceil" => "⌈".to_string(),
                "rceil" => "⌉".to_string(),
                "backslash" => "\\".to_string(),
                other => other.to_string(),
            },
            _ => String::new(),
        }
    }

    fn parse_primary(&mut self) -> Option<MathAtom> {
        self.skip_spaces();
        if self.depth >= MAX_TEX_NESTING_DEPTH {
            let _ = self.next();
            return None;
        }
        let tok = self.next()?.clone();
        match tok {
            Token::LBrace => {
                let seq = self.parse_sequence(None);
                if matches!(self.peek(), Some(Token::RBrace)) {
                    self.pos += 1;
                }
                Some(MathAtom::Group(seq))
            }
            Token::RBrace | Token::Amp | Token::NewRow | Token::Space => None,
            Token::LBracket => Some(MathAtom::Symbol {
                text: "[".to_string(),
                style: MathFontStyle::Roman,
                class: AtomClass::Open,
            }),
            Token::RBracket => Some(MathAtom::Symbol {
                text: "]".to_string(),
                style: MathFontStyle::Roman,
                class: AtomClass::Close,
            }),
            Token::Caret | Token::Underscore => None,
            Token::Char(ch) => Some(char_to_atom(ch)),
            Token::Command(cmd) => {
                self.depth += 1;
                let res = self.parse_command(&cmd);
                self.depth -= 1;
                res
            }
        }
    }

    fn parse_command(&mut self, cmd: &str) -> Option<MathAtom> {
        match cmd {
            "displaystyle" => return Some(MathAtom::StyleSwitch(TeXStyle::Display)),
            "textstyle" => return Some(MathAtom::StyleSwitch(TeXStyle::Text)),
            "scriptstyle" => return Some(MathAtom::StyleSwitch(TeXStyle::Script)),
            "scriptscriptstyle" => return Some(MathAtom::StyleSwitch(TeXStyle::ScriptScript)),
            "frac" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                return Some(MathAtom::Frac {
                    num,
                    den,
                    rule: true,
                    forced_style: None,
                });
            }
            "dfrac" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                return Some(MathAtom::Frac {
                    num,
                    den,
                    rule: true,
                    forced_style: Some(TeXStyle::Display),
                });
            }
            "tfrac" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                return Some(MathAtom::Frac {
                    num,
                    den,
                    rule: true,
                    forced_style: Some(TeXStyle::Text),
                });
            }
            "binom" | "dbinom" | "tbinom" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                let forced = if cmd == "dbinom" {
                    Some(TeXStyle::Display)
                } else if cmd == "tbinom" {
                    Some(TeXStyle::Text)
                } else {
                    None
                };
                return Some(MathAtom::Delimited {
                    left: "(".to_string(),
                    right: ")".to_string(),
                    body: vec![MathAtom::Frac {
                        num,
                        den,
                        rule: false,
                        forced_style: forced,
                    }],
                });
            }
            "sqrt" => {
                self.skip_spaces();
                let index = if matches!(self.peek(), Some(Token::LBracket)) {
                    self.pos += 1;
                    let mut idx_tokens = Vec::new();
                    let mut depth = 0usize;
                    while let Some(t) = self.next().cloned() {
                        match t {
                            Token::LBrace => {
                                depth += 1;
                                idx_tokens.push(t);
                            }
                            Token::RBrace => {
                                depth = depth.saturating_sub(1);
                                idx_tokens.push(t);
                            }
                            Token::RBracket if depth == 0 => break,
                            other => idx_tokens.push(other),
                        }
                    }
                    let mut sub_parser = TexParser::new(&idx_tokens);
                    Some(sub_parser.parse_sequence(None))
                } else {
                    None
                };
                let radicand = self.parse_arg();
                return Some(MathAtom::Sqrt { index, radicand });
            }
            "left" => {
                let left = self.parse_delimiter();
                let body = self.parse_sequence(None);
                let right = if matches!(self.peek(), Some(Token::Command(c)) if c == "right") {
                    self.pos += 1;
                    self.parse_delimiter()
                } else {
                    String::new()
                };
                return Some(MathAtom::Delimited { left, right, body });
            }
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl"
            | "biggr" | "Biggl" | "Biggr" => {
                let d = self.parse_delimiter();
                return Some(MathAtom::Symbol {
                    text: d,
                    style: MathFontStyle::Symbol,
                    class: AtomClass::Ord,
                });
            }
            "begin" => {
                let env = self.parse_raw_group_text();
                return Some(self.parse_environment(&env));
            }
            "text" | "textrm" | "mbox" => {
                let txt = self.parse_raw_group_text();
                return Some(MathAtom::Symbol {
                    text: txt,
                    style: MathFontStyle::Roman,
                    class: AtomClass::Ord,
                });
            }
            "textbf" => {
                let txt = self.parse_raw_group_text();
                return Some(MathAtom::Symbol {
                    text: txt,
                    style: MathFontStyle::Bold,
                    class: AtomClass::Ord,
                });
            }
            "textit" => {
                let txt = self.parse_raw_group_text();
                return Some(MathAtom::Symbol {
                    text: txt,
                    style: MathFontStyle::Italic,
                    class: AtomClass::Ord,
                });
            }
            "operatorname" => {
                let txt = self.parse_raw_group_text();
                return Some(MathAtom::Symbol {
                    text: txt,
                    style: MathFontStyle::Roman,
                    class: AtomClass::Op,
                });
            }
            "mathrm" | "rm" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Roman,
                    body: self.parse_arg(),
                });
            }
            "mathbf" | "bf" | "boldsymbol" | "bm" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Bold,
                    body: self.parse_arg(),
                });
            }
            "mathit" | "it" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Italic,
                    body: self.parse_arg(),
                });
            }
            "mathsf" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::SansSerif,
                    body: self.parse_arg(),
                });
            }
            "mathtt" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Monospace,
                    body: self.parse_arg(),
                });
            }
            "mathcal" | "mathscr" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Script,
                    body: self.parse_arg(),
                });
            }
            "mathbb" => {
                let raw = self.parse_raw_group_text();
                let mapped: String = raw.chars().map(blackboard_char).collect();
                return Some(MathAtom::Symbol {
                    text: mapped,
                    style: MathFontStyle::Blackboard,
                    class: AtomClass::Ord,
                });
            }
            "mathfrak" => {
                return Some(MathAtom::Styled {
                    style: MathFontStyle::Fraktur,
                    body: self.parse_arg(),
                });
            }
            "overline" | "bar" => {
                return Some(MathAtom::Overline(self.parse_arg()));
            }
            "underline" => {
                return Some(MathAtom::Underline(self.parse_arg()));
            }
            "hat" => {
                return Some(MathAtom::Accent {
                    accent: "^".to_string(),
                    body: self.parse_arg(),
                    is_wide: false,
                });
            }
            "widehat" => {
                return Some(MathAtom::Accent {
                    accent: "^".to_string(),
                    body: self.parse_arg(),
                    is_wide: true,
                });
            }
            "tilde" | "widetilde" => {
                return Some(MathAtom::Accent {
                    accent: "~".to_string(),
                    body: self.parse_arg(),
                    is_wide: cmd == "widetilde",
                });
            }
            "vec" => {
                return Some(MathAtom::Accent {
                    accent: "→".to_string(),
                    body: self.parse_arg(),
                    is_wide: false,
                });
            }
            "dot" => {
                return Some(MathAtom::Accent {
                    accent: "˙".to_string(),
                    body: self.parse_arg(),
                    is_wide: false,
                });
            }
            "ddot" => {
                return Some(MathAtom::Accent {
                    accent: "¨".to_string(),
                    body: self.parse_arg(),
                    is_wide: false,
                });
            }
            "not" => {
                return Some(MathAtom::Symbol {
                    text: "̸".to_string(),
                    style: MathFontStyle::Symbol,
                    class: AtomClass::Rel,
                });
            }
            // Spacing commands
            "," | "thinspace" => return Some(MathAtom::Space(THIN_SPACE)),
            ":" | ">" | "medspace" => return Some(MathAtom::Space(MED_SPACE)),
            ";" | "thickspace" => return Some(MathAtom::Space(THICK_SPACE)),
            "!" => return Some(MathAtom::Space(-THIN_SPACE)),
            " " => return Some(MathAtom::Space(0.33)),
            "quad" => return Some(MathAtom::Space(1.0)),
            "qquad" => return Some(MathAtom::Space(2.0)),
            "phantom" => {
                let _ = self.parse_arg();
                return Some(MathAtom::Space(0.5));
            }
            // Big operators
            "sum" => return Some(big_op("∑", false)),
            "prod" => return Some(big_op("∏", false)),
            "coprod" => return Some(big_op("∐", false)),
            "int" => return Some(big_op("∫", true)),
            "iint" => return Some(big_op("∬", true)),
            "iiint" => return Some(big_op("∭", true)),
            "oint" => return Some(big_op("∮", true)),
            "bigcup" => return Some(big_op("⋃", false)),
            "bigcap" => return Some(big_op("⋂", false)),
            "bigoplus" => return Some(big_op("⨁", false)),
            "bigotimes" => return Some(big_op("⨂", false)),
            "bigvee" => return Some(big_op("⋁", false)),
            "bigwedge" => return Some(big_op("⋀", false)),
            "bigsqcup" => return Some(big_op("⨆", false)),
            "lim" | "limsup" | "liminf" | "sup" | "inf" | "max" | "min" | "det" | "gcd" => {
                let label = match cmd {
                    "limsup" => "lim sup",
                    "liminf" => "lim inf",
                    other => other,
                };
                return Some(MathAtom::BigOp {
                    symbol: label.to_string(),
                    is_integral: false,
                    limits_mode: LimitsMode::Default,
                });
            }
            // Standard Roman operator names (no above/below limits)
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan"
            | "sinh" | "cosh" | "tanh" | "coth" | "log" | "ln" | "lg" | "exp" | "arg" | "deg"
            | "dim" | "hom" | "ker" | "Pr" | "mod" | "bmod" | "pmod" => {
                return Some(MathAtom::Symbol {
                    text: cmd.trim_start_matches(['b', 'p']).to_string(),
                    style: MathFontStyle::Roman,
                    class: AtomClass::Op,
                });
            }
            _ => {}
        }

        if let Some((sym, style, class)) = lookup_tex_symbol(cmd) {
            return Some(MathAtom::Symbol {
                text: sym.to_string(),
                style,
                class,
            });
        }

        // Escaped single punctuation or unknown command fallback
        if cmd.len() == 1 {
            return Some(MathAtom::Symbol {
                text: cmd.to_string(),
                style: MathFontStyle::Roman,
                class: AtomClass::Ord,
            });
        }
        Some(MathAtom::Symbol {
            text: format!("\\{cmd}"),
            style: MathFontStyle::Roman,
            class: AtomClass::Ord,
        })
    }

    fn parse_environment(&mut self, env: &str) -> MathAtom {
        // Consume optional column spec for `array` / `alignedat`
        if env == "array" || env == "alignedat" {
            let _ = self.parse_raw_group_text();
        }
        let mut rows: Vec<Vec<Vec<MathAtom>>> = Vec::new();
        let mut cur_row: Vec<Vec<MathAtom>> = Vec::new();
        loop {
            let cell = self.parse_sequence(Some(env));
            cur_row.push(cell);
            match self.peek().cloned() {
                Some(Token::Amp) => {
                    self.pos += 1;
                }
                Some(Token::NewRow) => {
                    self.pos += 1;
                    // Optional `[spacing]` after `\\`
                    if matches!(self.peek(), Some(Token::LBracket)) {
                        while let Some(t) = self.next() {
                            if matches!(t, Token::RBracket) {
                                break;
                            }
                        }
                    }
                    rows.push(std::mem::take(&mut cur_row));
                }
                Some(Token::Command(c)) if c == "end" => {
                    self.pos += 1;
                    let _ = self.parse_raw_group_text();
                    break;
                }
                _ => break,
            }
        }
        if !cur_row.is_empty() && !(cur_row.len() == 1 && cur_row[0].is_empty() && !rows.is_empty())
        {
            rows.push(cur_row);
        }

        let (left, right, small, left_aligned) = match env {
            "pmatrix" => ("(", ")", false, false),
            "bmatrix" => ("[", "]", false, false),
            "Bmatrix" => ("{", "}", false, false),
            "vmatrix" => ("|", "|", false, false),
            "Vmatrix" => ("‖", "‖", false, false),
            "smallmatrix" => ("", "", true, false),
            "cases" | "dcases" => ("{", "", false, true),
            "aligned" | "alignedat" | "split" => ("", "", false, true),
            "gathered" | "matrix" | "array" => ("", "", false, false),
            _ => ("", "", false, false),
        };
        MathAtom::Matrix {
            left: left.to_string(),
            right: right.to_string(),
            rows,
            small,
            left_aligned,
        }
    }
}

fn big_op(symbol: &str, is_integral: bool) -> MathAtom {
    MathAtom::BigOp {
        symbol: symbol.to_string(),
        is_integral,
        limits_mode: LimitsMode::Default,
    }
}

fn char_to_atom(ch: char) -> MathAtom {
    let (text, style, class) = match ch {
        '+' => ("+".to_string(), MathFontStyle::Symbol, AtomClass::Bin),
        '-' => ("−".to_string(), MathFontStyle::Symbol, AtomClass::Bin),
        '*' => ("∗".to_string(), MathFontStyle::Symbol, AtomClass::Bin),
        '=' | '<' | '>' => (ch.to_string(), MathFontStyle::Symbol, AtomClass::Rel),
        '(' | '[' | '{' => (ch.to_string(), MathFontStyle::Roman, AtomClass::Open),
        ')' | ']' | '}' => (ch.to_string(), MathFontStyle::Roman, AtomClass::Close),
        ',' | ';' => (ch.to_string(), MathFontStyle::Roman, AtomClass::Punct),
        ':' => (":".to_string(), MathFontStyle::Roman, AtomClass::Rel),
        c if c.is_ascii_alphabetic() => (c.to_string(), MathFontStyle::Italic, AtomClass::Ord),
        c => (c.to_string(), MathFontStyle::Roman, AtomClass::Ord),
    };
    MathAtom::Symbol { text, style, class }
}

fn blackboard_char(ch: char) -> char {
    match ch {
        'C' => 'ℂ',
        'H' => 'ℍ',
        'N' => 'ℕ',
        'P' => 'ℙ',
        'Q' => 'ℚ',
        'R' => 'ℝ',
        'Z' => 'ℤ',
        other => other,
    }
}

fn lookup_tex_symbol(cmd: &str) -> Option<(&'static str, MathFontStyle, AtomClass)> {
    use AtomClass::*;
    use MathFontStyle::{Italic, Roman, Symbol};
    let entry = match cmd {
        // Lowercase Greek
        "alpha" => ("α", Italic, Ord),
        "beta" => ("β", Italic, Ord),
        "gamma" => ("γ", Italic, Ord),
        "delta" => ("δ", Italic, Ord),
        "epsilon" => ("ϵ", Italic, Ord),
        "varepsilon" => ("ε", Italic, Ord),
        "zeta" => ("ζ", Italic, Ord),
        "eta" => ("η", Italic, Ord),
        "theta" => ("θ", Italic, Ord),
        "vartheta" => ("ϑ", Italic, Ord),
        "iota" => ("ι", Italic, Ord),
        "kappa" => ("κ", Italic, Ord),
        "lambda" => ("λ", Italic, Ord),
        "mu" => ("μ", Italic, Ord),
        "nu" => ("ν", Italic, Ord),
        "xi" => ("ξ", Italic, Ord),
        "pi" => ("π", Italic, Ord),
        "varpi" => ("ϖ", Italic, Ord),
        "rho" => ("ρ", Italic, Ord),
        "varrho" => ("ϱ", Italic, Ord),
        "sigma" => ("σ", Italic, Ord),
        "varsigma" => ("ς", Italic, Ord),
        "tau" => ("τ", Italic, Ord),
        "upsilon" => ("υ", Italic, Ord),
        "phi" => ("ϕ", Italic, Ord),
        "varphi" => ("φ", Italic, Ord),
        "chi" => ("χ", Italic, Ord),
        "psi" => ("ψ", Italic, Ord),
        "omega" => ("ω", Italic, Ord),
        // Uppercase Greek
        "Gamma" => ("Γ", Roman, Ord),
        "Delta" => ("Δ", Roman, Ord),
        "Theta" => ("Θ", Roman, Ord),
        "Lambda" => ("Λ", Roman, Ord),
        "Xi" => ("Ξ", Roman, Ord),
        "Pi" => ("Π", Roman, Ord),
        "Sigma" => ("Σ", Roman, Ord),
        "Upsilon" => ("Υ", Roman, Ord),
        "Phi" => ("Φ", Roman, Ord),
        "Psi" => ("Ψ", Roman, Ord),
        "Omega" => ("Ω", Roman, Ord),
        // Binary operators
        "pm" => ("±", Symbol, Bin),
        "mp" => ("∓", Symbol, Bin),
        "times" => ("×", Symbol, Bin),
        "div" => ("÷", Symbol, Bin),
        "cdot" => ("·", Symbol, Bin),
        "ast" => ("∗", Symbol, Bin),
        "star" => ("⋆", Symbol, Bin),
        "circ" => ("∘", Symbol, Bin),
        "bullet" => ("•", Symbol, Bin),
        "oplus" => ("⊕", Symbol, Bin),
        "ominus" => ("⊖", Symbol, Bin),
        "otimes" => ("⊗", Symbol, Bin),
        "oslash" => ("⊘", Symbol, Bin),
        "odot" => ("⊙", Symbol, Bin),
        "cup" => ("∪", Symbol, Bin),
        "cap" => ("∩", Symbol, Bin),
        "setminus" | "smallsetminus" => ("∖", Symbol, Bin),
        "wedge" | "land" => ("∧", Symbol, Bin),
        "vee" | "lor" => ("∨", Symbol, Bin),
        "dagger" => ("†", Symbol, Bin),
        "ddagger" => ("‡", Symbol, Bin),
        // Relations
        "le" | "leq" => ("≤", Symbol, Rel),
        "ge" | "geq" => ("≥", Symbol, Rel),
        "ne" | "neq" => ("≠", Symbol, Rel),
        "approx" => ("≈", Symbol, Rel),
        "equiv" => ("≡", Symbol, Rel),
        "sim" => ("∼", Symbol, Rel),
        "simeq" => ("≃", Symbol, Rel),
        "cong" => ("≅", Symbol, Rel),
        "propto" => ("∝", Symbol, Rel),
        "ll" => ("≪", Symbol, Rel),
        "gg" => ("≫", Symbol, Rel),
        "prec" => ("≺", Symbol, Rel),
        "succ" => ("≻", Symbol, Rel),
        "in" => ("∈", Symbol, Rel),
        "notin" => ("∉", Symbol, Rel),
        "ni" => ("∋", Symbol, Rel),
        "subset" => ("⊂", Symbol, Rel),
        "subseteq" => ("⊆", Symbol, Rel),
        "supset" => ("⊃", Symbol, Rel),
        "supseteq" => ("⊇", Symbol, Rel),
        "perp" => ("⊥", Symbol, Rel),
        "parallel" => ("∥", Symbol, Rel),
        "mid" => ("∣", Symbol, Rel),
        "vdash" => ("⊢", Symbol, Rel),
        "models" => ("⊨", Symbol, Rel),
        // Arrows
        "to" | "rightarrow" => ("→", Symbol, Rel),
        "leftarrow" | "gets" => ("←", Symbol, Rel),
        "leftrightarrow" => ("↔", Symbol, Rel),
        "Rightarrow" | "implies" => ("⇒", Symbol, Rel),
        "Leftarrow" => ("⇐", Symbol, Rel),
        "Leftrightarrow" | "iff" => ("⇔", Symbol, Rel),
        "longrightarrow" => ("⟶", Symbol, Rel),
        "longleftarrow" => ("⟵", Symbol, Rel),
        "longleftrightarrow" => ("⟷", Symbol, Rel),
        "mapsto" => ("↦", Symbol, Rel),
        "uparrow" => ("↑", Symbol, Rel),
        "downarrow" => ("↓", Symbol, Rel),
        "nearrow" => ("↗", Symbol, Rel),
        "searrow" => ("↘", Symbol, Rel),
        // Misc math symbols
        "infty" => ("∞", Symbol, Ord),
        "partial" => ("∂", Symbol, Ord),
        "nabla" => ("∇", Symbol, Ord),
        "forall" => ("∀", Symbol, Ord),
        "exists" => ("∃", Symbol, Ord),
        "nexists" => ("∄", Symbol, Ord),
        "neg" | "lnot" => ("¬", Symbol, Ord),
        "emptyset" => ("∅", Symbol, Ord),
        "varnothing" => ("∅", Symbol, Ord),
        "hbar" => ("ℏ", Italic, Ord),
        "ell" => ("ℓ", Italic, Ord),
        "Re" => ("ℜ", Symbol, Ord),
        "Im" => ("ℑ", Symbol, Ord),
        "aleph" => ("ℵ", Symbol, Ord),
        "wp" => ("℘", Symbol, Ord),
        "angle" => ("∠", Symbol, Ord),
        "triangle" => ("△", Symbol, Ord),
        "ldots" | "dots" => ("…", Roman, Ord),
        "cdots" => ("⋯", Symbol, Ord),
        "vdots" => ("⋮", Symbol, Ord),
        "ddots" => ("⋱", Symbol, Ord),
        "prime" => ("′", Symbol, Ord),
        "degree" => ("°", Symbol, Ord),
        "{" | "lbrace" => ("{", Roman, Open),
        "}" | "rbrace" => ("}", Roman, Close),
        "|" | "Vert" => ("‖", Symbol, Ord),
        "vert" => ("|", Symbol, Ord),
        "langle" => ("⟨", Symbol, Open),
        "rangle" => ("⟩", Symbol, Close),
        "lfloor" => ("⌊", Symbol, Open),
        "rfloor" => ("⌋", Symbol, Close),
        "lceil" => ("⌈", Symbol, Open),
        "rceil" => ("⌉", Symbol, Close),
        "%" => ("%", Roman, Ord),
        "$" => ("$", Roman, Ord),
        "#" => ("#", Roman, Ord),
        "&" => ("&", Roman, Ord),
        "_" => ("_", Roman, Ord),
        _ => return None,
    };
    Some(entry)
}

// ============================================================================
// Pure-Rust Knuth Box Layout Engine
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeXStyle {
    Display,
    Text,
    Script,
    ScriptScript,
}

impl TeXStyle {
    fn font_size(self, base: f32) -> f32 {
        let factor = match self {
            Self::Display | Self::Text => 1.0,
            Self::Script => SCRIPT_SCALE,
            Self::ScriptScript => SCRIPTSCRIPT_SCALE,
        };
        (base * factor).max(MIN_FONT_SIZE)
    }

    const fn script_style(self) -> Self {
        match self {
            Self::Display | Self::Text => Self::Script,
            Self::Script | Self::ScriptScript => Self::ScriptScript,
        }
    }

    const fn frac_child_style(self) -> Self {
        match self {
            Self::Display => Self::Text,
            Self::Text => Self::Script,
            Self::Script | Self::ScriptScript => Self::ScriptScript,
        }
    }
}

/// Intermediate box with origin at `(0, 0)` on its own baseline:
/// elements have `y` relative to baseline (negative = above baseline, positive = below baseline).
#[derive(Debug, Clone, Default)]
struct MathBox {
    width: f32,
    ascent: f32,
    depth: f32,
    class: Option<AtomClass>,
    runs: Vec<PositionedGlyphRun>,
    rules: Vec<FormulaRule>,
    polylines: Vec<FormulaPolyline>,
}

impl MathBox {
    fn shift(&mut self, dx: f32, dy: f32) {
        for r in &mut self.runs {
            r.x += dx;
            r.y += dy;
        }
        for rule in &mut self.rules {
            rule.x += dx;
            rule.y += dy;
        }
        for poly in &mut self.polylines {
            for (px, py) in &mut poly.points {
                *px += dx;
                *py += dy;
            }
        }
    }

    fn merge_shifted(&mut self, mut child: MathBox, dx: f32, dy: f32) {
        child.shift(dx, dy);
        self.ascent = self.ascent.max(child.ascent - dy);
        self.depth = self.depth.max(child.depth + dy);
        self.width = self.width.max(dx + child.width);
        self.runs.extend(child.runs);
        self.rules.extend(child.rules);
        self.polylines.extend(child.polylines);
    }

    fn into_layout(mut self) -> FormulaLayout {
        let ascent = self.ascent.max(1.0);
        let depth = self.depth.max(0.0);
        // Shift from baseline-relative y=0 to top-left-relative y=ascent
        self.shift(0.0, ascent);
        FormulaLayout {
            width: self.width.max(1.0),
            height: ascent + depth,
            ascent,
            depth,
            glyph_runs: self.runs,
            rules: self.rules,
            polylines: self.polylines,
        }
    }
}

fn approx_char_advance(ch: char, font_size: f32, style: MathFontStyle) -> f32 {
    let ratio = match ch {
        ' ' => 0.30,
        'i' | 'l' | 'j' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' | '′' | '·' => 0.30,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '{' | '}' | '/' | '\\' => 0.38,
        'm' | 'w' | 'M' | 'W' | '∑' | '∏' | '∐' | '⋃' | '⋂' | '⨁' | '⨂' | '∞' | '⟶' | '⟵' | '⟷' => {
            0.85
        }
        '∫' | '∮' => 0.55,
        '∬' => 0.85,
        '∭' => 1.15,
        'A'..='Z' | 'Γ' | 'Δ' | 'Θ' | 'Λ' | 'Ξ' | 'Π' | 'Σ' | 'Φ' | 'Ψ' | 'Ω' => 0.68,
        '0'..='9' => 0.56,
        _ => 0.56,
    };
    let style_factor = match style {
        MathFontStyle::Bold | MathFontStyle::BoldItalic => 1.06,
        MathFontStyle::Monospace => 1.04,
        _ => 1.0,
    };
    font_size * ratio * style_factor
}

fn measure_text_width(text: &str, font_size: f32, style: MathFontStyle) -> f32 {
    text.chars()
        .map(|c| approx_char_advance(c, font_size, style))
        .sum()
}

fn inter_atom_space(prev: AtomClass, next: AtomClass, style: TeXStyle, em: f32) -> f32 {
    use AtomClass::*;
    let in_script = matches!(style, TeXStyle::Script | TeXStyle::ScriptScript);
    let ratio = match (prev, next) {
        (Rel, _) | (_, Rel) if !in_script => THICK_SPACE,
        (Bin, _) | (_, Bin) if !in_script => MED_SPACE,
        (Op, Ord | Op) | (Ord | Close, Op) => THIN_SPACE,
        (Punct, _) if !in_script => THIN_SPACE,
        _ => 0.0,
    };
    ratio * em
}

fn build_formula_layout(atoms: &[MathAtom], style: TeXStyle, base_size: f32) -> FormulaLayout {
    layout_sequence(atoms, style, base_size, None).into_layout()
}

fn layout_sequence(
    atoms: &[MathAtom],
    mut style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let mut out = MathBox::default();
    let mut cursor_x = 0.0f32;
    let mut prev_class: Option<AtomClass> = None;

    for atom in atoms {
        if let MathAtom::StyleSwitch(new_style) = atom {
            style = *new_style;
            continue;
        }
        let child = layout_atom(atom, style, base_size, font_override, prev_class);
        let child_class = child.class;
        if let (Some(p), Some(n)) = (prev_class, child_class) {
            let gap = inter_atom_space(p, n, style, style.font_size(base_size));
            cursor_x += gap;
        }
        let advance = child.width;
        out.merge_shifted(child, cursor_x, 0.0);
        cursor_x += advance;
        if child_class.is_some() {
            prev_class = child_class;
        }
    }
    out.width = out.width.max(cursor_x);
    out
}

fn layout_atom(
    atom: &MathAtom,
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
    prev_class: Option<AtomClass>,
) -> MathBox {
    let em = style.font_size(base_size);
    match atom {
        MathAtom::StyleSwitch(_) => MathBox::default(),
        MathAtom::Space(ratio) => MathBox {
            width: (*ratio * em).max(0.0),
            ..MathBox::default()
        },
        MathAtom::Symbol {
            text,
            style: sym_style,
            class,
        } => {
            // Unary leading +/- becomes Ord instead of Bin
            let effective_class = if *class == AtomClass::Bin
                && matches!(
                    prev_class,
                    None | Some(
                        AtomClass::Bin
                            | AtomClass::Rel
                            | AtomClass::Open
                            | AtomClass::Punct
                            | AtomClass::Op
                    )
                ) {
                AtomClass::Ord
            } else {
                *class
            };
            let chosen_style = font_override.unwrap_or(*sym_style);
            let w = measure_text_width(text, em, chosen_style);
            let ascent = em * ASCENT_RATIO;
            let depth = em * DEPTH_RATIO;
            let mut b = MathBox {
                width: w,
                ascent,
                depth,
                class: Some(effective_class),
                ..MathBox::default()
            };
            if !text.is_empty() {
                b.runs.push(PositionedGlyphRun {
                    x: 0.0,
                    y: 0.0,
                    width: w,
                    ascent,
                    depth,
                    text: text.clone(),
                    font_size: em,
                    style: chosen_style,
                });
            }
            b
        }
        MathAtom::BigOp {
            symbol,
            is_integral,
            ..
        } => layout_big_op_glyph(symbol, *is_integral, style, em),
        MathAtom::Group(inner) => {
            let mut b = layout_sequence(inner, style, base_size, font_override);
            b.class = Some(AtomClass::Ord);
            b
        }
        MathAtom::Styled { style: st, body } => {
            let mut b = layout_sequence(body, style, base_size, Some(*st));
            b.class = Some(AtomClass::Ord);
            b
        }
        MathAtom::Frac {
            num,
            den,
            rule,
            forced_style,
        } => layout_fraction(
            num,
            den,
            *rule,
            forced_style.unwrap_or(style),
            base_size,
            font_override,
        ),
        MathAtom::Sqrt { index, radicand } => {
            layout_sqrt(index.as_deref(), radicand, style, base_size, font_override)
        }
        MathAtom::Scripts { base, sub, sup } => layout_scripts(
            base,
            sub.as_deref(),
            sup.as_deref(),
            style,
            base_size,
            font_override,
            prev_class,
        ),
        MathAtom::Overline(body) => layout_overline(body, style, base_size, font_override),
        MathAtom::Underline(body) => layout_underline(body, style, base_size, font_override),
        MathAtom::Accent {
            accent,
            body,
            is_wide,
        } => layout_accent(accent, body, *is_wide, style, base_size, font_override),
        MathAtom::Delimited { left, right, body } => {
            let inner = layout_sequence(body, style, base_size, font_override);
            wrap_with_delimiters(inner, left, right, em)
        }
        MathAtom::Matrix {
            left,
            right,
            rows,
            small,
            left_aligned,
        } => layout_matrix(
            left,
            right,
            rows,
            *small,
            *left_aligned,
            style,
            base_size,
            font_override,
        ),
    }
}

fn layout_big_op_glyph(symbol: &str, is_integral: bool, style: TeXStyle, em: f32) -> MathBox {
    let is_word = symbol.chars().all(|c| c.is_ascii_alphabetic() || c == ' ');
    let op_size = if is_word {
        em
    } else if style == TeXStyle::Display {
        em * BIG_OP_DISPLAY_SCALE
    } else {
        em * BIG_OP_TEXT_SCALE
    };
    let op_style = if is_word {
        MathFontStyle::Roman
    } else {
        MathFontStyle::Symbol
    };
    let w = measure_text_width(symbol, op_size, op_style);
    let ascent = if is_word {
        op_size * ASCENT_RATIO
    } else if is_integral {
        op_size * 0.85
    } else {
        op_size * 0.72
    };
    let depth = if is_word {
        op_size * DEPTH_RATIO
    } else if is_integral {
        op_size * 0.45
    } else {
        op_size * 0.34
    };
    let axis = em * MATH_AXIS_RATIO;
    let baseline_y = if is_word {
        0.0
    } else {
        ((ascent - depth) * 0.5) - axis
    };
    let mut b = MathBox {
        width: w,
        ascent: (ascent - baseline_y).max(op_size * 0.5),
        depth: (depth + baseline_y).max(op_size * 0.2),
        class: Some(AtomClass::Op),
        ..MathBox::default()
    };
    b.runs.push(PositionedGlyphRun {
        x: 0.0,
        y: baseline_y,
        width: w,
        ascent,
        depth,
        text: symbol.to_string(),
        font_size: op_size,
        style: op_style,
    });
    b
}

fn layout_fraction(
    num: &[MathAtom],
    den: &[MathAtom],
    draw_rule: bool,
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let child_style = style.frac_child_style();
    let num_box = layout_sequence(num, child_style, base_size, font_override);
    let den_box = layout_sequence(den, child_style, base_size, font_override);

    let axis = em * MATH_AXIS_RATIO;
    let rule_h = (em * RULE_THICKNESS_RATIO).max(1.0);
    let gap = em
        * if style == TeXStyle::Display {
            FRAC_GAP_DISPLAY
        } else {
            FRAC_GAP_TEXT
        };
    let pad_x = em * FRAC_PAD_X;
    let inner_w = num_box.width.max(den_box.width).max(em * 0.4);
    let total_w = inner_w + pad_x * 2.0;

    let num_x = (total_w - num_box.width) * 0.5;
    let den_x = (total_w - den_box.width) * 0.5;

    // Position numerator baseline so its bottom (`num_y + num_box.depth`) clears the rule top
    let rule_y = -axis - rule_h * 0.5;
    let num_y = rule_y - gap - num_box.depth;
    let den_y = (rule_y + rule_h) + gap + den_box.ascent;

    let mut out = MathBox {
        width: total_w,
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };
    out.merge_shifted(num_box, num_x, num_y);
    out.merge_shifted(den_box, den_x, den_y);
    if draw_rule {
        out.rules.push(FormulaRule {
            x: pad_x * 0.5,
            y: rule_y,
            width: total_w - pad_x,
            height: rule_h,
        });
    }
    out
}

fn layout_sqrt(
    index: Option<&[MathAtom]>,
    radicand: &[MathAtom],
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let inner = layout_sequence(radicand, style, base_size, font_override);
    let rule_h = (em * RULE_THICKNESS_RATIO).max(1.0);
    let gap = em * 0.14;
    let rad_w = em * 0.55;
    let top_y = -(inner.ascent + gap + rule_h);
    let bot_y = inner.depth.max(em * 0.1);
    let mid_y = top_y + (bot_y - top_y) * 0.58;

    let mut index_width = 0.0f32;
    let idx_box = index.map(|idx_atoms| {
        let b = layout_sequence(idx_atoms, TeXStyle::ScriptScript, base_size, font_override);
        index_width = (b.width - rad_w * 0.35).max(0.0);
        b
    });

    let rad_x = index_width;
    let body_x = rad_x + rad_w + em * 0.06;
    let total_w = body_x + inner.width + em * 0.08;

    let mut out = MathBox {
        width: total_w,
        ascent: (-top_y) + rule_h * 0.5,
        depth: bot_y,
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };

    if let Some(ib) = idx_box {
        let ix = (rad_x + rad_w * 0.35 - ib.width).max(0.0);
        let iy = mid_y - em * 0.12 - ib.depth;
        out.merge_shifted(ib, ix, iy);
    }

    // Radical checkmark polyline
    out.polylines.push(FormulaPolyline {
        points: vec![
            (rad_x, mid_y + em * 0.06),
            (rad_x + rad_w * 0.25, mid_y),
            (rad_x + rad_w * 0.52, bot_y),
            (rad_x + rad_w, top_y + rule_h * 0.5),
        ],
        stroke_width: rule_h * 1.15,
    });

    // Vinculum top bar spanning the radicand
    out.rules.push(FormulaRule {
        x: rad_x + rad_w,
        y: top_y,
        width: total_w - (rad_x + rad_w),
        height: rule_h,
    });

    out.merge_shifted(inner, body_x, 0.0);
    out
}

fn layout_scripts(
    base: &MathAtom,
    sub: Option<&[MathAtom]>,
    sup: Option<&[MathAtom]>,
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
    prev_class: Option<AtomClass>,
) -> MathBox {
    let em = style.font_size(base_size);
    let script_style = style.script_style();
    let base_box = layout_atom(base, style, base_size, font_override, prev_class);

    // Check if this is a big operator with limits placed above & below
    let place_above_below = match base {
        MathAtom::BigOp {
            is_integral,
            limits_mode,
            ..
        } => match limits_mode {
            LimitsMode::Limits => true,
            LimitsMode::NoLimits => false,
            LimitsMode::Default => style == TeXStyle::Display && !is_integral,
        },
        _ => false,
    };

    let sub_box = sub.map(|s| layout_sequence(s, script_style, base_size, font_override));
    let sup_box = sup.map(|s| layout_sequence(s, script_style, base_size, font_override));

    if place_above_below {
        let gap = em * 0.14;
        let max_w = base_box
            .width
            .max(sub_box.as_ref().map_or(0.0, |b| b.width))
            .max(sup_box.as_ref().map_or(0.0, |b| b.width));
        let base_ascent = base_box.ascent;
        let base_depth = base_box.depth;
        let mut out = MathBox {
            width: max_w,
            class: base_box.class,
            ..MathBox::default()
        };
        let bx = (max_w - base_box.width) * 0.5;
        out.merge_shifted(base_box, bx, 0.0);
        if let Some(sup_b) = sup_box {
            let sx = (max_w - sup_b.width) * 0.5;
            let sy = -(base_ascent + gap + sup_b.depth);
            out.merge_shifted(sup_b, sx, sy);
        }
        if let Some(sub_b) = sub_box {
            let sx = (max_w - sub_b.width) * 0.5;
            let sy = base_depth + gap + sub_b.ascent;
            out.merge_shifted(sub_b, sx, sy);
        }
        return out;
    }

    // Standard side-script placement (Knuth TeXbook Rule 18)
    let script_x = base_box.width + em * 0.04;
    let sup_shift_base = em
        * if style == TeXStyle::Display {
            SUP_SHIFT_DISPLAY
        } else {
            SUP_SHIFT_TEXT
        };
    let mut sup_y = -(sup_shift_base.max(base_box.ascent - em * 0.32));
    let mut sub_y = (em * SUB_SHIFT_BASE).max(base_box.depth + em * 0.08);

    if let (Some(sup_b), Some(sub_b)) = (&sup_box, &sub_box) {
        let min_gap = em * 0.12;
        let sup_bottom = sup_y + sup_b.depth;
        let sub_top = sub_y - sub_b.ascent;
        if sub_top - sup_bottom < min_gap {
            let adjust = (min_gap - (sub_top - sup_bottom)) * 0.5;
            sup_y -= adjust;
            sub_y += adjust;
        }
    }

    let mut out = MathBox {
        class: base_box.class,
        ..MathBox::default()
    };
    out.merge_shifted(base_box, 0.0, 0.0);
    if let Some(sup_b) = sup_box {
        out.merge_shifted(sup_b, script_x, sup_y);
    }
    if let Some(sub_b) = sub_box {
        out.merge_shifted(sub_b, script_x, sub_y);
    }
    out.width += em * 0.04;
    out
}

fn layout_overline(
    body: &[MathAtom],
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let inner = layout_sequence(body, style, base_size, font_override);
    let rule_h = (em * RULE_THICKNESS_RATIO).max(1.0);
    let gap = em * 0.12;
    let rule_y = -(inner.ascent + gap + rule_h);
    let w = inner.width;
    let mut out = MathBox {
        width: w,
        ascent: (-rule_y) + rule_h * 0.5,
        depth: inner.depth,
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };
    out.merge_shifted(inner, 0.0, 0.0);
    out.rules.push(FormulaRule {
        x: 0.0,
        y: rule_y,
        width: w,
        height: rule_h,
    });
    out
}

fn layout_underline(
    body: &[MathAtom],
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let inner = layout_sequence(body, style, base_size, font_override);
    let rule_h = (em * RULE_THICKNESS_RATIO).max(1.0);
    let gap = em * 0.12;
    let rule_y = inner.depth + gap;
    let w = inner.width;
    let mut out = MathBox {
        width: w,
        ascent: inner.ascent,
        depth: rule_y + rule_h,
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };
    out.merge_shifted(inner, 0.0, 0.0);
    out.rules.push(FormulaRule {
        x: 0.0,
        y: rule_y,
        width: w,
        height: rule_h,
    });
    out
}

fn layout_accent(
    accent: &str,
    body: &[MathAtom],
    is_wide: bool,
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let inner = layout_sequence(body, style, base_size, font_override);
    let acc_size = if is_wide { em * 0.9 } else { em * 0.78 };
    let acc_w = measure_text_width(accent, acc_size, MathFontStyle::Symbol);
    let total_w = inner.width.max(acc_w);
    let body_x = (total_w - inner.width) * 0.5;
    let acc_x = (total_w - acc_w) * 0.5;
    let acc_y = -(inner.ascent + em * 0.06);
    let mut out = MathBox {
        width: total_w,
        ascent: inner.ascent + acc_size * 0.45,
        depth: inner.depth,
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };
    out.merge_shifted(inner, body_x, 0.0);
    out.runs.push(PositionedGlyphRun {
        x: acc_x,
        y: acc_y,
        width: acc_w,
        ascent: acc_size * 0.4,
        depth: 0.0,
        text: accent.to_string(),
        font_size: acc_size,
        style: MathFontStyle::Symbol,
    });
    out
}

fn build_scaled_delimiter(delim: &str, target_height: f32, em: f32) -> MathBox {
    if delim.is_empty() {
        return MathBox::default();
    }
    let scale_h = target_height.max(em * (ASCENT_RATIO + DEPTH_RATIO));
    let axis = em * MATH_AXIS_RATIO;
    let half = scale_h * 0.5;
    let top_y = -axis - half;
    let bot_y = -axis + half;
    let rule_t = (em * RULE_THICKNESS_RATIO).max(1.0);

    // Tall vertical bars are drawn as crisp rules; other delimiters use scaled glyph runs
    if delim == "|" {
        let w = em * 0.28;
        let mut b = MathBox {
            width: w,
            ascent: (-top_y).max(0.0),
            depth: bot_y.max(0.0),
            ..MathBox::default()
        };
        b.rules.push(FormulaRule {
            x: (w - rule_t) * 0.5,
            y: top_y,
            width: rule_t,
            height: scale_h,
        });
        return b;
    }
    if delim == "‖" {
        let w = em * 0.42;
        let mut b = MathBox {
            width: w,
            ascent: (-top_y).max(0.0),
            depth: bot_y.max(0.0),
            ..MathBox::default()
        };
        b.rules.push(FormulaRule {
            x: w * 0.25,
            y: top_y,
            width: rule_t,
            height: scale_h,
        });
        b.rules.push(FormulaRule {
            x: w * 0.65,
            y: top_y,
            width: rule_t,
            height: scale_h,
        });
        return b;
    }
    if delim == "[" || delim == "]" {
        let w = em * 0.36;
        let cap_w = w * 0.6;
        let stem_x = if delim == "[" {
            w * 0.25
        } else {
            w * 0.75 - rule_t
        };
        let cap_x = if delim == "[" {
            stem_x
        } else {
            stem_x - cap_w + rule_t
        };
        let mut b = MathBox {
            width: w,
            ascent: (-top_y).max(0.0),
            depth: bot_y.max(0.0),
            ..MathBox::default()
        };
        b.rules.push(FormulaRule {
            x: stem_x,
            y: top_y,
            width: rule_t,
            height: scale_h,
        });
        b.rules.push(FormulaRule {
            x: cap_x,
            y: top_y,
            width: cap_w,
            height: rule_t,
        });
        b.rules.push(FormulaRule {
            x: cap_x,
            y: bot_y - rule_t,
            width: cap_w,
            height: rule_t,
        });
        return b;
    }

    let font_size = (scale_h * 0.92).max(em);
    let w = measure_text_width(delim, font_size, MathFontStyle::Symbol).max(em * 0.35);
    let ascent = font_size * ASCENT_RATIO;
    let depth = font_size * DEPTH_RATIO;
    let baseline_y = ((ascent - depth) * 0.5) - axis;
    let mut b = MathBox {
        width: w,
        ascent: (-top_y).max(ascent - baseline_y),
        depth: bot_y.max(depth + baseline_y),
        ..MathBox::default()
    };
    b.runs.push(PositionedGlyphRun {
        x: 0.0,
        y: baseline_y,
        width: w,
        ascent,
        depth,
        text: delim.to_string(),
        font_size,
        style: MathFontStyle::Symbol,
    });
    b
}

fn wrap_with_delimiters(inner: MathBox, left: &str, right: &str, em: f32) -> MathBox {
    let axis = em * MATH_AXIS_RATIO;
    let half_span = (inner.ascent - axis).max(inner.depth + axis).max(em * 0.55);
    let target_h = half_span * 2.0 + em * 0.1;

    let mut out = MathBox {
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };
    let mut x = 0.0f32;
    if !left.is_empty() {
        let lb = build_scaled_delimiter(left, target_h, em);
        let lw = lb.width;
        out.merge_shifted(lb, x, 0.0);
        x += lw + em * 0.06;
    }
    let iw = inner.width;
    out.merge_shifted(inner, x, 0.0);
    x += iw;
    if !right.is_empty() {
        x += em * 0.06;
        let rb = build_scaled_delimiter(right, target_h, em);
        let rw = rb.width;
        out.merge_shifted(rb, x, 0.0);
        x += rw;
    }
    out.width = out.width.max(x);
    out
}

#[allow(clippy::too_many_arguments)]
fn layout_matrix(
    left: &str,
    right: &str,
    rows: &[Vec<Vec<MathAtom>>],
    small: bool,
    left_aligned: bool,
    style: TeXStyle,
    base_size: f32,
    font_override: Option<MathFontStyle>,
) -> MathBox {
    let em = style.font_size(base_size);
    let cell_style = if small {
        TeXStyle::Script
    } else if style == TeXStyle::Display {
        TeXStyle::Text
    } else {
        style
    };
    let cell_em = cell_style.font_size(base_size);
    let col_gap = cell_em * if small { 0.45 } else { MATRIX_COL_GAP };
    let row_gap = cell_em * if small { 0.18 } else { MATRIX_ROW_GAP };

    let num_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || num_cols == 0 {
        return MathBox::default();
    }

    let mut cell_boxes: Vec<Vec<MathBox>> = Vec::with_capacity(rows.len());
    let mut col_widths = vec![0.0f32; num_cols];
    let mut row_ascents = vec![cell_em * ASCENT_RATIO; rows.len()];
    let mut row_depths = vec![cell_em * DEPTH_RATIO; rows.len()];

    for (r_idx, row) in rows.iter().enumerate() {
        let mut r_boxes = Vec::with_capacity(row.len());
        for (c_idx, cell_atoms) in row.iter().enumerate() {
            let cb = layout_sequence(cell_atoms, cell_style, base_size, font_override);
            col_widths[c_idx] = col_widths[c_idx].max(cb.width);
            row_ascents[r_idx] = row_ascents[r_idx].max(cb.ascent);
            row_depths[r_idx] = row_depths[r_idx].max(cb.depth);
            r_boxes.push(cb);
        }
        cell_boxes.push(r_boxes);
    }

    let total_w: f32 = col_widths.iter().sum::<f32>() + col_gap * num_cols.saturating_sub(1) as f32;
    let total_h: f32 = row_ascents
        .iter()
        .zip(&row_depths)
        .map(|(a, d)| a + d)
        .sum::<f32>()
        + row_gap * rows.len().saturating_sub(1) as f32;

    let axis = em * MATH_AXIS_RATIO;
    let mut cursor_y = -axis - total_h * 0.5;
    let mut grid = MathBox {
        width: total_w,
        ascent: (axis + total_h * 0.5).max(0.0),
        depth: (total_h * 0.5 - axis).max(0.0),
        class: Some(AtomClass::Ord),
        ..MathBox::default()
    };

    for (r_idx, r_boxes) in cell_boxes.into_iter().enumerate() {
        let baseline_y = cursor_y + row_ascents[r_idx];
        let mut cursor_x = 0.0f32;
        for (c_idx, cb) in r_boxes.into_iter().enumerate() {
            let cell_x = if left_aligned {
                cursor_x
            } else {
                cursor_x + (col_widths[c_idx] - cb.width) * 0.5
            };
            grid.merge_shifted(cb, cell_x, baseline_y);
            cursor_x += col_widths[c_idx] + col_gap;
        }
        cursor_y += row_ascents[r_idx] + row_depths[r_idx] + row_gap;
    }

    if left.is_empty() && right.is_empty() {
        grid
    } else {
        wrap_with_delimiters(grid, left, right, em)
    }
}
