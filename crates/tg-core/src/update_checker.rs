//! Port of `core/update_checker.h` and the decision / version-comparison logic of
//! `core/update_checker.cpp`.
//!
//! Network fetching (e.g. GitHub Releases API) is abstracted behind [`UpdateSource`] so
//! scheduling, channel filtering, version comparison, and ready-update verification remain
//! pure and fully testable without network I/O.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::version::AppVersion;

/// Minimum interval between regular update checks (`UpdateDelayConstPart = 8 * 3600`).
pub const UPDATE_DELAY_CONST_SECS: i64 = 8 * 3600;
/// Random jitter added to regular update check intervals (`UpdateDelayRandPart = 8 * 3600`).
pub const UPDATE_DELAY_RAND_SECS: i64 = 8 * 3600;
/// Minimum interval for alpha/canary frequent checks (`kFrequentUpdateDelayConstPart = 600`).
pub const FREQUENT_UPDATE_DELAY_CONST_SECS: i64 = 600;
/// Random jitter for alpha/canary frequent checks (`kFrequentUpdateDelayRandPart = 300`).
pub const FREQUENT_UPDATE_DELAY_RAND_SECS: i64 = 300;
/// Timeout before retrying an update check (`kUpdaterTimeout = 10s`).
pub const UPDATER_TIMEOUT_MS: u64 = 10_000;

/// Legacy alpha marker in `tupdates/temp/tdata/version` (`0x7FFF_FFFF`).
pub const VERSION_FILE_ALPHA_MARKER: u32 = 0x7FFF_FFFF;
/// Canary v2 marker in `tupdates/temp/tdata/version` (`kVersionFileCanaryMarker = 0x7FFF_FFFE`).
pub const VERSION_FILE_CANARY_MARKER: u32 = 0x7FFF_FFFE;

/// Errors returned by update checking or version file handling.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// Filesystem I/O error.
    #[error("update I/O error: {0}")]
    Io(#[from] io::Error),
    /// Network or remote source failure.
    #[error("update source error: {0}")]
    Source(String),
    /// Ready update version file is malformed or corrupted.
    #[error("corrupt update version file")]
    CorruptVersionFile,
}

/// Update release channel (`Updates::Channel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateChannel {
    /// Standard stable releases.
    #[default]
    Stable,
    /// Beta pre-releases.
    Beta,
    /// Public canary builds.
    CanaryPublic,
    /// Private canary builds.
    CanaryPrivate,
}

/// High-level update checker state (`Core::UpdateChecker::State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// Idle / waiting for next scheduled check.
    #[default]
    None,
    /// Querying remote source for latest release metadata.
    Checking,
    /// Downloading update package.
    Download,
    /// Unpacking / verifying downloaded update.
    Unpacking,
    /// Update is unpacked and ready to install on restart.
    Ready,
}

/// Download progress snapshot (`Core::UpdateChecker::Progress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateProgress {
    /// Bytes downloaded so far.
    pub already: u64,
    /// Total expected bytes.
    pub size: u64,
}

/// Metadata describing an available remote release (e.g. from GitHub Releases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Semantic / numeric version of the release.
    pub version: AppVersion,
    /// Release channel.
    pub channel: UpdateChannel,
    /// Direct download URL for the platform package.
    pub download_url: String,
    /// Optional human-readable release notes / changelog.
    pub notes: String,
    /// Optional expected SHA-256 hex digest of the release package.
    pub sha256: Option<String>,
}

/// Reason an update check was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Updater disabled at startup (e.g. external package manager or `-noupdate`).
    Disabled,
    /// Automatic updates turned off in configuration (`!auto_update`).
    AutoUpdateOff,
    /// Secondary instance in multi-instance mode without debug enabled.
    ManyInstance,
    /// Not enough time has elapsed since `last_update_check`.
    NotDueYet {
        /// Seconds remaining until the next check should run (including tdesktop's +5s grace).
        wait_secs: i64,
    },
}

/// Result of evaluating whether to update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    /// Check skipped before querying network.
    Skip(SkipReason),
    /// Installed version is already up to date.
    UpToDate,
    /// A newer compatible release is available.
    UpdateAvailable(ReleaseInfo),
}

