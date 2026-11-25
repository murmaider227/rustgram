//! Safe remote filename sanitization, directory containment, and non-overwriting file creation.
//!
//! Ports tdesktop's `base::FileNameFromUserString` (`lib_base/base/base_file_utilities.cpp`),
//! `base::Platform::FileNameFromUserString` (`lib_base/base/platform/win/base_file_utilities_win.cpp`),
//! and unique filename allocation (`filedialogNextFilename` in `core/file_utilities.cpp:113`),
//! hardened against path traversal, drive-letter overrides, and overwrites.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::error::{PlatformError, Result};

/// Windows reserved DOS device names (bare or followed by `.ext`).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Fallback filename used when a remote name reduces to empty, `.`, or `..`.
const FALLBACK_FILENAME: &str = "file";

/// Maximum character length kept for a single sanitized filename component.
const MAX_FILENAME_CHARS: usize = 200;

/// Maximum collision retries when generating unique filenames (`name (1).ext`, ...).
const MAX_UNIQUE_ATTEMPTS: usize = 100_000;

/// Returns `true` if `ch` is a Unicode BiDi control character that can visually spoof file
/// extensions (`IsBidiControl` in `base_file_utilities.cpp:16`).
#[must_use]
pub const fn is_bidi_control(ch: char) -> bool {
    let code = ch as u32;
    code == 0x061C
        || code == 0x200E
        || code == 0x200F
        || (code >= 0x202A && code <= 0x202E)
        || (code >= 0x2066 && code <= 0x2069)
}

/// Sanitizes an untrusted remote/user filename into a single safe filename component for Windows
/// and cross-platform filesystems.
///
/// Guarantees:
/// 1. Reduces any path (`..\..\Startup\x.bat`, `C:\Windows\x.dll`, `/etc/passwd`, `\\host\share\a`)
///    to its last path component, stripping drive prefixes (`C:`).
/// 2. Strips forbidden characters (`\ / : * ? " < > |`), ASCII control characters (`< 0x20`,
///    `0x7F`), and Unicode BiDi override/isolate marks.
/// 3. Trims leading spaces and trailing dots/spaces (before checking dangerous extensions).
/// 4. Falls back to `"file"` if the result is empty or consists only of dots (`.`, `..`).
/// 5. Masks `.lnk` and `.scf` with `.download` (case-insensitive).
/// 6. Prefixes reserved DOS device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`)
///    with `_`.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    // 1. Strip trailing path separators and reduce to the last path component across both `/` and `\`.
    let trimmed_slashes = name.trim_end_matches(['/', '\\']);
    let last_segment = trimmed_slashes.rsplit(['/', '\\']).next().unwrap_or("");

    // Strip a leading Windows drive prefix (`C:evil.exe` -> `evil.exe`).
    let without_drive = {
        let b = last_segment.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            &last_segment[2..]
        } else {
            last_segment
        }
    };

    // 2. Strip forbidden characters, control chars, and BiDi spoofing characters.
    let mut cleaned = String::with_capacity(without_drive.len() + 16);
    for ch in without_drive.chars() {
        let forbidden = (ch as u32) < 0x20
            || ch == '\x7F'
            || is_bidi_control(ch)
            || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if !forbidden {
            cleaned.push(ch);
        }
    }

    // 3. Trim leading spaces and trailing dots/spaces.
    let trimmed = cleaned.trim_start_matches(' ').trim_end_matches(['.', ' ']);

    // 4. Reject empty or dots-only names (`.`, `..`, `...`).
    let mut out = if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        FALLBACK_FILENAME.to_owned()
    } else {
        truncate_preserving_extension(trimmed, MAX_FILENAME_CHARS)
    };

    // Re-trim trailing dots/spaces after any truncation.
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() || out.chars().all(|c| c == '.') {
        out = FALLBACK_FILENAME.to_owned();
    }

    // 5. Mask `.lnk` and `.scf` (case-insensitive) with `.download`.
    let lower = out.to_ascii_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".scf") {
        out.push_str(".download");
    }

    // 6. Prefix Windows reserved DOS device names (`CON`, `NUL.txt`, `COM1 .log`, etc.) with `_`.
    let stem_before_dot = out.split('.').next().unwrap_or("").trim_end_matches(' ');
    let stem_upper = stem_before_dot.to_ascii_uppercase();
    if WINDOWS_RESERVED_NAMES.contains(&stem_upper.as_str()) {
        out.insert(0, '_');
    }

    out
}

