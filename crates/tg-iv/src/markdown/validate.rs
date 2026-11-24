//! Port of `iv/markdown/iv_markdown_parse_validate.cpp`.
//!
//! Validates raw Markdown byte buffers against size limits, unsupported BOMs (UTF-16/UTF-32),
//! binary heuristics, and strict UTF-8 encoding before parsing.

use crate::markdown::document::{
    MarkdownSourceValidationResult, ParseOptions, ValidatedMarkdownSource,
};
use crate::markdown::math_extract::build_line_starts;

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF32_BE_BOM: [u8; 4] = [0x00, 0x00, 0xFE, 0xFF];
const UTF32_LE_BOM: [u8; 4] = [0xFF, 0xFE, 0x00, 0x00];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];

fn has_unsupported_unicode_bom(source: &[u8]) -> bool {
    source.starts_with(&UTF32_BE_BOM)
        || source.starts_with(&UTF32_LE_BOM)
        || source.starts_with(&UTF16_BE_BOM)
        || source.starts_with(&UTF16_LE_BOM)
}

const fn is_allowed_control(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\r')
}

/// Detects binary content using tdesktop's exact heuristic (`LooksBinary`):
/// rejects any NUL byte, or when more than 10% of bytes are non-whitespace ASCII controls.
#[must_use]
pub fn looks_binary(source: &[u8]) -> bool {
    if source.is_empty() {
        return false;
    }
    let mut control_bytes = 0usize;
    for &byte in source {
        if byte == 0 {
            return true;
        }
        if (byte < 0x20 || byte == 0x7F) && !is_allowed_control(byte) {
            control_bytes += 1;
        }
    }
    control_bytes.saturating_mul(10) > source.len()
}

fn validation_failure(source_name: String, error: &str) -> MarkdownSourceValidationResult {
    MarkdownSourceValidationResult {
        source: ValidatedMarkdownSource {
            source_name,
            ..ValidatedMarkdownSource::default()
        },
        error: error.to_string(),
        ok: false,
    }
}

/// Validates and normalizes raw Markdown source bytes (`ValidateMarkdownSourceForIv`).
///
/// Error codes match tdesktop verbatim:
/// - `"source-too-large"`
/// - `"source-unsupported-bom"`
/// - `"source-binary"`
/// - `"source-invalid-utf8"`
#[must_use]
pub fn validate_markdown_source_for_iv(
    source: &[u8],
    options: ParseOptions,
) -> MarkdownSourceValidationResult {
    if source.len() > options.limits.max_source_bytes {
        return validation_failure(options.source_name, "source-too-large");
    }
    if has_unsupported_unicode_bom(source) {
        return validation_failure(options.source_name, "source-unsupported-bom");
    }
    let stripped = source.strip_prefix(&UTF8_BOM).unwrap_or(source);
    if looks_binary(stripped) {
        return validation_failure(options.source_name, "source-binary");
    }
    let Ok(decoded_str) = std::str::from_utf8(stripped) else {
        return validation_failure(options.source_name, "source-invalid-utf8");
    };
    let normalized = stripped.to_vec();
    let line_starts = build_line_starts(&normalized);
    MarkdownSourceValidationResult {
        source: ValidatedMarkdownSource {
            normalized,
            decoded: decoded_str.to_string(),
            line_starts,
            source_name: options.source_name,
        },
        error: String::new(),
        ok: true,
    }
}
