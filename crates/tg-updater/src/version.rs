//! Semantic versioning, update channels, and platform targets.
//!
//! Port of `Telegram/SourceFiles/core/version.h`,
//! `Telegram/SourceFiles/core/update_channel.h`, and target/channel policy from
//! `Telegram/SourceFiles/core/update_verify.cpp`.

use std::cmp::Ordering;
use std::fmt;

use crate::UpdateError;

/// Update release channel, mirroring `Core::Updates::Channel` in tdesktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UpdateChannel {
    /// Production releases only.
    Stable = 0,
    /// Includes prerelease / beta builds as well as newer stable builds.
    Beta = 1,
}

impl UpdateChannel {
    /// Encodes the channel as its wire byte in the `TFUP` envelope.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decodes a wire byte into an [`UpdateChannel`].
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Stable),
            1 => Some(Self::Beta),
            _ => None,
        }
    }

    /// Canonical lowercase name (`"stable"` or `"beta"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Target operating system encoded inside a signed update envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PlatformOs {
    /// Microsoft Windows.
    Windows = 0,
    /// Apple macOS.
    Mac = 1,
    /// Linux.
    Linux = 2,
}

impl PlatformOs {
    /// Decodes a wire byte into a [`PlatformOs`].
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Windows),
            1 => Some(Self::Mac),
            2 => Some(Self::Linux),
            _ => None,
        }
    }

    /// Short asset identifier matching release filenames (`win`, `mac`, `linux`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "win",
            Self::Mac => "mac",
            Self::Linux => "linux",
        }
    }
}

/// Target CPU architecture encoded inside a signed update envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PlatformArch {
    /// 32-bit x86.
    X86 = 0,
    /// 64-bit x86_64.
    X64 = 1,
    /// 64-bit ARM64 / AArch64.
    Arm64 = 2,
}

impl PlatformArch {
    /// Decodes a wire byte into a [`PlatformArch`].
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::X86),
            1 => Some(Self::X64),
            2 => Some(Self::Arm64),
            _ => None,
        }
    }

    /// Short asset identifier matching release filenames (`x86`, `x64`, `arm64`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

/// Platform target bound into the signed update header to prevent cross-platform substitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Target {
    /// Operating system.
    pub os: PlatformOs,
    /// CPU architecture.
    pub arch: PlatformArch,
}

impl Target {
    /// Windows x64 target (primary Rustgram target).
    pub const WIN_X64: Self = Self {
        os: PlatformOs::Windows,
        arch: PlatformArch::X64,
    };

    /// Returns the compile-time target for the running binary.
    #[must_use]
    pub const fn current() -> Self {
        let os = if cfg!(target_os = "windows") {
            PlatformOs::Windows
        } else if cfg!(target_os = "macos") {
            PlatformOs::Mac
        } else {
            PlatformOs::Linux
        };
        let arch = if cfg!(target_arch = "x86") {
            PlatformArch::X86
        } else if cfg!(target_arch = "aarch64") {
            PlatformArch::Arm64
        } else {
            PlatformArch::X64
        };
        Self { os, arch }
    }

    /// Returns the filename suffix used for this target, e.g. `"win-x64"`.
    #[must_use]
    pub fn asset_suffix(self) -> String {
        format!("{}-{}", self.os.as_str(), self.arch.as_str())
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os.as_str(), self.arch.as_str())
    }
}

/// One dot-separated identifier inside a SemVer prerelease tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PrereleaseIdent {
    /// Numeric identifier (compared numerically: `2 < 10`).
    Numeric(u64),
    /// Alphanumeric identifier (compared ASCII-lexicographically).
    AlphaNumeric(String),
}

impl Ord for PrereleaseIdent {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(a), Self::Numeric(b)) => a.cmp(b),
            (Self::AlphaNumeric(a), Self::AlphaNumeric(b)) => a.cmp(b),
            // SemVer 2.0.0 §11.4.3: numeric identifiers always have lower precedence than non-numeric.
            (Self::Numeric(_), Self::AlphaNumeric(_)) => Ordering::Less,
            (Self::AlphaNumeric(_), Self::Numeric(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PrereleaseIdent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PrereleaseIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numeric(n) => write!(f, "{n}"),
            Self::AlphaNumeric(s) => f.write_str(s),
        }
    }
}

/// Semantic version (`major.minor.patch[-prerelease]`) with SemVer 2.0.0 ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    /// Major version component.
    pub major: u32,
    /// Minor version component.
    pub minor: u32,
    /// Patch version component.
    pub patch: u32,
    /// Optional dot-separated prerelease identifiers (empty for stable releases).
    pub prerelease: Vec<PrereleaseIdent>,
}

