//! Port of `Telegram/SourceFiles/iv/editor/iv_editor_math_box.h` and `iv_editor_math_box.cpp`.
//!
//! Provides inline/display LaTeX formula validation (delimiter matching, environment balance,
//! length caps, and command argument checks) together with the headless [`MathBoxState`] and
//! [`MathPreviewState`] model driven by [`crate::markdown::formula::layout_formula`].

use thiserror::Error;

use crate::editor::text_ops::formula_source_to_rich_text;
use crate::markdown::document::MathKind;
use crate::markdown::formula::{
    FormulaLayoutRequest, MAX_FORMULA_TEX_LENGTH, RenderedFormula, layout_formula,
};
use crate::rich_page::{RichBlock, TextWithEntities};

/// Built-in LaTeX preview samples matching `kFormulaSamples` in `iv_editor_math_box.cpp`.
pub const FORMULA_SAMPLES: &[&str] = &[
    r"e^{i\pi}=-1",
    r"x^n+y^n=z^n",
    r"\sin^2\alpha+\cos^2\alpha=1",
    r"x_{1,2}=\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
];

/// Default minimum preview height in logical pixels (`st::ivFormulaPreviewMinHeight`).
pub const DEFAULT_PREVIEW_MIN_HEIGHT: i32 = 72;

/// Default horizontal preview padding per side in logical pixels.
pub const DEFAULT_PREVIEW_PADDING_X: i32 = 16;

/// Default vertical preview padding per side in logical pixels.
pub const DEFAULT_PREVIEW_PADDING_Y: i32 = 16;

/// Default font size for display math preview in logical pixels.
pub const DEFAULT_DISPLAY_TEXT_SIZE: i32 = 22;

/// Default font size for inline math preview in logical pixels.
pub const DEFAULT_INLINE_TEXT_SIZE: i32 = 16;

/// Default maximum render width cap in logical pixels.
pub const DEFAULT_RENDER_WIDTH_CAP: i32 = 2048;

/// Default maximum render height cap in logical pixels.
pub const DEFAULT_RENDER_HEIGHT_CAP: i32 = 1024;

/// Validation error returned when a LaTeX math expression is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MathValidationError {
    /// Formula source is empty or whitespace-only.
    #[error("formula is empty")]
    Empty,

    /// Formula exceeds [`MAX_FORMULA_TEX_LENGTH`].
    #[error("formula length {len} exceeds maximum {max}")]
    TooLong {
        /// Actual length in characters.
        len: usize,
        /// Maximum allowed length.
        max: usize,
    },

    /// An opening delimiter (`{`, `(`, `[`, `\left`, `\begin{...}`, `$`, `$$`, `\(`, `\[`)
    /// was never closed.
    #[error("unclosed delimiter (expected `{expected}`, opened at byte {opened_at})")]
    UnclosedDelimiter {
        /// Expected closing delimiter token.
        expected: String,
        /// Byte offset where the delimiter was opened.
        opened_at: usize,
    },

    /// A closing delimiter appeared with no matching opening delimiter.
    #[error("unexpected closing delimiter `{found}` at byte {at}")]
    UnexpectedClosingDelimiter {
        /// Unexpected closing delimiter token.
        found: String,
        /// Byte offset where the unexpected delimiter appeared.
        at: usize,
    },

    /// A closing delimiter did not match the most recently opened delimiter.
    #[error(
        "mismatched delimiter: expected `{expected}` (opened at byte {opened_at}), found `{found}` at byte {closed_at}"
    )]
    MismatchedDelimiter {
        /// Expected closing delimiter token.
        expected: String,
        /// Actual closing delimiter encountered.
        found: String,
        /// Byte offset of the opening delimiter.
        opened_at: usize,
        /// Byte offset of the closing delimiter.
        closed_at: usize,
    },

    /// A trailing unescaped backslash `\` appeared at the end of the expression.
    #[error("dangling backslash at byte {at}")]
    TrailingBackslash {
        /// Byte offset of the trailing backslash.
        at: usize,
    },

    /// A command requiring an argument or delimiter token (such as `\left`, `\right`,
    /// `\begin`, `\end`) was missing its operand.
    #[error("missing argument for `\\{command}` at byte {at}")]
    MissingCommandArgument {
        /// Command name without leading backslash.
        command: String,
        /// Byte offset of the command.
        at: usize,
    },

    /// An unescaped `$` appeared inside the formula body.
    #[error("unexpected unescaped `$` at byte {at}")]
    UnescapedDollar {
        /// Byte offset of the `$`.
        at: usize,
    },
}

