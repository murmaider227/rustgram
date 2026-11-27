//! Startup, self-updater, and single-instance activation DTOs shared across
//! `tg-app-core`, `tg-app-settings`, `tg-app-window`, and `tg-app`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tg_updater::github::AvailableUpdate;
use tg_updater::version::Version;

/// Persisted startup preferences stored in `<working_dir>/tdata/startup_prefs.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupPrefs {
    /// Whether automatic update checks are enabled (`gAutoUpdate`).
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Whether beta pre-releases are accepted (`gInstallBetaVersion`).
    #[serde(default)]
    pub install_beta: bool,
    /// Whether `tg://` and `tonsite://` URL handlers should be registered on first run.
    #[serde(default = "default_true")]
    pub register_url_schemes: bool,
    /// Opt-in: whether Rustgram may make itself the system `tg://` / `tonsite://` handler.
    /// Off by default (owner, 2026-11-26): while Rustgram is in testing, browser links keep opening
    /// the user's own Telegram. The older `register_url_schemes` key is ignored for this reason.
    #[serde(default)]
    pub take_url_schemes: bool,
    /// Whether first-run URL scheme registration has already completed for this working dir.
    #[serde(default)]
    pub url_schemes_registered: bool,
    /// Whether `-autostart` should launch minimized to the system tray.
    #[serde(default = "default_true")]
    pub start_minimized_on_autostart: bool,
    /// Unix timestamp (seconds) of the last completed update check.
    #[serde(default)]
    pub last_update_check_unix: i64,
}

const fn default_true() -> bool {
    true
}

impl Default for StartupPrefs {
    fn default() -> Self {
        Self {
            auto_update: true,
            install_beta: false,
            register_url_schemes: true,
            take_url_schemes: false,
            url_schemes_registered: false,
            start_minimized_on_autostart: true,
            last_update_check_unix: 0,
        }
    }
}

impl StartupPrefs {
    /// Path to the persisted startup preferences file inside `working_dir`.
    #[must_use]
    pub fn path_in(working_dir: &Path) -> PathBuf {
        working_dir.join("tdata").join("startup_prefs.json")
    }

    /// Loads preferences from `working_dir`, returning defaults if absent or malformed.
    #[must_use]
    pub fn load_from(working_dir: &Path) -> Self {
        let path = Self::path_in(working_dir);
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Persists preferences to `<working_dir>/tdata/startup_prefs.json`.
    pub fn save_to(&self, working_dir: &Path) -> std::io::Result<()> {
        let path = Self::path_in(working_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }
}

/// Returns the compiled-in application SemVer parsed from `CARGO_PKG_VERSION`.
#[must_use]
pub fn current_app_version() -> Version {
    // Release builds get their version (e.g. 0.1.0-alpha.3) from `tg-xtask dist` through
    // RUSTGRAM_VERSION; the crate version alone would make every alpha think it is 0.1.0 and refuse
    // the next 0.1.0-alpha.N update.
    app_version_from(option_env!("RUSTGRAM_VERSION"), env!("CARGO_PKG_VERSION"))
}

/// The release version when one was set at build time, otherwise the crate version.
pub fn app_version_from(release: Option<&str>, crate_version: &str) -> Version {
    release
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| Version::parse(v.trim_start_matches('v')).ok())
        .or_else(|| Version::parse(crate_version).ok())
        .unwrap_or_else(|| Version::new(0, 1, 0))
}

#[cfg(test)]
mod app_version_tests {
    use super::*;

    #[test]
    fn release_builds_use_the_release_version() {
        let v = app_version_from(Some("0.1.0-alpha.3"), "0.1.0");
        assert_eq!(v, Version::parse("0.1.0-alpha.3").expect("version"));
        assert!(v.is_prerelease());
        assert_eq!(
            app_version_from(Some("v0.2.0"), "0.1.0"),
            Version::parse("0.2.0").expect("version")
        );
    }

    #[test]
    fn dev_builds_fall_back_to_the_crate_version() {
        assert_eq!(
            app_version_from(None, "0.1.0"),
            Version::parse("0.1.0").expect("version")
        );
        assert_eq!(
            app_version_from(Some("  "), "0.1.0"),
            Version::parse("0.1.0").expect("version")
        );
        assert_eq!(
            app_version_from(Some("not a version"), "0.1.0"),
            Version::parse("0.1.0").expect("version")
        );
    }
}

/// High-level update state readable by the Main Menu and Settings > Advanced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// Update checking is disabled (e.g. `-noupdate` CLI flag or auto-update off).
    Disabled,
    /// Idle and waiting for the next scheduled or manual check.
    #[default]
    Idle,
    /// Querying GitHub Releases metadata.
    Checking,
    /// Installed version is already the newest eligible release.
    UpToDate {
        /// Unix timestamp (seconds) when the check completed.
        checked_at_unix: i64,
    },
    /// A newer release was found and is queued for download.
    Available(AvailableUpdate),
    /// Downloading and verifying the `.tupdate` package.
    Downloading {
        /// Target release version.
        version: Version,
        /// Bytes received so far.
        downloaded_bytes: u64,
        /// Expected total bytes (from release metadata).
        total_bytes: u64,
    },
    /// Signed update has been verified, unpacked into `tupdates/temp`, and is ready to install.
    Ready {
        /// Ready release version.
        version: Version,
        /// Path to `<working_dir>/tupdates/temp`.
        staging_dir: PathBuf,
        /// Whether `rustgram-updater.exe` was promoted into the install directory.
        updater_promoted: bool,
    },
    /// The last check or download failed.
    Failed {
        /// Human-readable error summary.
        message: String,
        /// Unix timestamp (seconds) when the failure occurred.
        checked_at_unix: i64,
    },
}

