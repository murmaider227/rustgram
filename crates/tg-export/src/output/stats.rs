//! Port of `export/output/export_output_stats.h` and `export/output/export_output_stats.cpp`.

use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

/// Thread-safe counters for written export files and bytes (port of `Export::Output::Stats`).
#[derive(Debug, Default)]
pub struct Stats {
    files: AtomicI32,
    bytes: AtomicI64,
}

impl Clone for Stats {
    fn clone(&self) -> Self {
        Self {
            files: AtomicI32::new(self.files.load(Ordering::Relaxed)),
            bytes: AtomicI64::new(self.bytes.load(Ordering::Relaxed)),
        }
    }
}

impl Stats {
    /// Creates zeroed counters.
    pub const fn new() -> Self {
        Self {
            files: AtomicI32::new(0),
            bytes: AtomicI64::new(0),
        }
    }

    /// Increments the exported files count by 1.
    pub fn increment_files(&self) {
        self.files.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `count` bytes to the exported bytes total.
    pub fn increment_bytes(&self, count: i64) {
        self.bytes.fetch_add(count, Ordering::Relaxed);
    }

    /// Returns the total number of exported files.
    pub fn files_count(&self) -> i32 {
        self.files.load(Ordering::Relaxed)
    }

    /// Returns the total number of written bytes.
    pub fn bytes_count(&self) -> i64 {
        self.bytes.load(Ordering::Relaxed)
    }
}