fn truncate_preserving_extension(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_owned();
    }
    let (stem, ext) = split_stem_and_ext(name);
    let ext_chars = ext.chars().count();
    if !ext.is_empty() && ext_chars < max_chars / 2 {
        let keep_stem = max_chars - ext_chars;
        let truncated_stem: String = stem.chars().take(keep_stem).collect();
        format!("{truncated_stem}{ext}")
    } else {
        name.chars().take(max_chars).collect()
    }
}

/// Splits a sanitized filename into `(stem, ext)` where `ext` includes the leading `'.'`.
///
/// Leading dotfiles like `".gitignore"` keep the entire string as `stem` with empty `ext`.
#[must_use]
pub fn split_stem_and_ext(file_name: &str) -> (&str, &str) {
    if let Some(dot) = file_name.rfind('.')
        && dot > 0
        && dot + 1 < file_name.len()
    {
        (&file_name[..dot], &file_name[dot..])
    } else {
        (file_name, "")
    }
}

/// Returns `true` if `path` contains any `..` (`Component::ParentDir`) segment.
#[must_use]
pub fn path_has_parent_traversal(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn ensure_single_normal_component(name: &str) -> Result<()> {
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) if !name.contains('/') && !name.contains('\\') => Ok(()),
        _ => Err(PlatformError::InvalidArgument(format!(
            "unsafe filename component: '{name}'"
        ))),
    }
}

fn canonicalize_base_dir(base_dir: &Path) -> Result<PathBuf> {
    if path_has_parent_traversal(base_dir) {
        return Err(PlatformError::InvalidArgument(format!(
            "refusing save directory containing '..': '{}'",
            base_dir.display()
        )));
    }
    std::fs::create_dir_all(base_dir)?;
    let canon = std::fs::canonicalize(base_dir)?;
    if !canon.is_dir() {
        return Err(PlatformError::InvalidArgument(format!(
            "save target is not a directory: '{}'",
            canon.display()
        )));
    }
    Ok(canon)
}

/// Resolves an untrusted remote filename inside `base_dir`, guaranteeing the resulting path stays
/// strictly inside `base_dir` (never escaping via `..`, drive prefixes, or absolute paths).
pub fn resolve_safe_path_in_dir(base_dir: &Path, untrusted_name: &str) -> Result<PathBuf> {
    let canon_base = canonicalize_base_dir(base_dir)?;
    let safe_name = sanitize_filename(untrusted_name);
    ensure_single_normal_component(&safe_name)?;
    let candidate = canon_base.join(&safe_name);
    if candidate.parent() != Some(canon_base.as_path()) || !candidate.starts_with(&canon_base) {
        return Err(PlatformError::InvalidArgument(format!(
            "resolved path '{}' escapes target directory '{}'",
            candidate.display(),
            canon_base.display()
        )));
    }
    Ok(candidate)
}

/// Computes a non-existing unique path inside `base_dir` (`"name.ext"`, `"name (1).ext"`, ...)
/// for an untrusted remote filename, guaranteeing containment inside `base_dir`.
pub fn unique_path_in_dir(base_dir: &Path, untrusted_name: &str) -> Result<PathBuf> {
    let canon_base = canonicalize_base_dir(base_dir)?;
    let safe_name = sanitize_filename(untrusted_name);
    ensure_single_normal_component(&safe_name)?;
    let (stem, ext) = split_stem_and_ext(&safe_name);

    for attempt in 0..=MAX_UNIQUE_ATTEMPTS {
        let name = if attempt == 0 {
            safe_name.clone()
        } else {
            format!("{stem} ({attempt}){ext}")
        };
        ensure_single_normal_component(&name)?;
        let candidate = canon_base.join(&name);
        if candidate.parent() != Some(canon_base.as_path()) {
            return Err(PlatformError::InvalidArgument(
                "candidate path escaped target directory".into(),
            ));
        }
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }

    Err(PlatformError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("exhausted unique filename attempts for '{safe_name}'"),
    )))
}

