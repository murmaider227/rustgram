//! Lint (TGB-2957): no toast or label in the UI crates formats a raw error.
//!
//! A backend error is a string such as `RPC error 400: CHANNELS_TOO_MUCH`; tdesktop never shows
//! it. This test scans `crates/tg-app` and `crates/tg-app-*` sources for `format!` calls that put
//! an error variable (`{err}`, `{e}`, `{error}`, `{err:?}`, ...) into text that reaches a toast,
//! a label or an error line. Local errors (file, OS, parse) are allowed only when listed in
//! `tests/raw_error_text_allowlist.txt`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const ERROR_VARS: [&str; 4] = ["err", "e", "error", "why"];
const SINKS: [&str; 11] = [
    "show_toast",
    "toast",
    "Toast",
    "RichText::new",
    "label(",
    "status_message",
    "error = ",
    "message:",
    "Inform",
    "notice",
    "error: Some",
];

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name.contains("test") || name.starts_with("mock") || name == "screenshots.rs"
}

/// The text of the string literal that starts the `format!` arguments at `from`, or `None`.
fn format_literal(src: &str, from: usize) -> Option<&str> {
    let rest = &src[from..];
    let open = rest.find('"')?;
    if !rest[..open]
        .trim_start_matches(['(', ' ', '\n', '\r', '\t'])
        .is_empty()
    {
        return None;
    }
    let body = &rest[open + 1..];
    let mut escaped = false;
    for (i, ch) in body.char_indices() {
        match ch {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(&body[..i]),
            _ => escaped = false,
        }
    }
    None
}

/// `{err}` / `{err:?}` in the literal, or `{}` / `{:?}` with an error variable among the
/// arguments that follow it (`args`: the text up to the end of the statement).
fn has_error_placeholder(literal: &str, args: &str) -> bool {
    let named = ERROR_VARS.iter().any(|var| {
        literal.contains(&format!("{{{var}}}")) || literal.contains(&format!("{{{var}:?}}"))
    });
    let positional = literal.contains("{}") || literal.contains("{:?}");
    named
        || (positional
            && args
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|token| ERROR_VARS.contains(&token)))
}

/// The byte index of the `)` that closes the `format!(` whose literal ends at `from`.
fn format_args_end(src: &str, from: usize) -> usize {
    let mut depth = 0_i32;
    for (i, ch) in src[from..].char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return from + i,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    src.len()
}

/// The source without its inline `mod tests { ... }` block (an external `mod tests;` stays).
fn non_test_part(src: &str) -> &str {
    src.find("#[cfg(test)]\nmod tests {")
        .or_else(|| src.find("#[cfg(test)]\r\nmod tests {"))
        .map_or(src, |at| &src[..at])
}

fn allowlist() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/raw_error_text_allowlist.txt");
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn ui_crates_do_not_format_raw_errors_into_visible_text() {
    let crates = workspace_crates();
    let mut files = Vec::new();
    for entry in fs::read_dir(&crates).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "tg-app" || name.starts_with("tg-app-") {
            rust_files(&entry.path().join("src"), &mut files);
        }
    }
    assert!(
        files.len() > 100,
        "found only {} UI source files",
        files.len()
    );
    let allowed = allowlist();
    let mut used = vec![false; allowed.len()];
    let mut report = String::new();
    for file in files.iter().filter(|f| !is_test_file(f)) {
        let Ok(full) = fs::read_to_string(file) else {
            continue;
        };
        let src = non_test_part(&full);
        let rel = file
            .strip_prefix(&crates)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let mut search = 0;
        while let Some(at) = src[search..].find("format!(") {
            let start = search + at;
            search = start + "format!(".len();
            let Some(literal) = format_literal(src, search) else {
                continue;
            };
            let after = search + src[search..].find(literal).unwrap_or(0) + literal.len();
            let args_end = format_args_end(src, after);
            if !has_error_placeholder(literal, &src[after..args_end]) {
                continue;
            }
            let line_start = src[..start]
                .rmatch_indices('\n')
                .nth(3)
                .map_or(0, |(i, _)| i);
            let context = &src[line_start..search];
            if !SINKS.iter().any(|sink| context.contains(sink)) {
                continue;
            }
            let key = format!("{rel}|{literal}");
            if let Some(i) = allowed.iter().position(|a| *a == key) {
                used[i] = true;
                continue;
            }
            let line = src[..start].matches('\n').count() + 1;
            let _ = writeln!(report, "  crates/{rel}:{line}: format!(\"{literal}\")");
        }
    }
    assert!(
        report.is_empty(),
        "These format a raw error into a toast, label or error line:\n{report}\n\
         A request failure must show what tdesktop shows at that call site: map it with \
         tg_app_core::user_error (add a function citing the tdesktop .fail() handler, or use \
         error_type_toast / silent) and show the UserFacingError (TGB-2957). If the error is \
         local (file, OS, parse) and never an RPC error, add the line `<path under crates/>|<format \
         string>` to crates/tg-app-core/tests/raw_error_text_allowlist.txt with a # reason."
    );
    let stale: Vec<&String> = allowed
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(a, _)| a)
        .collect();
    assert!(
        stale.is_empty(),
        "Stale entries in crates/tg-app-core/tests/raw_error_text_allowlist.txt (the format \
         string changed or is gone; remove or update them): {stale:#?}"
    );
}