/// Read-only snapshot of updater state and policy exposed to UI screens (Main Menu and Settings > Advanced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSnapshot {
    /// Current state of the updater state machine.
    pub state: UpdateState,
    /// Currently running application version.
    pub current_version: Version,
    /// Whether periodic background checks are enabled in preferences.
    pub auto_update_enabled: bool,
    /// Whether beta pre-releases are allowed.
    pub allow_beta: bool,
    /// Whether `-noupdate` was passed on the command line for this session.
    pub disabled_by_cli: bool,
    /// Unix timestamp (seconds) of the last completed check (`0` if never checked).
    pub last_check_unix: i64,
    /// Scheduled Unix timestamp (seconds) for the next periodic check, if applicable.
    pub next_check_unix: Option<i64>,
}

impl Default for UpdateSnapshot {
    fn default() -> Self {
        Self {
            state: UpdateState::Idle,
            current_version: current_app_version(),
            auto_update_enabled: true,
            allow_beta: false,
            disabled_by_cli: false,
            last_check_unix: 0,
            next_check_unix: None,
        }
    }
}

impl UpdateSnapshot {
    /// Returns `true` when an unpacked, signature-verified update is ready to apply on restart.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self.state, UpdateState::Ready { .. })
    }

    /// Returns the staged update version when [`UpdateState::Ready`].
    #[must_use]
    pub fn ready_version(&self) -> Option<&Version> {
        match &self.state {
            UpdateState::Ready { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Returns `true` when a check or download is currently in flight.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Checking | UpdateState::Downloading { .. }
        )
    }

    /// Returns `true` if a manual update check can currently be initiated.
    #[must_use]
    pub fn can_check_now(&self) -> bool {
        !self.disabled_by_cli && !self.is_busy() && !self.is_ready()
    }

    /// Formats a concise human-readable status string suitable for Main Menu and Settings > Advanced.
    #[must_use]
    pub fn status_label(&self) -> String {
        match &self.state {
            UpdateState::Disabled => {
                if self.disabled_by_cli {
                    "Updates disabled (-noupdate)".to_owned()
                } else {
                    "Automatic updates are off".to_owned()
                }
            }
            UpdateState::Idle => format!("Version {}", self.current_version),
            UpdateState::Checking => "Checking for updates...".to_owned(),
            UpdateState::UpToDate { .. } => {
                format!("Up to date (v{})", self.current_version)
            }
            UpdateState::Available(avail) => format!("Update v{} available", avail.version),
            UpdateState::Downloading {
                version,
                downloaded_bytes,
                total_bytes,
            } => {
                if *total_bytes > 0 {
                    let pct = ((*downloaded_bytes as f64 / *total_bytes as f64) * 100.0)
                        .clamp(0.0, 100.0);
                    format!("Downloading v{version} ({pct:.0}%)...")
                } else {
                    format!("Downloading v{version}...")
                }
            }
            UpdateState::Ready { version, .. } => {
                format!("Update v{version} ready — restart to install")
            }
            UpdateState::Failed { message, .. } => format!("Update check failed: {message}"),
        }
    }
}

/// UI-originated updater action forwarded from `WindowController` to `TgforkApp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateUiAction {
    /// Trigger an immediate update check in the background.
    CheckNow,
    /// Launch `rustgram-updater.exe` and exit the application to install a staged update.
    RestartToUpdate,
}

/// Callback sink implemented by `startup::updater::UpdateController` so `tg-app-settings`
/// can propagate `auto_update` / `allow_beta` preferences without depending on `startup`.
pub trait UpdatePreferencesSink {
    /// Applies updated `auto_update` / `allow_beta` policy to the running updater.
    fn update_preferences(&self, auto_update: bool, allow_beta: bool);
}

/// Typed incoming activation delivered from a secondary launch over the named pipe
/// (or queued from initial CLI arguments on startup).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncomingActivation {
    /// External `tg://`, `tonsite://`, `http(s)://` links to open.
    pub urls: Vec<String>,
    /// Local file paths passed via `-sendpath` or Explorer "Send to" to open in ShareBox.
    pub send_paths: Vec<PathBuf>,
    /// Bring the main window to the foreground (`CMD:show;` or implicit on URL/file open).
    pub show: bool,
    /// Request the running instance to exit cleanly (`CMD:quit;`).
    pub quit: bool,
}

impl IncomingActivation {
    /// Returns `true` if this activation contains any actionable payload.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.urls.is_empty() && self.send_paths.is_empty() && !self.show && !self.quit
    }
}