/// Normalizes a math source string the same way `MathPreview::setSource` and `EditMathBox::submit`
/// do: trims outer whitespace and replaces `\r\n`, `\r`, and `\n` with spaces.
#[must_use]
pub fn normalize_math_source(source: &str) -> String {
    let trimmed = source.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push(' ');
        } else if ch == '\n' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Strips optional outer TeX math delimiters (`$$...$$`, `$...$`, `\[...\]`, `\(...\)`)
/// and detects the implied [`MathKind`], rejecting mismatched or unclosed outer delimiters.
pub fn strip_math_delimiters(
    source: &str,
) -> Result<(String, Option<MathKind>), MathValidationError> {
    let normalized = normalize_math_source(source);
    if normalized.is_empty() {
        return Err(MathValidationError::Empty);
    }

    if let Some(rest) = normalized.strip_prefix("$$") {
        if let Some(inner) = rest.strip_suffix("$$") {
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                return Err(MathValidationError::Empty);
            }
            return Ok((trimmed.to_string(), Some(MathKind::Display)));
        }
        if let Some(bad_end) = rest.strip_suffix('$') {
            return Err(MathValidationError::MismatchedDelimiter {
                expected: "$$".to_string(),
                found: "$".to_string(),
                opened_at: 0,
                closed_at: 2 + bad_end.len(),
            });
        }
        return Err(MathValidationError::UnclosedDelimiter {
            expected: "$$".to_string(),
            opened_at: 0,
        });
    }

    if let Some(rest) = normalized.strip_prefix('$') {
        if rest.ends_with("$$") {
            return Err(MathValidationError::MismatchedDelimiter {
                expected: "$".to_string(),
                found: "$$".to_string(),
                opened_at: 0,
                closed_at: normalized.len() - 2,
            });
        }
        if let Some(inner) = rest.strip_suffix('$') {
            if inner.ends_with('\\') && !is_escaped_backslash(inner, inner.len() - 1) {
                return Err(MathValidationError::UnclosedDelimiter {
                    expected: "$".to_string(),
                    opened_at: 0,
                });
            }
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                return Err(MathValidationError::Empty);
            }
            return Ok((trimmed.to_string(), Some(MathKind::Inline)));
        }
        return Err(MathValidationError::UnclosedDelimiter {
            expected: "$".to_string(),
            opened_at: 0,
        });
    }

    if let Some(rest) = normalized.strip_prefix(r"\[") {
        if let Some(inner) = rest.strip_suffix(r"\]") {
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                return Err(MathValidationError::Empty);
            }
            return Ok((trimmed.to_string(), Some(MathKind::Display)));
        }
        if rest.ends_with(r"\)") {
            return Err(MathValidationError::MismatchedDelimiter {
                expected: r"\]".to_string(),
                found: r"\)".to_string(),
                opened_at: 0,
                closed_at: normalized.len() - 2,
            });
        }
        return Err(MathValidationError::UnclosedDelimiter {
            expected: r"\]".to_string(),
            opened_at: 0,
        });
    }

    if let Some(rest) = normalized.strip_prefix(r"\(") {
        if let Some(inner) = rest.strip_suffix(r"\)") {
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                return Err(MathValidationError::Empty);
            }
            return Ok((trimmed.to_string(), Some(MathKind::Inline)));
        }
        if rest.ends_with(r"\]") {
            return Err(MathValidationError::MismatchedDelimiter {
                expected: r"\)".to_string(),
                found: r"\]".to_string(),
                opened_at: 0,
                closed_at: normalized.len() - 2,
            });
        }
        return Err(MathValidationError::UnclosedDelimiter {
            expected: r"\)".to_string(),
            opened_at: 0,
        });
    }

    // Check stray closing math-mode delimiters when no opening delimiter was present.
    if normalized.ends_with(r"\]") {
        return Err(MathValidationError::UnexpectedClosingDelimiter {
            found: r"\]".to_string(),
            at: normalized.len() - 2,
        });
    }
    if normalized.ends_with(r"\)") {
        return Err(MathValidationError::UnexpectedClosingDelimiter {
            found: r"\)".to_string(),
            at: normalized.len() - 2,
        });
    }

    Ok((normalized, None))
}

