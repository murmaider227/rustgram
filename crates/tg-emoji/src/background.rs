//! Process-wide switch for decoding images off the paint path.
//!
//! Off by default, so tests and headless screenshot harnesses decode synchronously and render
//! the same pixels on their first frame. The desktop binary turns it on at startup: emoji sprite
//! sheets (and, through the same switch, chat media in `tg-app-history`) are then decoded on
//! worker threads and only uploaded on the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};

static BACKGROUND_DECODE: AtomicBool = AtomicBool::new(false);

/// Turns background decoding on or off for the whole process.
pub fn set_background_decode(enabled: bool) {
    BACKGROUND_DECODE.store(enabled, Ordering::Relaxed);
}

/// Returns `true` when images should be decoded on worker threads.
#[must_use]
pub fn background_decode_enabled() -> bool {
    BACKGROUND_DECODE.load(Ordering::Relaxed)
}
