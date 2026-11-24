//! Port of `export/output/export_output_file.h` and `export/output/export_output_file.cpp`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::output::result::OutputResult;
use crate::output::stats::Stats;

/// Incremental export file writer with existing-file truncation/recovery and stats tracking
/// (port of `Export::Output::File`).
pub struct OutputFile {
    path: String,
    offset: i64,
    file: Option<File>,
    in_stats: bool,
}

impl OutputFile {
    /// Creates a new `OutputFile` handle targeting `path` (does not create the file until first write).
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            file: None,
            in_stats: false,
        }
    }

    /// Returns the number of bytes written so far.
    pub const fn size(&self) -> i64 {
        self.offset
    }

    /// Returns true if no bytes have been written yet.
    pub const fn is_empty(&self) -> bool {
        self.offset == 0
    }

    /// Returns the file's full path string.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Writes `block` to the file, flushing immediately and updating `stats`.
    ///
    /// On error, the underlying file handle is closed so a subsequent retry reopens cleanly.
    pub fn write_block(&mut self, block: &[u8], stats: Option<&Stats>) -> OutputResult {
        let result = self.write_block_attempt(block, stats);
        if result.is_error() {
            self.file = None;
        }
        result
    }

    fn write_block_attempt(&mut self, block: &[u8], stats: Option<&Stats>) -> OutputResult {
        if let Some(s) = stats
            && !self.in_stats
        {
            self.in_stats = true;
            s.increment_files();
        }
        let reopen_res = self.reopen();
        if reopen_res.is_error() {
            return reopen_res;
        }
        let size = block.len();
        if size == 0 {
            return OutputResult::success();
        }
        if let Some(ref mut file) = self.file
            && file.write_all(block).is_ok()
            && file.flush().is_ok()
        {
            self.offset += size as i64;
            if let Some(s) = stats {
                s.increment_bytes(size as i64);
            }
            return OutputResult::success();
        }
        self.error()
    }

    fn reopen(&mut self) -> OutputResult {
        if self.file.is_some() {
            return OutputResult::success();
        }
        let p = Path::new(&self.path);
        if p.exists() {
            let meta = match fs::metadata(p) {
                Ok(m) => m,
                Err(_) => return self.error(),
            };
            if (meta.len() as i64) < self.offset {
                return self.fatal_error();
            }
            match OpenOptions::new().write(true).open(p) {
                Ok(f) => {
                    if f.set_len(self.offset as u64).is_err() {
                        return self.error();
                    }
                }
                Err(_) => return self.error(),
            }
        } else if self.offset > 0 {
            return self.fatal_error();
        }

        if let Ok(f) = OpenOptions::new().create(true).append(true).open(p) {
            self.file = Some(f);
            return OutputResult::success();
        }
        if let Some(parent) = p.parent()
            && !parent.exists()
            && fs::create_dir_all(parent).is_ok()
            && let Ok(f) = OpenOptions::new().create(true).append(true).open(p)
        {
            self.file = Some(f);
            return OutputResult::success();
        }
        self.error()
    }

    fn error(&self) -> OutputResult {
        OutputResult::error(self.path.clone())
    }

    fn fatal_error(&self) -> OutputResult {
        OutputResult::fatal_error(self.path.clone())
    }

    /// Finds a non-colliding relative path under `folder` for `suggested`
    /// (port of `Export::Output::File::PrepareRelativePath`).
    ///
    /// Splits at the *first* `.` in `suggested` (matching tdesktop's `indexOf('.')`)
    /// so multi-part extensions like `file.tar.xz` become `file (1).tar.xz`.
    pub fn prepare_relative_path(folder: &str, suggested: &str) -> String {
        let full = format!("{folder}{suggested}");
        if !Path::new(&full).exists() {
            return suggested.to_string();
        }
        let (base, extension) = match suggested.find('.') {
            Some(pos) => (&suggested[..pos], &suggested[pos..]),
            None => (suggested, ""),
        };
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            let candidate = format!("{base} ({attempt}){extension}");
            let candidate_full = format!("{folder}{candidate}");
            if !Path::new(&candidate_full).exists() {
                return candidate;
            }
        }
    }

    /// Copies a file from `source` on disk to `path`, updating `stats`
    /// (port of `Export::Output::File::Copy`).
    pub fn copy(source: &str, path: &str, stats: Option<&Stats>) -> OutputResult {
        let bytes = match fs::read(source) {
            Ok(b) => b,
            Err(_) => return OutputResult::fatal_error(source),
        };
        Self::new(path).write_block(&bytes, stats)
    }

    /// Writes embedded bytes directly to `path`, updating `stats`.
    pub fn write_bytes(path: &str, bytes: &[u8], stats: Option<&Stats>) -> OutputResult {
        Self::new(path).write_block(bytes, stats)
    }
}
