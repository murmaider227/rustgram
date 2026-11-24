//! Runtime loader for custom `.strings` language files.
//!
//! Port of `Telegram/SourceFiles/lang/lang_file_parser.cpp`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use thiserror::Error;

use crate::auto::{KEYS_COUNT, get_key_index};
use crate::strings_parser::strip_comments;

/// Maximum accepted custom language file size (4 MiB, matching `kLangFileLimit`).
pub const LANG_FILE_LIMIT: u64 = 4 * 1024 * 1024;

/// Error opening or decoding a custom language file.
#[derive(Debug, Error)]
pub enum FileParserError {
    /// Underlying filesystem I/O failure.
    #[error("could not read language file: {0}")]
    Io(#[from] std::io::Error),
    /// File exceeded [`LANG_FILE_LIMIT`].
    #[error("language file is too big ({0} bytes, limit is {LANG_FILE_LIMIT})")]
    TooBig(u64),
    /// File was shorter than 3 bytes or decoded to empty content.
    #[error("language file is empty or too short")]
    EmptyOrTooShort,
    /// Invalid UTF-16 encoding detected.
    #[error("could not decode UTF-16 language file")]
    InvalidUtf16,
}

/// Read a custom `.strings` file from disk, supporting UTF-8 (with or without BOM)
/// and UTF-16 LE/BE (with or without BOM), returning UTF-8 bytes.
///
/// Port of `Lang::FileParser::ReadFile`.
pub fn read_lang_file(
    absolute_path: &Path,
    relative_path: Option<&Path>,
) -> Result<Vec<u8>, FileParserError> {
    let path = relative_path
        .filter(|p| !p.as_os_str().is_empty() && p.exists())
        .unwrap_or(absolute_path);

    let meta = fs::metadata(path)?;
    if meta.len() > LANG_FILE_LIMIT {
        return Err(FileParserError::TooBig(meta.len()));
    }
    let raw = fs::read(path)?;
    decode_lang_file_bytes(&raw)
}

/// Decode raw bytes of a custom `.strings` file (UTF-8 or UTF-16 LE/BE) into UTF-8 bytes.
pub fn decode_lang_file_bytes(raw: &[u8]) -> Result<Vec<u8>, FileParserError> {
    if raw.len() < 3 {
        return Err(FileParserError::EmptyOrTooShort);
    }

    // UTF-16 LE BOM, UTF-16 BE BOM, or heuristic UTF-16 LE (second byte == 0)
    if (raw[0] == 0xFF && raw[1] == 0xFE) || (raw[0] == 0xFE && raw[1] == 0xFF) || raw[1] == 0 {
        let (big_endian, slice) = if raw[0] == 0xFF && raw[1] == 0xFE {
            (false, &raw[2..])
        } else if raw[0] == 0xFE && raw[1] == 0xFF {
            (true, &raw[2..])
        } else {
            (false, raw)
        };
        return decode_utf16(slice, big_endian);
    }

    // Heuristic UTF-16 BE without BOM (first byte == 0)
    if raw[0] == 0 {
        return decode_utf16(raw, true);
    }

    // UTF-8 with optional BOM
    let slice = if raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        &raw[3..]
    } else {
        raw
    };
    if slice.is_empty() {
        return Err(FileParserError::EmptyOrTooShort);
    }
    Ok(slice.to_vec())
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> Result<Vec<u8>, FileParserError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(FileParserError::InvalidUtf16);
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for &chunk in bytes.as_chunks::<2>().0 {
        let u = if big_endian {
            u16::from_be_bytes(chunk)
        } else {
            u16::from_le_bytes(chunk)
        };
        units.push(u);
    }
    let decoded = String::from_utf16(&units).map_err(|_| FileParserError::InvalidUtf16)?;
    if decoded.is_empty() {
        return Err(FileParserError::EmptyOrTooShort);
    }
    Ok(decoded.into_bytes())
}

type ParseCallback<'a> = Option<&'a mut dyn FnMut(&str, &[u8])>;

/// Runtime `.strings` file parser matching `Lang::FileParser`.
#[derive(Debug, Default)]
pub struct FileParser {
    errors: Vec<String>,
    warnings: Vec<String>,
    found: BTreeMap<u16, String>,
}

impl FileParser {
    /// Parse `content` (UTF-8 `.strings` bytes), invoking `callback(key, value_bytes)` for each entry.
    pub fn parse_content<F>(content: &[u8], mut callback: F) -> Self
    where
        F: FnMut(&str, &[u8]),
    {
        let mut parser = Self::default();
        let stripped = strip_comments(content);
        parser.run(&stripped, None, Some(&mut callback));
        parser
    }

    /// Read and parse `path`, collecting only the keys listed in `request`.
    pub fn parse_file_for_keys(path: &Path, request: &BTreeSet<u16>) -> Self {
        let mut parser = Self::default();
        match read_lang_file(path, Some(path)) {
            Ok(bytes) => {
                let stripped = strip_comments(&bytes);
                parser.run(&stripped, Some(request), None);
            }
            Err(e) => {
                parser.errors.push(e.to_string());
            }
        }
        parser
    }