fn is_escaped_backslash(s: &str, backslash_idx: usize) -> bool {
    let bytes = s.as_bytes();
    let mut count = 0usize;
    let mut idx = backslash_idx;
    while idx > 0 && bytes[idx - 1] == b'\\' {
        count += 1;
        idx -= 1;
    }
    count % 2 == 1
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenDelimiter {
    Brace { at: usize },
    Paren { at: usize },
    Bracket { at: usize },
    LeftRight { at: usize },
    Environment { name: String, at: usize },
}

impl OpenDelimiter {
    fn expected_close(&self) -> String {
        match self {
            Self::Brace { .. } => "}".to_string(),
            Self::Paren { .. } => ")".to_string(),
            Self::Bracket { .. } => "]".to_string(),
            Self::LeftRight { .. } => r"\right".to_string(),
            Self::Environment { name, .. } => format!(r"\end{{{name}}}"),
        }
    }

    const fn opened_at(&self) -> usize {
        match self {
            Self::Brace { at }
            | Self::Paren { at }
            | Self::Bracket { at }
            | Self::LeftRight { at }
            | Self::Environment { at, .. } => *at,
        }
    }
}

/// Validates a LaTeX math formula for inline or display rendering and returns the cleaned
/// TeX body (with optional matching outer `$...$`/`$$...$$`/`\(...\)`/`\[...\]` stripped).
pub fn validate_math_formula(source: &str, _kind: MathKind) -> Result<String, MathValidationError> {
    let (body, _) = strip_math_delimiters(source)?;
    let char_len = body.chars().count();
    if char_len == 0 {
        return Err(MathValidationError::Empty);
    }
    if char_len > MAX_FORMULA_TEX_LENGTH {
        return Err(MathValidationError::TooLong {
            len: char_len,
            max: MAX_FORMULA_TEX_LENGTH,
        });
    }

    let bytes = body.as_bytes();
    let mut stack: Vec<OpenDelimiter> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if i + 1 >= bytes.len() {
                    return Err(MathValidationError::TrailingBackslash { at: i });
                }
                let next = bytes[i + 1];
                if next.is_ascii_alphabetic() {
                    let cmd_start = i + 1;
                    let mut cmd_end = cmd_start;
                    while cmd_end < bytes.len() && bytes[cmd_end].is_ascii_alphabetic() {
                        cmd_end += 1;
                    }
                    let cmd = &body[cmd_start..cmd_end];
                    match cmd {
                        "left" => {
                            let after = skip_ascii_spaces(bytes, cmd_end);
                            let Some(consumed) = consume_left_right_delimiter(&body, after) else {
                                return Err(MathValidationError::MissingCommandArgument {
                                    command: "left".to_string(),
                                    at: i,
                                });
                            };
                            stack.push(OpenDelimiter::LeftRight { at: i });
                            i = consumed;
                            continue;
                        }
                        "right" => {
                            let after = skip_ascii_spaces(bytes, cmd_end);
                            let Some(consumed) = consume_left_right_delimiter(&body, after) else {
                                return Err(MathValidationError::MissingCommandArgument {
                                    command: "right".to_string(),
                                    at: i,
                                });
                            };
                            match stack.pop() {
                                Some(OpenDelimiter::LeftRight { .. }) => {}
                                Some(other) => {
                                    return Err(MathValidationError::MismatchedDelimiter {
                                        expected: other.expected_close(),
                                        found: r"\right".to_string(),
                                        opened_at: other.opened_at(),
                                        closed_at: i,
                                    });
                                }
                                None => {
                                    return Err(MathValidationError::UnexpectedClosingDelimiter {
                                        found: r"\right".to_string(),
                                        at: i,
                                    });
                                }
                            }
                            i = consumed;
                            continue;
                        }
                        "begin" => {
                            let Some((env_name, after_env)) = parse_braced_ident(&body, cmd_end)
                            else {
                                return Err(MathValidationError::MissingCommandArgument {
                                    command: "begin".to_string(),
                                    at: i,
                                });
                            };
                            stack.push(OpenDelimiter::Environment {
                                name: env_name,
                                at: i,
                            });
                            i = after_env;
                            continue;
                        }
                        "end" => {
                            let Some((env_name, after_env)) = parse_braced_ident(&body, cmd_end)
                            else {
                                return Err(MathValidationError::MissingCommandArgument {
                                    command: "end".to_string(),
                                    at: i,
                                });
                            };
                            let found_token = format!(r"\end{{{env_name}}}");
                            match stack.pop() {
                                Some(OpenDelimiter::Environment { name, at }) => {
                                    if name != env_name {
                                        return Err(MathValidationError::MismatchedDelimiter {
                                            expected: format!(r"\end{{{name}}}"),
                                            found: found_token,
                                            opened_at: at,
                                            closed_at: i,
                                        });
                                    }
                                }
                                Some(other) => {
                                    return Err(MathValidationError::MismatchedDelimiter {
                                        expected: other.expected_close(),
                                        found: found_token,
                                        opened_at: other.opened_at(),
                                        closed_at: i,
                                    });
                                }
                                None => {
                                    return Err(MathValidationError::UnexpectedClosingDelimiter {
                                        found: found_token,
                                        at: i,
                                    });
                                }
                            }
                            i = after_env;
                            continue;
                        }
                        _ => {
                            i = cmd_end;
                            continue;
                        }
                    }
                } else {
                    // Single-character escaped symbol (`\{`, `\}`, `\\`, `\$`, `\,`, etc.).
                    let Some(ch) = body[i + 1..].chars().next() else {
                        return Err(MathValidationError::TrailingBackslash { at: i });
                    };
                    if ch == '(' || ch == '[' {
                        return Err(MathValidationError::UnclosedDelimiter {
                            expected: if ch == '(' {
                                r"\)".to_string()
                            } else {
                                r"\]".to_string()
                            },
                            opened_at: i,
                        });
                    }
                    if ch == ')' || ch == ']' {
                        return Err(MathValidationError::UnexpectedClosingDelimiter {
                            found: format!("\\{ch}"),
                            at: i,
                        });
                    }
                    i += 1 + ch.len_utf8();
                    continue;
                }
            }
            b'$' => {
                return Err(MathValidationError::UnescapedDollar { at: i });
            }
            b'{' => {
                stack.push(OpenDelimiter::Brace { at: i });
                i += 1;
            }
            b'}' => {
                match stack.pop() {
                    Some(OpenDelimiter::Brace { .. }) => {}
                    Some(other) => {
                        return Err(MathValidationError::MismatchedDelimiter {
                            expected: other.expected_close(),
                            found: "}".to_string(),
                            opened_at: other.opened_at(),
                            closed_at: i,
                        });
                    }
                    None => {
                        return Err(MathValidationError::UnexpectedClosingDelimiter {
                            found: "}".to_string(),
                            at: i,
                        });
                    }
                }
                i += 1;
            }
            b'(' => {
                stack.push(OpenDelimiter::Paren { at: i });
                i += 1;
            }
            b')' => {
                match stack.pop() {
                    Some(OpenDelimiter::Paren { .. }) => {}
                    Some(other) => {
                        return Err(MathValidationError::MismatchedDelimiter {
                            expected: other.expected_close(),
                            found: ")".to_string(),
                            opened_at: other.opened_at(),
                            closed_at: i,
                        });
                    }
                    None => {
                        return Err(MathValidationError::UnexpectedClosingDelimiter {
                            found: ")".to_string(),
                            at: i,
                        });
                    }
                }
                i += 1;
            }
            b'[' => {
                stack.push(OpenDelimiter::Bracket { at: i });
                i += 1;
            }
            b']' => {
                match stack.pop() {
                    Some(OpenDelimiter::Bracket { .. }) => {}
                    Some(other) => {
                        return Err(MathValidationError::MismatchedDelimiter {
                            expected: other.expected_close(),
                            found: "]".to_string(),
                            opened_at: other.opened_at(),
                            closed_at: i,
                        });
                    }
                    None => {
                        return Err(MathValidationError::UnexpectedClosingDelimiter {
                            found: "]".to_string(),
                            at: i,
                        });
                    }
                }
                i += 1;
            }
            _ => {
                let ch = body[i..].chars().next().unwrap_or(' ');
                i += ch.len_utf8();
            }
        }
    }

    if let Some(unclosed) = stack.pop() {
        return Err(MathValidationError::UnclosedDelimiter {
            expected: unclosed.expected_close(),
            opened_at: unclosed.opened_at(),
        });
    }

    Ok(body)
}