/// Inputs to update check scheduling (`Updater::start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateCheckPolicy {
    /// Currently installed version.
    pub current_version: AppVersion,
    /// Active release channel.
    pub channel: UpdateChannel,
    /// Whether beta releases are accepted when on `Stable`.
    pub install_beta: bool,
    /// Whether automatic updates are enabled (`cAutoUpdate()`).
    pub auto_update: bool,
    /// Whether the updater is disabled globally (`UpdaterDisabled()`).
    pub updater_disabled: bool,
    /// Whether another instance owns the installation (`cManyInstance()`).
    pub many_instance: bool,
    /// Whether debug mode is active (`Logs::DebugEnabled()`).
    pub debug_enabled: bool,
    /// Unix timestamp (seconds) of the last check (`cLastUpdateCheck()`).
    pub last_check_unix: i64,
    /// Force waiting even if a partial update file exists (`forceWait`).
    pub force_wait: bool,
    /// Whether an incomplete downloaded update file already exists in `tupdates/`.
    pub has_partial_update_file: bool,
}

impl Default for UpdateCheckPolicy {
    fn default() -> Self {
        Self {
            current_version: AppVersion::CURRENT,
            channel: UpdateChannel::Stable,
            install_beta: false,
            auto_update: true,
            updater_disabled: false,
            many_instance: false,
            debug_enabled: false,
            last_check_unix: 0,
            force_wait: false,
            has_partial_update_file: false,
        }
    }
}

/// Network backend trait for fetching the latest release metadata (e.g. GitHub Releases).
pub trait UpdateSource {
    /// Queries the remote source for the latest release matching `channel` (and optional beta).
    fn fetch_latest_release(
        &self,
        channel: UpdateChannel,
        allow_beta: bool,
    ) -> Result<Option<ReleaseInfo>, UpdateError>;
}

/// Computes seconds remaining until the next update check (`update_checker.cpp:1778-1795`).
///
/// Returns `0` if a check should run immediately.
#[must_use]
pub fn compute_next_check_wait_secs(
    policy: &UpdateCheckPolicy,
    now_unix: i64,
    rand_u64: u64,
) -> i64 {
    let frequent = matches!(
        policy.channel,
        UpdateChannel::CanaryPublic | UpdateChannel::CanaryPrivate
    );
    let const_delay = if frequent {
        FREQUENT_UPDATE_DELAY_CONST_SECS
    } else {
        UPDATE_DELAY_CONST_SECS
    };
    let rand_delay = if frequent {
        FREQUENT_UPDATE_DELAY_RAND_SECS
    } else {
        UPDATE_DELAY_RAND_SECS
    };
    let jitter = (rand_u64 % (rand_delay as u64)) as i64;
    let update_in_secs = policy.last_check_unix + const_delay + jitter - now_unix;

    let mut send_request = update_in_secs <= 0 || update_in_secs > const_delay + rand_delay;
    if !send_request && !policy.force_wait && policy.has_partial_update_file {
        send_request = true;
    }

    if send_request { 0 } else { update_in_secs + 5 }
}

/// Returns `true` if `candidate` is strictly newer than `current` and allowed by channel policy.
#[must_use]
pub fn is_candidate_applicable(
    current: AppVersion,
    current_channel: UpdateChannel,
    allow_beta: bool,
    candidate: &ReleaseInfo,
) -> bool {
    if candidate.download_url.is_empty() {
        return false;
    }
    let channel_allowed = match (current_channel, candidate.channel) {
        (UpdateChannel::Stable, UpdateChannel::Stable) => true,
        (UpdateChannel::Stable, UpdateChannel::Beta) => allow_beta,
        (UpdateChannel::Beta, UpdateChannel::Stable | UpdateChannel::Beta) => true,
        (
            UpdateChannel::CanaryPublic,
            UpdateChannel::CanaryPublic | UpdateChannel::Stable | UpdateChannel::Beta,
        ) => true,
        (UpdateChannel::CanaryPrivate, UpdateChannel::CanaryPrivate) => true,
        _ => false,
    };
    if !channel_allowed {
        return false;
    }
    candidate.version.to_numeric() > current.to_numeric()
}

