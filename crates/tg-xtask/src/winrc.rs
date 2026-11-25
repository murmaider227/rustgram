//! Windows `VERSIONINFO` resource script generation and release version metadata.
//!
//! Defines canonical Win32 version resource fields shared across all three Rustgram
//! Windows executables (`rustgram.exe`, `rustgram-updater.exe`, `rustgram-widget-helper.exe`)
//! and generates standard `.rc` resource scripts checked by `tg-xtask` tests.

use std::env;
use std::path::Path;

use tg_updater::version::Version;
use tg_updater::{DEFAULT_EXE_NAME, UPDATER_EXE_NAME};

use crate::XtaskError;
use crate::dist::{WIDGET_HELPER_EXE_NAME, read_workspace_version};

/// Windows `VERSIONINFO` `ProductName` shared by all Rustgram executables and the installer.
pub const PRODUCT_NAME: &str = "Rustgram";

/// Windows `VERSIONINFO` `CompanyName` shared by all Rustgram executables and the installer.
pub const COMPANY_NAME: &str = "Rustgram Authors";

/// Windows `VERSIONINFO` `LegalCopyright` string.
pub const LEGAL_COPYRIGHT: &str = "Copyright (c) 2026 Rustgram Authors (GPL-3.0)";

/// Per-executable Windows `VERSIONINFO` metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryVersionResource {
    /// Executable filename (`OriginalFilename`), e.g. `"rustgram.exe"`.
    pub exe_name: &'static str,
    /// Cargo binary target / `InternalName`, e.g. `"rustgram"`.
    pub internal_name: &'static str,
    /// Human-readable `FileDescription`, e.g. `"Rustgram Desktop"`.
    pub file_description: &'static str,
    /// Relative path to the crate's `build.rs` that embeds this resource.
    pub build_script_rel_path: &'static str,
    /// Whether this executable also embeds the primary application icon (`1 ICON`).
    pub has_icon: bool,
}

/// Canonical Windows version resource specifications for every shipped Rustgram executable.
pub const DIST_BINARY_RESOURCES: &[BinaryVersionResource] = &[
    BinaryVersionResource {
        exe_name: DEFAULT_EXE_NAME,
        internal_name: "rustgram",
        file_description: "Rustgram Desktop",
        build_script_rel_path: "crates/tg-app/build.rs",
        has_icon: true,
    },
    BinaryVersionResource {
        exe_name: UPDATER_EXE_NAME,
        internal_name: "rustgram-updater",
        file_description: "Rustgram Updater",
        build_script_rel_path: "crates/tg-updater/build.rs",
        has_icon: false,
    },
    BinaryVersionResource {
        exe_name: WIDGET_HELPER_EXE_NAME,
        internal_name: "rustgram-widget-helper",
        file_description: "Rustgram Widget Helper",
        build_script_rel_path: "crates/tg-platform/build.rs",
        has_icon: false,
    },
];

/// Resolves the release version from an explicit override, `RUSTGRAM_VERSION` (or legacy
/// `TGFORK_VERSION`), or the workspace `Cargo.toml` version.
pub fn resolve_release_version(
    workspace_root: &Path,
    override_version: Option<&Version>,
) -> Result<Version, XtaskError> {
    if let Some(v) = override_version {
        return Ok(v.clone());
    }
    for var in ["RUSTGRAM_VERSION", "TGFORK_VERSION"] {
        if let Ok(raw) = env::var(var) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Ok(Version::parse(trimmed)?);
            }
        }
    }
    read_workspace_version(workspace_root)
}

/// Escapes backslashes and double quotes for Win32 `.rc` string literals.
fn escape_rc_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\"\"")
}

/// Generates a standard Win32 `.rc` resource script containing `VS_VERSION_INFO` (and optional
/// `1 ICON`) for the given binary metadata and release [`Version`].
#[must_use]
pub fn generate_version_rc(
    spec: &BinaryVersionResource,
    version: &Version,
    icon_path: Option<&str>,
) -> String {
    let version_str = version.to_string();
    let major = u16::try_from(version.major).unwrap_or(u16::MAX);
    let minor = u16::try_from(version.minor).unwrap_or(u16::MAX);
    let patch = u16::try_from(version.patch).unwrap_or(u16::MAX);
    let quad_commas = format!("{major},{minor},{patch},0");

    let mut out = String::new();
    out.push_str("// Generated Windows resource script for ");
    out.push_str(spec.exe_name);
    out.push('\n');

    if spec.has_icon
        && let Some(icon) = icon_path
    {
        out.push_str(&format!("1 ICON \"{}\"\n\n", escape_rc_string(icon)));
    }

    out.push_str(&format!(
        "\
1 VERSIONINFO
FILEVERSION {quad_commas}
PRODUCTVERSION {quad_commas}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK \"StringFileInfo\"
    BEGIN
        BLOCK \"040904b0\"
        BEGIN
            VALUE \"CompanyName\", \"{company}\"
            VALUE \"FileDescription\", \"{desc}\"
            VALUE \"FileVersion\", \"{ver}\"
            VALUE \"InternalName\", \"{internal}\"
            VALUE \"LegalCopyright\", \"{copyright}\"
            VALUE \"OriginalFilename\", \"{orig}\"
            VALUE \"ProductName\", \"{product}\"
            VALUE \"ProductVersion\", \"{ver}\"
        END
    END
    BLOCK \"VarFileInfo\"
    BEGIN
        VALUE \"Translation\", 0x0409, 1200
    END
END
",
        company = escape_rc_string(COMPANY_NAME),
        desc = escape_rc_string(spec.file_description),
        ver = escape_rc_string(&version_str),
        internal = escape_rc_string(spec.internal_name),
        copyright = escape_rc_string(LEGAL_COPYRIGHT),
        orig = escape_rc_string(spec.exe_name),
        product = escape_rc_string(PRODUCT_NAME),
    ));

    out
}
