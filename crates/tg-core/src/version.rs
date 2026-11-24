//! Port of `core/version.h`.
//!
//! Application identity and version constants for Rustgram.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Stable application identifier used for single-instance IPC and platform registration.
pub const APP_ID: &str = "{8B4E2D1A-7C91-4E3F-9A62-3D5C1E8F4B20}";

/// Human-readable application name.
pub const APP_NAME: &str = "Rustgram";

/// Default executable stem name (without `.exe`).
pub const APP_FILE: &str = "Rustgram";

/// Portable mode marker folder name next to the executable (Rustgram equivalent of `TelegramForcePortable`).
pub const PORTABLE_FOLDER_NAME: &str = "RustgramForcePortable";

/// Semantic version string for Rustgram.
pub const APP_VERSION_STR: &str = "0.1.0";

/// Numeric version encoded with tdesktop's `major * 1_000_000 + minor * 1_000 + patch` scheme.
pub const APP_VERSION: u32 = 1_000;

/// Whether this build is a beta release.
pub const APP_BETA_VERSION: bool = false;

/// Closed-alpha version number (0 for non-alpha builds).
pub const APP_ALPHA_VERSION: u64 = 0;

/// Reference Telegram Desktop version (7.4.5) whose `Core::Settings` binary layout we match.
pub const TDESKTOP_COMPAT_VERSION: i32 = 7_004_005;

/// Errors returned when parsing a version string.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VersionParseError {
    /// The string did not contain valid dot-separated components.
    #[error("invalid version string: {0}")]
    InvalidFormat(String),
    /// A numeric component overflowed the allowed range (`0..=999` for minor/patch).
    #[error("version component out of range: {0}")]
    OutOfRange(String),
}

/// Structured application version supporting tdesktop's numeric encoding (`MMmmmppp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppVersion {
    /// Major version component.
    pub major: u32,
    /// Minor version component (`0..=999`).
    pub minor: u32,
    /// Patch version component (`0..=999`).
    pub patch: u32,
}

impl AppVersion {
    /// Current compiled Rustgram version (`0.1.0`).
    pub const CURRENT: Self = Self {
        major: 0,
        minor: 1,
        patch: 0,
    };

    /// Creates a new version tuple.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Encodes as tdesktop's integer version: `major * 1_000_000 + minor * 1_000 + patch`.
    #[must_use]
    pub const fn to_numeric(self) -> u32 {
        self.major * 1_000_000 + self.minor * 1_000 + self.patch
    }

    /// Decodes from tdesktop's integer version encoding.
    #[must_use]
    pub const fn from_numeric(value: u32) -> Self {
        Self {
            major: value / 1_000_000,
            minor: (value / 1_000) % 1_000,
            patch: value % 1_000,
        }
    }
}

impl fmt::Display for AppVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for AppVersion {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim().strip_prefix('v').unwrap_or(s.trim());
        // Strip optional pre-release / build metadata (e.g. "0.2.0-beta.1").
        let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(VersionParseError::InvalidFormat(s.to_owned()));
        }
        let major: u32 = parts[0]
            .parse()
            .map_err(|_| VersionParseError::InvalidFormat(s.to_owned()))?;
        let minor: u32 = parts[1]
            .parse()
            .map_err(|_| VersionParseError::InvalidFormat(s.to_owned()))?;
        let patch: u32 = if parts.len() == 3 {
            parts[2]
                .parse()
                .map_err(|_| VersionParseError::InvalidFormat(s.to_owned()))?
        } else {
            0
        };
        if minor >= 1_000 || patch >= 1_000 || major > 4_000 {
            return Err(VersionParseError::OutOfRange(s.to_owned()));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_matches_constants() {
        assert_eq!(AppVersion::CURRENT.to_numeric(), APP_VERSION);
        assert_eq!(AppVersion::CURRENT.to_string(), APP_VERSION_STR);
        assert_eq!(AppVersion::from_numeric(APP_VERSION), AppVersion::CURRENT);
    }

    #[test]
    fn tdesktop_numeric_round_trip() {
        let v = AppVersion::from_numeric(7_004_005);
        assert_eq!(v, AppVersion::new(7, 4, 5));
        assert_eq!(v.to_numeric(), 7_004_005);
        assert_eq!(v.to_string(), "7.4.5");
    }

    #[test]
    fn parse_version_strings() {
        assert_eq!(
            "0.1.0".parse::<AppVersion>().unwrap(),
            AppVersion::new(0, 1, 0)
        );
        assert_eq!(
            "v7.4.5".parse::<AppVersion>().unwrap(),
            AppVersion::new(7, 4, 5)
        );
        assert_eq!(
            "1.2".parse::<AppVersion>().unwrap(),
            AppVersion::new(1, 2, 0)
        );
        assert_eq!(
            "v1.2.3-beta.1".parse::<AppVersion>().unwrap(),
            AppVersion::new(1, 2, 3)
        );
        assert!("invalid".parse::<AppVersion>().is_err());
        assert!("1.1000.0".parse::<AppVersion>().is_err());
    }
}