fn skip_ascii_spaces(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn consume_left_right_delimiter(body: &str, pos: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    if pos >= bytes.len() {
        return None;
    }
    let b = bytes[pos];
    if matches!(
        b,
        b'(' | b')' | b'[' | b']' | b'|' | b'.' | b'/' | b'<' | b'>'
    ) {
        return Some(pos + 1);
    }
    if b == b'\\' && pos + 1 < bytes.len() {
        let next = bytes[pos + 1];
        if matches!(next, b'{' | b'}' | b'|' | b'\\') {
            return Some(pos + 2);
        }
        if next.is_ascii_alphabetic() {
            let mut end = pos + 1;
            while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                end += 1;
            }
            let cmd = &body[pos + 1..end];
            if matches!(
                cmd,
                "langle"
                    | "rangle"
                    | "lfloor"
                    | "rfloor"
                    | "lceil"
                    | "rceil"
                    | "lvert"
                    | "rvert"
                    | "lVert"
                    | "rVert"
                    | "vert"
                    | "Vert"
                    | "backslash"
                    | "uparrow"
                    | "downarrow"
                    | "updownarrow"
                    | "Uparrow"
                    | "Downarrow"
                    | "Updownarrow"
                    | "lbrace"
                    | "rbrace"
                    | "lbrack"
                    | "rbrack"
            ) {
                return Some(end);
            }
        }
    }
    None
}

