//! Workspace-stable paths and embedded assets shared across `tg-app-*` crates.

use std::path::{Path, PathBuf};

/// Embedded Rustgram SVG application icon (`assets/icon.svg`).
pub const APP_ICON_SVG: &[u8] = include_bytes!("../../../../assets/icon.svg");

/// Resolves `crates/tg-app/docs/` regardless of which `crates/tg-app-*` crate invokes it.
#[must_use]
pub fn workspace_app_docs_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sibling = manifest_dir.join("../tg-app/docs");
    if manifest_dir.file_name().and_then(|n| n.to_str()) == Some("tg-app") {
        manifest_dir.join("docs")
    } else {
        sibling
    }
}
