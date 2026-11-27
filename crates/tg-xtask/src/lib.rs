//! Rustgram release and packaging automation (`cargo run -p tg-xtask -- <command>`).
//!
//! Provides pure-Rust icon rendering (`assets/icon.svg` -> multi-size `.ico`),
//! portable ZIP packaging, Ed25519-signed `.tupdate` generation, optional Inno Setup
//! (`ISCC.exe`) installer compilation, SHA-256 manifest output, and signing keygen.

pub mod dist;
pub mod icon;
pub mod update_path;
pub mod winrc;

use thiserror::Error;

/// Errors returned by `tg-xtask` operations.
#[derive(Debug, Error)]
pub enum XtaskError {
    /// Failed to locate the Rustgram workspace root.
    #[error("could not locate workspace root containing Cargo.toml and assets/icon.svg")]
    WorkspaceNotFound,

    /// Invalid command-line arguments or configuration.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// SVG parsing or rasterization failed.
    #[error("SVG rendering failed: {0}")]
    SvgRender(String),

    /// ICO encoding failed.
    #[error("ICO encoding failed: {0}")]
    IcoEncode(String),

    /// External tool (`cargo` or `ISCC.exe`) returned a non-zero status.
    #[error("external command '{program}' failed with status {status}")]
    CommandFailed {
        /// Program name.
        program: String,
        /// Exit status description.
        status: String,
    },

    /// Required `ISCC.exe` compiler was not found (`--require-iscc`).
    #[error("Inno Setup compiler (ISCC.exe) not found")]
    IsccNotFound,

    /// Expected build artifact was missing on disk.
    #[error("missing build artifact: {0}")]
    MissingArtifact(String),

    /// The release files are inconsistent: the update package would install a copy that differs
    /// from the portable folder, or a staged file is listed as obsolete.
    #[error("release files are inconsistent: {0}")]
    InconsistentRelease(String),

    /// ZIP archive creation error.
    #[error("ZIP error: {0}")]
    Zip(String),

    /// Signing key could not be decoded.
    #[error("invalid Ed25519 signing key: {0}")]
    InvalidSigningKey(String),

    /// Propagated `tg-updater` error.
    #[error(transparent)]
    Updater(#[from] tg_updater::UpdateError),

    /// Filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