/// Atomically creates and writes a new file inside `base_dir` under a sanitized, unique filename
/// (`"name.ext"`, `"name (1).ext"`, ...).
///
/// Uses `OpenOptions::create_new(true)` so an existing file can never be overwritten even under
/// concurrent saves. Returns the final path written inside `base_dir`.
pub fn write_unique_file_in_dir(
    base_dir: &Path,
    untrusted_name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let canon_base = canonicalize_base_dir(base_dir)?;
    let safe_name = sanitize_filename(untrusted_name);
    ensure_single_normal_component(&safe_name)?;
    let (stem, ext) = split_stem_and_ext(&safe_name);

    for attempt in 0..=MAX_UNIQUE_ATTEMPTS {
        let name = if attempt == 0 {
            safe_name.clone()
        } else {
            format!("{stem} ({attempt}){ext}")
        };
        ensure_single_normal_component(&name)?;
        let candidate = canon_base.join(&name);
        if candidate.parent() != Some(canon_base.as_path()) || !candidate.starts_with(&canon_base) {
            return Err(PlatformError::InvalidArgument(
                "candidate path escaped target directory".into(),
            ));
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.flush()?;
                return Ok(candidate);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(PlatformError::Io(err)),
        }
    }

    Err(PlatformError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("exhausted unique filename attempts for '{safe_name}'"),
    )))
}

/// Writes `bytes` to a unique non-overwriting file inside `destination.parent()`, sanitizing
/// `destination.file_name()` and refusing any `..` traversal in `destination`.
pub fn write_unique_file_at_destination(destination: &Path, bytes: &[u8]) -> Result<PathBuf> {
    if path_has_parent_traversal(destination) {
        return Err(PlatformError::InvalidArgument(format!(
            "refusing destination containing path traversal '..': '{}'",
            destination.display()
        )));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            PlatformError::InvalidArgument(format!(
                "destination must include a parent directory: '{}'",
                destination.display()
            ))
        })?;
    let raw_name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            PlatformError::InvalidArgument(format!(
                "destination is missing a valid filename: '{}'",
                destination.display()
            ))
        })?;
    write_unique_file_in_dir(parent, raw_name, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_traversal_drives_forbidden_chars_and_trailing_dots() {
        assert_eq!(sanitize_filename(r"..\..\Startup\x.bat"), "x.bat");
        assert_eq!(sanitize_filename(r"C:\Windows\x.dll"), "x.dll");
        assert_eq!(sanitize_filename("C:x.dll"), "x.dll");
        assert_eq!(
            sanitize_filename(r"\\host\share\payload.exe"),
            "payload.exe"
        );
        assert_eq!(sanitize_filename("/etc/passwd"), "passwd");
        assert_eq!(sanitize_filename(".."), "file");
        assert_eq!(sanitize_filename("."), "file");
        assert_eq!(sanitize_filename("..."), "file");
        assert_eq!(sanitize_filename("   "), "file");
        assert_eq!(sanitize_filename("report.pdf. . "), "report.pdf");
        assert_eq!(sanitize_filename("evil.lnk. "), "evil.lnk.download");
        assert_eq!(sanitize_filename("evil.SCF."), "evil.SCF.download");
        assert_eq!(sanitize_filename("ab:c*d?e\"f<g>h|i.txt"), "abcdefghi.txt");
        assert_eq!(
            sanitize_filename("name\x00\x1f\u{202E}txt.exe"),
            "nametxt.exe"
        );
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_filename("com1 .log"), "_com1 .log");
    }

    #[test]
    fn write_unique_never_overwrites_and_stays_inside_dir() {
        let base =
            std::env::temp_dir().join(format!("rustgram_file_safety_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let canon_base = std::fs::canonicalize(&base).unwrap();

        let p1 = write_unique_file_in_dir(&base, r"..\..\Startup\x.bat", b"first").unwrap();
        assert_eq!(p1.parent(), Some(canon_base.as_path()));
        assert_eq!(p1.file_name().and_then(|s| s.to_str()), Some("x.bat"));

        let p2 = write_unique_file_in_dir(&base, r"C:\Windows\x.bat", b"second").unwrap();
        assert_eq!(p2.parent(), Some(canon_base.as_path()));
        assert_eq!(p2.file_name().and_then(|s| s.to_str()), Some("x (1).bat"));

        assert_eq!(std::fs::read(&p1).unwrap(), b"first");
        assert_eq!(std::fs::read(&p2).unwrap(), b"second");

        // Direct destination with `..` must be refused.
        assert!(write_unique_file_at_destination(&base.join(r"..\escape.txt"), b"x").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