fn parse_braced_ident(body: &str, pos: usize) -> Option<(String, usize)> {
    let bytes = body.as_bytes();
    let i = skip_ascii_spaces(bytes, pos);
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    let close = body[i + 1..].find('}')?;
    let inner = body[i + 1..i + 1 + close].trim();
    if inner.is_empty() {
        return None;
    }
    Some((inner.to_string(), i + 1 + close + 1))
}

/// Confirmed submission produced by [`MathBoxState::submit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathSubmission {
    /// Validated, normalized LaTeX source (without outer `$...$`/`$$...$$`).
    pub source: String,
    /// `true` for a separate-line display math block; `false` for inline math.
    pub separate_line: bool,
    /// Corresponding [`MathKind`].
    pub kind: MathKind,
}

/// Live preview layout state for the math editor dialog (`MathPreview` in `iv_editor_math_box.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct MathPreviewState {
    /// Currently previewed TeX source (user input or active placeholder sample).
    pub source: String,
    /// Inline vs display math mode.
    pub kind: MathKind,
    /// True when the input field is empty and the preview is showing a sample formula.
    pub using_sample: bool,
    /// Outer card width in logical pixels.
    pub card_width: i32,
    /// Computed preview surface width (at least `card_width`).
    pub preview_width: i32,
    /// Computed desired preview height (at least [`DEFAULT_PREVIEW_MIN_HEIGHT`]).
    pub desired_height: i32,
    /// Validation error if the current user input is invalid (`None` when showing sample).
    pub validation_error: Option<MathValidationError>,
    /// Layout engine output when the formula is valid and rendered cleanly.
    pub rendered: Option<RenderedFormula>,
}