/// Evaluates scheduling policy and, when due, queries `source` to decide whether to update.
pub struct UpdateChecker<S: UpdateSource> {
    source: S,
    state: UpdateState,
    progress: UpdateProgress,
    available_release: Option<ReleaseInfo>,
}

impl<S: UpdateSource> UpdateChecker<S> {
    /// Creates a new update checker backed by `source`.
    #[must_use]
    pub fn new(source: S) -> Self {
        Self {
            source,
            state: UpdateState::None,
            progress: UpdateProgress::default(),
            available_release: None,
        }
    }

    /// Current updater state.
    #[must_use]
    pub fn state(&self) -> UpdateState {
        self.state
    }

    /// Current download progress.
    #[must_use]
    pub fn progress(&self) -> UpdateProgress {
        self.progress
    }

    /// Latest detected update release, if any.
    #[must_use]
    pub fn available_release(&self) -> Option<&ReleaseInfo> {
        self.available_release.as_ref()
    }

    /// Updates download progress and transitions state to [`UpdateState::Download`].
    pub fn set_progress(&mut self, already: u64, size: u64) {
        self.state = UpdateState::Download;
        self.progress = UpdateProgress { already, size };
    }

    /// Marks the downloaded update as unpacked and ready to install.
    pub fn mark_ready(&mut self) {
        self.state = UpdateState::Ready;
    }

    /// Resets the checker back to [`UpdateState::None`].
    pub fn stop(&mut self) {
        self.state = UpdateState::None;
        self.progress = UpdateProgress::default();
    }

    /// Runs the decision logic against `policy`, querying `source` only if allowed and due.
    pub fn check(
        &mut self,
        policy: &UpdateCheckPolicy,
        now_unix: i64,
        rand_u64: u64,
    ) -> Result<UpdateDecision, UpdateError> {
        if policy.updater_disabled {
            return Ok(UpdateDecision::Skip(SkipReason::Disabled));
        }
        if !policy.auto_update {
            return Ok(UpdateDecision::Skip(SkipReason::AutoUpdateOff));
        }
        if policy.many_instance && !policy.debug_enabled {
            return Ok(UpdateDecision::Skip(SkipReason::ManyInstance));
        }
        let wait_secs = compute_next_check_wait_secs(policy, now_unix, rand_u64);
        if wait_secs > 0 {
            return Ok(UpdateDecision::Skip(SkipReason::NotDueYet { wait_secs }));
        }

        self.state = UpdateState::Checking;
        let fetched = self
            .source
            .fetch_latest_release(policy.channel, policy.install_beta);
        match fetched {
            Ok(Some(release))
                if is_candidate_applicable(
                    policy.current_version,
                    policy.channel,
                    policy.install_beta,
                    &release,
                ) =>
            {
                self.available_release = Some(release.clone());
                Ok(UpdateDecision::UpdateAvailable(release))
            }
            Ok(_) => {
                self.state = UpdateState::None;
                self.available_release = None;
                Ok(UpdateDecision::UpToDate)
            }
            Err(err) => {
                self.state = UpdateState::None;
                Err(err)
            }
        }
    }
}

/// Path to `tupdates/temp/tdata/version` inside `working_dir`.
#[must_use]
pub fn ready_update_version_path(working_dir: &Path) -> PathBuf {
    working_dir
        .join("tupdates")
        .join("temp")
        .join("tdata")
        .join("version")
}