    /// Joined parse errors (empty string if none).
    pub fn errors(&self) -> String {
        self.errors.join("\n")
    }

    /// Individual parse errors.
    pub fn errors_list(&self) -> &[String] {
        &self.errors
    }

    /// Joined parse warnings (empty string if none).
    pub fn warnings(&self) -> String {
        self.warnings.join("\n")
    }

    /// Matched entries when constructed via [`Self::parse_file_for_keys`].
    pub fn found(&self) -> &BTreeMap<u16, String> {
        &self.found
    }

    fn run(
        &mut self,
        content: &[u8],
        request: Option<&BTreeSet<u16>>,
        mut callback: ParseCallback<'_>,
    ) {
        if content.is_empty() {
            self.errors.push("Got empty lang file content".to_string());
            return;
        }
        let mut pos = 0usize;
        while pos < content.len() {
            if !self.read_key_value(content, &mut pos, request, &mut callback) {
                break;
            }
        }
    }

    fn read_key_value(
        &mut self,
        data: &[u8],
        pos: &mut usize,
        request: Option<&BTreeSet<u16>>,
        callback: &mut ParseCallback<'_>,
    ) -> bool {
        if !skip_whitespaces(data, pos) {
            return false;
        }
        if data[*pos] != b'"' {
            self.errors
                .push("Expected quote before key name!".to_string());
            return false;
        }
        *pos += 1;
        let name_start = *pos;
        while *pos < data.len() && is_file_key_byte(data[*pos]) {
            *pos += 1;
        }
        let key = std::str::from_utf8(&data[name_start..*pos]).unwrap_or("");
        if *pos >= data.len() || data[*pos] != b'"' {
            self.errors
                .push(format!("Expected quote after key name '{key}'!"));
            return false;
        }
        *pos += 1;

        if !skip_whitespaces(data, pos) {
            self.errors
                .push(format!("Unexpected end of file in key '{key}'!"));
            return false;
        }
        if data[*pos] != b'=' {
            self.errors.push(format!("'=' expected in key '{key}'!"));
            return false;
        }
        *pos += 1;
        if !skip_whitespaces(data, pos) {
            self.errors
                .push(format!("Unexpected end of file in key '{key}'!"));
            return false;
        }
        if data[*pos] != b'"' {
            self.errors
                .push(format!("Expected string after '=' in key '{key}'!"));
            return false;
        }

        let mut skipping = false;
        let mut key_index = KEYS_COUNT as u16;
        if callback.is_none() {
            if let Some(idx) = get_key_index(key) {
                key_index = idx;
                skipping = request.is_some_and(|req| !req.contains(&idx));
            } else {
                skipping = true;
            }
        }

        let mut value = Vec::new();
        *pos += 1;
        let mut start = *pos;
        while *pos < data.len() && data[*pos] != b'"' {
            if data[*pos] == b'\n' {
                self.errors
                    .push(format!("Unexpected end of string in key '{key}'!"));
                return false;
            }
            if data[*pos] == b'\\' {
                if *pos + 1 >= data.len() {
                    self.errors
                        .push(format!("Unexpected end of file in key '{key}'!"));
                    return false;
                }
                let next = data[*pos + 1];
                if next == b'"' || next == b'\\' {
                    if !skipping && *pos > start {
                        value.extend_from_slice(&data[start..*pos]);
                    }
                    *pos += 1;
                    start = *pos;
                } else if next == b'n' {
                    if !skipping && *pos > start {
                        value.extend_from_slice(&data[start..*pos]);
                    }
                    if !skipping {
                        value.push(b'\n');
                    }
                    *pos += 1;
                    start = *pos + 1;
                }
            }
            *pos += 1;
        }
        if *pos >= data.len() {
            self.errors
                .push(format!("Unexpected end of file in key '{key}'!"));
            return false;
        }
        if !skipping && *pos > start {
            value.extend_from_slice(&data[start..*pos]);
        }

        *pos += 1;
        if !skip_whitespaces(data, pos) {
            self.errors
                .push(format!("Unexpected end of file in key '{key}'!"));
            return false;
        }
        if data[*pos] != b';' {
            self.errors
                .push(format!("';' expected after \"value\" in key '{key}'!"));
            return false;
        }
        *pos += 1;
        skip_whitespaces(data, pos);

        if let Some(cb) = callback.as_deref_mut() {
            cb(key, &value);
        } else if !skipping {
            self.found
                .insert(key_index, String::from_utf8_lossy(&value).into_owned());
        }

        true
    }
}

fn is_file_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'#'
}

fn skip_whitespaces(data: &[u8], pos: &mut usize) -> bool {
    while *pos < data.len() && matches!(data[*pos], b' ' | b'\n' | b'\r' | b'\t') {
        *pos += 1;
    }
    *pos < data.len()
}
