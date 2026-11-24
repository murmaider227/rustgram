//! Source-compatibility re-exports of the real `tg-theme` palette and `tg-style` adapters.
//!
//! Previously this module held temporary stand-ins while `tg-theme` and `tg-style` were built
//! in parallel. All styles and palette lookups are now backed directly by [`crate::style`]
//! (`tg-style`) and [`crate::theme`] (`tg-theme`). New code should import from [`crate::style`]
//! and [`crate::theme`] directly.

pub mod colors;
pub mod style;

pub use colors::{Colors, DayPalette, parse_hex_color};
pub use style::*;