impl MathPreviewState {
    fn recompute(&mut self, tex: &str, kind: MathKind, using_sample: bool) {
        self.source = tex.to_string();
        self.kind = kind;
        self.using_sample = using_sample;

        if tex.is_empty() {
            self.validation_error = None;
            self.rendered = None;
            self.relayout();
            return;
        }

        match validate_math_formula(tex, kind) {
            Ok(clean) => {
                self.validation_error = None;
                let text_size = match kind {
                    MathKind::Display => DEFAULT_DISPLAY_TEXT_SIZE,
                    MathKind::Inline => DEFAULT_INLINE_TEXT_SIZE,
                };
                let req = FormulaLayoutRequest {
                    trimmed_tex: clean,
                    kind,
                    text_size,
                    render_width_cap: DEFAULT_RENDER_WIDTH_CAP,
                    render_height_cap: DEFAULT_RENDER_HEIGHT_CAP,
                };
                let rendered = layout_formula(&req);
                if rendered.measured.success {
                    self.rendered = Some(rendered);
                } else {
                    self.rendered = None;
                }
            }
            Err(err) => {
                self.validation_error = if using_sample { None } else { Some(err) };
                self.rendered = None;
            }
        }
        self.relayout();
    }

    fn relayout(&mut self) {
        let (logical_w, logical_h) = self
            .rendered
            .as_ref()
            .map(|r| (r.measured.logical_width, r.measured.logical_height))
            .unwrap_or((0, 0));
        self.desired_height =
            DEFAULT_PREVIEW_MIN_HEIGHT.max(logical_h + 2 * DEFAULT_PREVIEW_PADDING_Y);
        self.preview_width = self
            .card_width
            .max(logical_w + 2 * DEFAULT_PREVIEW_PADDING_X);
    }
}

/// Headless state machine for the rich editor's math formula modal (`Iv::Editor::EditMathBox`).
#[derive(Debug, Clone, PartialEq)]
pub struct MathBoxState {
    /// True when editing an existing formula; false when inserting a new formula.
    pub editing_existing: bool,
    /// Optional separate-line toggle (`None` when display/inline mode is fixed by context).
    pub separate_line: Option<bool>,
    /// Raw text currently entered in the formula input field.
    pub raw_input: String,
    /// Index into [`FORMULA_SAMPLES`] used for the empty-state placeholder.
    pub sample_index: usize,
    /// Current live preview state.
    pub preview: MathPreviewState,
    /// Tracks `setExternalInteractionActive` lifecycle from `iv_editor_math_box.cpp`.
    pub external_interaction_active: bool,
    /// Last accepted submission, if [`Self::submit`] succeeded.
    pub submitted: Option<MathSubmission>,
}

impl MathBoxState {
    /// Opens a new math box state matching `EditMathBox(startSource, editingExisting, separateLine)`.
    #[must_use]
    pub fn new(
        start_source: &str,
        editing_existing: bool,
        separate_line: Option<bool>,
        card_width: i32,
    ) -> Self {
        let kind = if separate_line.unwrap_or(true) {
            MathKind::Display
        } else {
            MathKind::Inline
        };
        let mut state = Self {
            editing_existing,
            separate_line,
            raw_input: String::new(),
            sample_index: 0,
            preview: MathPreviewState {
                source: String::new(),
                kind,
                using_sample: false,
                card_width: card_width.max(0),
                preview_width: card_width.max(0),
                desired_height: DEFAULT_PREVIEW_MIN_HEIGHT,
                validation_error: None,
                rendered: None,
            },
            external_interaction_active: true,
            submitted: None,
        };
        state.set_input(start_source);
        state
    }

