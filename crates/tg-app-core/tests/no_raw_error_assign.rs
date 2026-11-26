//! Lint (TGB-2977): no toast, label or error line in the UI crates takes a raw error string
//! directly (`toast = Some(err)`, `self.error = Some(err.clone())`, `show_toast(err)`).
//!
//! `no_raw_error_text.rs` catches `format!` calls; this test catches the direct assignments. A
//! backend error is a string such as `RPC error 400: CHANNELS_TOO_MUCH`, and tdesktop never shows
//! it: map a request failure with `tg_app_core::user_error` (a function citing the tdesktop
//! `.fail()` handler) and show its `UserFacingError`.
//!
//! A local error (file, OS, parse, a check done before any request) may stay: mark the line with
//! a trailing `// local error: <reason>` comment. Files that still have unreviewed sites are
//! listed with their count in `tests/raw_error_assign_baseline.txt`; the count may only go down.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const ERROR_VARS: [&str; 4] = ["err", "e", "error", "why"];
const SUFFIXES: [&str; 5] = ["", ".clone()", ".to_string()", ".to_owned()", ".into()"];
/// Words in the assigned place that make it visible text.
const SINK_WORDS: [&str; 5] = ["error", "toast", "notice", "status", "message"];
const LOCAL_MARK: &str = "// local error:";

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
    name.contains("test") || name.starts_with("mock") || name.contains("shots")
}

/// The source without its inline `mod tests { ... }` block (an external `mod tests;` stays).
fn non_test_part(src: &str) -> &str {
    src.find("#[cfg(test)]\nmod tests {")
        .or_else(|| src.find("#[cfg(test)]\r\nmod tests {"))
        .map_or(src, |at| &src[..at])
}

/// `true` when `line` puts an error variable as-is into a visible-text place.
fn is_raw_assignment(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or_default();
    for var in ERROR_VARS {
        for suffix in SUFFIXES {
            let wrapped = format!("Some({var}{suffix})");
            if let Some(at) = code.find(&wrapped) {
                let before = &code[..at];
                let Some(eq) = before.rfind('=') else {
                    continue;
                };
                if before[eq..].trim_start_matches('=').trim().is_empty()
                    && !before[..eq].ends_with(['=', '!', '<', '>'])
                {
                    let place = before[..eq].to_ascii_lowercase();
                    let place = place.rsplit("=>").next().unwrap_or_default();
                    if SINK_WORDS.iter().any(|w| place.contains(w)) {
                        return true;
                    }
                }
            }
            if code.contains(&format!("show_toast({var}{suffix})"))
                || code.contains(&format!("show_toast({var}{suffix},"))
            {
                return true;
            }
        }
    }
    false
}

/// `path under crates/` → number of raw assignments.
fn scan() -> BTreeMap<String, usize> {
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
    let mut found = BTreeMap::new();
    for file in files.iter().filter(|f| !is_test_file(f)) {
        let Ok(full) = fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(&crates)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let count = non_test_part(&full)
            .lines()
            .filter(|l| !l.contains(LOCAL_MARK) && is_raw_assignment(l))
            .count();
        if count > 0 {
            found.insert(rel, count);
        }
    }
    found
}

fn baseline() -> BTreeMap<String, usize> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/raw_error_assign_baseline.txt");
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (path, count) = l.rsplit_once(' ')?;
            Some((path.trim().to_owned(), count.parse().ok()?))
        })
        .collect()
}

#[test]
fn detector_matches_the_raw_shapes_only() {
    for raw in [
        "Err(e) => toast = Some(e),",
        "self.error = Some(err.clone());",
        "Err(err) => modal.error = Some(err),",
        "*toast = Some(err);",
        "self.controller.show_toast(err);",
        "self.show_toast(error, now_secs);",
        "state.proxy_form.error = Some(e.to_string()),",
        "self.status_notice = Some(err.clone());",
    ] {
        assert!(is_raw_assignment(raw), "{raw}");
    }
    for mapped in [
        "Err(err) => toast = user_error::send_message(&err, target).into_text(),",
        "self.error = Some(tr::lng_attach_failed());",
        "self.error = Some(err.clone()); // local error: file read",
        "let peer = Some(e);",
        "if self.error == Some(err) {",
        "self.cache = Some(err.clone());",
    ] {
        let line = mapped.split(LOCAL_MARK).next().unwrap_or_default();
        assert!(
            !is_raw_assignment(line) || mapped.contains(LOCAL_MARK),
            "{mapped}"
        );
    }
}

#[test]
fn ui_crates_do_not_assign_raw_errors_to_visible_text() {
    let found = scan();
    let base = baseline();
    let mut report = String::new();
    for (path, &count) in &found {
        let allowed = base.get(path).copied().unwrap_or(0);
        if count > allowed {
            let _ = writeln!(
                report,
                "  crates/{path}: {count} raw assignments (baseline {allowed})"
            );
        }
    }
    assert!(
        report.is_empty(),
        "New raw error strings reach a toast, label or error line:\n{report}\n\
         A request failure must show what tdesktop shows at that call site: map it with \
         tg_app_core::user_error (a function citing the tdesktop .fail() handler, or \
         error_type_toast / silent) and show the UserFacingError (TGB-2977). A local error (file, \
         OS, parse, a check before any request) may stay with a trailing `{LOCAL_MARK} <reason>` \
         comment on the line."
    );
    let mut stale = String::new();
    for (path, &allowed) in &base {
        let count = found.get(path).copied().unwrap_or(0);
        if count < allowed {
            let _ = writeln!(stale, "  {path} {count} (was {allowed})");
        }
    }
    assert!(
        stale.is_empty(),
        "Fewer raw assignments than crates/tg-app-core/tests/raw_error_assign_baseline.txt \
         allows; lower these counts (delete a line at 0):\n{stale}"
    );
}
