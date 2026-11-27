//! Single-line text elision; the helper lives in [`tg_ui::elide`] (TGB-3066) so the media
//! viewer, calls and export screens share it.

pub use tg_ui::elide::{elide_end, elide_end_by};