    /// Returns the active placeholder formula sample string.
    #[must_use]
    pub fn placeholder_sample(&self) -> &'static str {
        FORMULA_SAMPLES[self.sample_index % FORMULA_SAMPLES.len()]
    }

    /// Returns the active [`MathKind`] derived from `separate_line`.
    #[must_use]
    pub fn kind(&self) -> MathKind {
        if self.separate_line.unwrap_or(true) {
            MathKind::Display
        } else {
            MathKind::Inline
        }
    }

    /// Updates the raw formula input and refreshes live validation & preview layout.
    pub fn set_input(&mut self, text: &str) {
        let was_empty = self.raw_input.trim().is_empty();
        self.raw_input = text.to_string();
        let normalized = normalize_math_source(text);
        if normalized.is_empty() {
            if !was_empty {
                self.sample_index = (self.sample_index + 1) % FORMULA_SAMPLES.len();
            }
            let sample = self.placeholder_sample();
            self.preview.recompute(sample, self.kind(), true);
        } else {
            self.preview.recompute(&normalized, self.kind(), false);
        }
    }

    /// Updates the "separate line" (display vs inline) checkbox and re-renders the preview.
    pub fn set_separate_line(&mut self, separate_line: bool) {
        self.separate_line = Some(separate_line);
        let kind = self.kind();
        if self.preview.using_sample {
            let sample = self.placeholder_sample();
            self.preview.recompute(sample, kind, true);
        } else {
            let normalized = normalize_math_source(&self.raw_input);
            self.preview.recompute(&normalized, kind, false);
        }
    }

    /// Updates the preview card width and recomputes layout geometry.
    pub fn set_card_width(&mut self, card_width: i32) {
        self.preview.card_width = card_width.max(0);
        self.preview.relayout();
    }

    /// Advances to the next placeholder sample when the input is empty.
    pub fn cycle_sample(&mut self) {
        self.sample_index = (self.sample_index + 1) % FORMULA_SAMPLES.len();
        if self.preview.using_sample {
            let sample = self.placeholder_sample();
            self.preview.recompute(sample, self.kind(), true);
        }
    }

    /// Marks the dialog closed, clearing `external_interaction_active`.
    pub fn close(&mut self) {
        self.external_interaction_active = false;
    }

    /// Returns `true` if the current user input is non-empty and passes validation.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        validate_math_formula(&self.raw_input, self.kind()).is_ok()
    }

    /// Validates and submits the current formula, storing the [`MathSubmission`] on success.
    pub fn submit(&mut self) -> Result<MathSubmission, MathValidationError> {
        let kind = self.kind();
        let validated = validate_math_formula(&self.raw_input, kind)?;
        let submission = MathSubmission {
            source: validated,
            separate_line: self.separate_line.unwrap_or(false),
            kind,
        };
        self.submitted = Some(submission.clone());
        self.external_interaction_active = false;
        Ok(submission)
    }

    /// Validates the current input and builds a display [`RichBlock`] (`RichBlockKind::Math`).
    pub fn to_rich_block(&self) -> Result<RichBlock, MathValidationError> {
        let validated = validate_math_formula(&self.raw_input, MathKind::Display)?;
        Ok(RichBlock::math(validated))
    }

    /// Validates the current input and builds an inline formula [`TextWithEntities`].
    pub fn to_inline_rich_text(&self) -> Result<TextWithEntities, MathValidationError> {
        let validated = validate_math_formula(&self.raw_input, MathKind::Inline)?;
        Ok(formula_source_to_rich_text(&validated))
    }
}