/// Writes `tupdates/temp/tdata/version` in tdesktop's binary format (`WriteUpdateVersionFile`).
pub fn write_update_version_file(
    working_dir: &Path,
    version: AppVersion,
    canary_version: Option<u64>,
) -> Result<(), UpdateError> {
    let path = ready_update_version_path(working_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut data = Vec::with_capacity(12);
    if let Some(canary) = canary_version {
        data.extend_from_slice(&VERSION_FILE_CANARY_MARKER.to_le_bytes());
        data.extend_from_slice(&canary.to_le_bytes());
    } else {
        data.extend_from_slice(&version.to_numeric().to_le_bytes());
    }
    fs::write(path, data)?;
    Ok(())
}

/// Checks whether a staged update in `tupdates/temp` has both a `ready` marker and a strictly
/// newer version in `tupdates/temp/tdata/version` (`PromoteReadyUpdater` version check).
pub fn check_ready_update_version(
    working_dir: &Path,
    current_version: AppVersion,
) -> Result<Option<AppVersion>, UpdateError> {
    let temp_dir = working_dir.join("tupdates").join("temp");
    if !temp_dir.join("ready").is_file() {
        return Ok(None);
    }
    let version_path = ready_update_version_path(working_dir);
    let bytes = match fs::read(&version_path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(UpdateError::Io(e)),
    };
    if bytes.len() < 4 {
        return Err(UpdateError::CorruptVersionFile);
    }
    let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if raw == VERSION_FILE_ALPHA_MARKER || raw == VERSION_FILE_CANARY_MARKER {
        return Ok(None);
    }
    let ready_ver = AppVersion::from_numeric(raw);
    if ready_ver > current_version {
        Ok(Some(ready_ver))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct StaticSource(Option<ReleaseInfo>);

    impl UpdateSource for StaticSource {
        fn fetch_latest_release(
            &self,
            _channel: UpdateChannel,
            _allow_beta: bool,
        ) -> Result<Option<ReleaseInfo>, UpdateError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn scheduling_and_version_comparison_decisions() {
        let release_0_2 = ReleaseInfo {
            version: AppVersion::new(0, 2, 0),
            channel: UpdateChannel::Stable,
            download_url: "https://example.invalid/rustgram-0.2.0.zip".to_owned(),
            notes: "Bug fixes".to_owned(),
            sha256: None,
        };
        let mut checker = UpdateChecker::new(StaticSource(Some(release_0_2.clone())));

        let now = 1_700_000_000i64;
        let mut policy = UpdateCheckPolicy {
            current_version: AppVersion::new(0, 1, 0),
            last_check_unix: now - 3600, // 1h ago -> not due (min 8h)
            ..Default::default()
        };

        // Not due yet.
        assert!(matches!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::Skip(SkipReason::NotDueYet { wait_secs }) if wait_secs > 0
        ));

        // Partial update file forces immediate check when !force_wait.
        policy.has_partial_update_file = true;
        assert_eq!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::UpdateAvailable(release_0_2.clone())
        );

        // Disabled / AutoUpdateOff / ManyInstance skip checks.
        policy.updater_disabled = true;
        assert_eq!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::Skip(SkipReason::Disabled)
        );
        policy.updater_disabled = false;
        policy.auto_update = false;
        assert_eq!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::Skip(SkipReason::AutoUpdateOff)
        );
        policy.auto_update = true;
        policy.many_instance = true;
        assert_eq!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::Skip(SkipReason::ManyInstance)
        );

        // Same version -> UpToDate.
        policy.many_instance = false;
        policy.current_version = AppVersion::new(0, 2, 0);
        assert_eq!(
            checker.check(&policy, now, 0).unwrap(),
            UpdateDecision::UpToDate
        );
    }

    #[test]
    fn beta_channel_filtering() {
        let beta = ReleaseInfo {
            version: AppVersion::new(0, 2, 0),
            channel: UpdateChannel::Beta,
            download_url: "https://example.invalid/beta.zip".to_owned(),
            notes: String::new(),
            sha256: None,
        };
        assert!(!is_candidate_applicable(
            AppVersion::new(0, 1, 0),
            UpdateChannel::Stable,
            false,
            &beta
        ));
        assert!(is_candidate_applicable(
            AppVersion::new(0, 1, 0),
            UpdateChannel::Stable,
            true,
            &beta
        ));
    }

    #[test]
    fn ready_update_version_file_check() {
        let dir = TempDir::new().unwrap();
        let temp_dir = dir.path().join("tupdates").join("temp");
        fs::create_dir_all(&temp_dir).unwrap();
        fs::write(temp_dir.join("ready"), b"1").unwrap();
        write_update_version_file(dir.path(), AppVersion::new(0, 2, 1), None).unwrap();

        assert_eq!(
            check_ready_update_version(dir.path(), AppVersion::new(0, 1, 0)).unwrap(),
            Some(AppVersion::new(0, 2, 1))
        );
        assert_eq!(
            check_ready_update_version(dir.path(), AppVersion::new(0, 2, 1)).unwrap(),
            None
        );
    }
}