impl Version {
    /// Creates a stable version `major.minor.patch`.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: Vec::new(),
        }
    }

    /// Parses a version string such as `"0.1.0"`, `"v1.2.3"`, `"v1.2.3-beta.1"`, or `"1.2.3+build"`.
    pub fn parse(input: &str) -> Result<Self, UpdateError> {
        let trimmed = input.trim();
        let without_v = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);

        // Strip optional build metadata (+...) which does not affect precedence.
        let (core_and_pre, _build) = match without_v.split_once('+') {
            Some((left, right)) => {
                if right.is_empty() {
                    return Err(UpdateError::InvalidVersion(input.to_string()));
                }
                (left, Some(right))
            }
            None => (without_v, None),
        };

        let (core, pre_str) = match core_and_pre.split_once('-') {
            Some((left, right)) => {
                if right.is_empty() {
                    return Err(UpdateError::InvalidVersion(input.to_string()));
                }
                (left, Some(right))
            }
            None => (core_and_pre, None),
        };

        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return Err(UpdateError::InvalidVersion(input.to_string()));
        }

        let parse_part = |s: &str| -> Result<u32, UpdateError> {
            if s.is_empty()
                || !s.bytes().all(|b| b.is_ascii_digit())
                || (s.len() > 1 && s.starts_with('0'))
            {
                return Err(UpdateError::InvalidVersion(input.to_string()));
            }
            s.parse::<u32>()
                .map_err(|_| UpdateError::InvalidVersion(input.to_string()))
        };

        let major = parse_part(parts[0])?;
        let minor = parse_part(parts[1])?;
        let patch = parse_part(parts[2])?;

        let mut prerelease = Vec::new();
        if let Some(pre) = pre_str {
            for ident in pre.split('.') {
                if ident.is_empty()
                    || !ident
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err(UpdateError::InvalidVersion(input.to_string()));
                }
                if ident.bytes().all(|b| b.is_ascii_digit()) {
                    if ident.len() > 1 && ident.starts_with('0') {
                        return Err(UpdateError::InvalidVersion(input.to_string()));
                    }
                    let num = ident
                        .parse::<u64>()
                        .map_err(|_| UpdateError::InvalidVersion(input.to_string()))?;
                    prerelease.push(PrereleaseIdent::Numeric(num));
                } else {
                    prerelease.push(PrereleaseIdent::AlphaNumeric(ident.to_string()));
                }
            }
        }

        Ok(Self {
            major,
            minor,
            patch,
            prerelease,
        })
    }

    /// Returns `true` when this version has prerelease identifiers (beta/alpha/rc).
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }

    /// Encodes `major.minor.patch` into tdesktop's `AppVersion` integer scheme:
    /// `major * 1_000_000 + minor * 1_000 + patch` (e.g. `7.4.5` -> `7_004_005`).
    #[must_use]
    pub fn to_tdesktop_numeric(&self) -> u32 {
        self.major
            .saturating_mul(1_000_000)
            .saturating_add(self.minor.min(999).saturating_mul(1_000))
            .saturating_add(self.patch.min(999))
    }

    /// Decodes a tdesktop numeric `AppVersion` back into a stable [`Version`].
    #[must_use]
    pub const fn from_tdesktop_numeric(numeric: u32) -> Self {
        let major = numeric / 1_000_000;
        let minor = (numeric / 1_000) % 1_000;
        let patch = numeric % 1_000;
        Self::new(major, minor, patch)
    }

    /// Formats the numeric 4-part Windows `VERSIONINFO` string (`major.minor.patch.0`).
    #[must_use]
    pub fn to_windows_quad(&self) -> String {
        format!("{}.{}.{}.0", self.major, self.minor, self.patch)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(
                || match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                    (true, true) => Ordering::Equal,
                    // Stable release has higher precedence than a prerelease of the same core version.
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => {
                        for (a, b) in self.prerelease.iter().zip(other.prerelease.iter()) {
                            let ord = a.cmp(b);
                            if ord != Ordering::Equal {
                                return ord;
                            }
                        }
                        self.prerelease.len().cmp(&other.prerelease.len())
                    }
                },
            )
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prerelease.is_empty() {
            f.write_str("-")?;
            for (idx, ident) in self.prerelease.iter().enumerate() {
                if idx > 0 {
                    f.write_str(".")?;
                }
                write!(f, "{ident}")?;
            }
        }
        Ok(())
    }
}

/// Evaluates whether a candidate release version should be installed over `current`,
/// mirroring `ChannelPolicyAllows` from `core/update_verify.cpp`.
///
/// - When `allow_beta` is `false`, prerelease/beta candidates are rejected.
/// - When `allow_beta` is `true`, both stable and beta candidates are accepted as long as
///   `candidate > current`.
#[must_use]
pub fn channel_allows_update(
    current: &Version,
    candidate: &Version,
    candidate_channel: UpdateChannel,
    allow_beta: bool,
) -> bool {
    if !allow_beta && (candidate_channel == UpdateChannel::Beta || candidate.is_prerelease()) {
        return false;
    }
    candidate > current
}
