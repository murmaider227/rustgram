//! High-level update checker and staging workflow.
//!
//! Port of `Core::UpdateChecker` from `Telegram/SourceFiles/core/update_checker.cpp`.

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;

use crate::github::{
    AvailableUpdate, DEFAULT_GITHUB_API_BASE, HttpClient, MAX_METADATA_SIZE, MAX_PACKAGE_SIZE,
    parse_releases_json, parse_sha256_sums, releases_api_url_with_base, select_release,
};
use crate::package::{promote_ready_updater, unpack_verified_package, verify_package_with_hash};
use crate::version::{Target, Version};
use crate::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UpdateError};

/// Configuration for [`UpdateChecker`].
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// GitHub repository (`owner/repo`), e.g. `"rustgram/rustgram"`.
    pub github_repo: String,
    /// Optional API base URL override (defaults to [`DEFAULT_GITHUB_API_BASE`]).
    /// Set to a local `http://127.0.0.1:<port>` server in offline integration tests.
    pub api_base_url: Option<String>,
    /// Currently running application version.
    pub current_version: Version,
    /// Whether prerelease/beta versions should be offered and accepted.
    pub allow_beta: bool,
    /// Expected platform target (defaults to [`Target::current()`]).
    pub target: Target,
    /// Working directory where `tupdates/` is staged.
    pub working_dir: PathBuf,
    /// Installation directory containing `rustgram.exe` and `rustgram-updater.exe`.
    pub install_dir: PathBuf,
    /// Ed25519 public key used to verify update envelopes.
    pub verifying_key: VerifyingKey,
}

/// Metadata describing a verified update that has been staged into `tupdates/temp`.
#[derive(Debug, Clone)]
pub struct StagedUpdate {
    /// Version of the staged update.
    pub version: Version,
    /// Path to `<working_dir>/tupdates/temp`.
    pub staging_dir: PathBuf,
    /// Whether `rustgram-updater.exe` was promoted into `install_dir`.
    pub updater_promoted: bool,
}

/// Checks GitHub Releases, downloads `.tupdate` assets, verifies their Ed25519 signatures,
/// and unpacks them into `<working_dir>/tupdates/temp`.
pub struct UpdateChecker<C: HttpClient> {
    client: C,
    config: UpdateConfig,
}

impl<C: HttpClient> UpdateChecker<C> {
    /// Creates a new [`UpdateChecker`] with the given HTTP client and configuration.
    #[must_use]
    pub fn new(client: C, config: UpdateConfig) -> Self {
        Self { client, config }
    }

    /// Queries GitHub Releases JSON and returns the newest eligible update, if any.
    ///
    /// Fails closed with [`UpdateError::UnconfiguredPublicKey`] if the configured verifying key
    /// is the all-zero placeholder.
    pub fn check(&self) -> Result<Option<AvailableUpdate>, UpdateError> {
        if crate::is_placeholder_public_key(self.config.verifying_key.as_bytes()) {
            return Err(UpdateError::UnconfiguredPublicKey);
        }
        let base = self
            .config
            .api_base_url
            .as_deref()
            .unwrap_or(DEFAULT_GITHUB_API_BASE);
        let url = releases_api_url_with_base(base, &self.config.github_repo);
        let headers = [
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", "rustgram-updater"),
        ];
        let body = self.client.get_bytes(&url, &headers, MAX_METADATA_SIZE)?;
        let releases = parse_releases_json(&body)?;
        Ok(select_release(
            &releases,
            &self.config.current_version,
            self.config.allow_beta,
            self.config.target,
        ))
    }

    /// Downloads `available`, verifies its optional SHA-256 digest and Ed25519 signature,
    /// unpacks it into `<working_dir>/tupdates/temp`, and promotes `rustgram-updater.exe` if bundled.
    ///
    /// Fails closed with [`UpdateError::UnconfiguredPublicKey`] before any network or disk
    /// activity when the verifying key is the all-zero placeholder.
    pub fn download_and_stage(
        &self,
        available: &AvailableUpdate,
    ) -> Result<StagedUpdate, UpdateError> {
        if crate::is_placeholder_public_key(self.config.verifying_key.as_bytes()) {
            return Err(UpdateError::UnconfiguredPublicKey);
        }
        let headers = [("User-Agent", "rustgram-updater")];
        let expected_sha256 = if let Some(hash) = available.expected_sha256 {
            Some(hash)
        } else if let Some(ref sums_url) = available.sha256_sums_url {
            let sums_bytes = self
                .client
                .get_bytes(sums_url, &headers, MAX_METADATA_SIZE)?;
            let sums_text = std::str::from_utf8(&sums_bytes).map_err(|_| {
                UpdateError::MalformedPackage("SHA256SUMS.txt is not valid UTF-8".into())
            })?;
            parse_sha256_sums(sums_text, &available.asset_name)
        } else {
            None
        };

        let bytes = self
            .client
            .get_bytes(&available.download_url, &headers, MAX_PACKAGE_SIZE)?;

        let verified = verify_package_with_hash(
            &bytes,
            expected_sha256.as_ref(),
            &self.config.verifying_key,
            &self.config.current_version,
            self.config.allow_beta,
            self.config.target,
        )?;
        // The release tag picked this candidate; the signed package must be that exact version, so
        // a wrong file attached to a release is never installed under another release's name.
        if verified.envelope.version != available.version {
            return Err(UpdateError::ReleaseVersionMismatch {
                release: available.version.to_string(),
                package: verified.envelope.version.to_string(),
            });
        }

        let staging_dir = unpack_verified_package(&verified, &self.config.working_dir)?;
        let updater_promoted =
            promote_ready_updater(&self.config.working_dir, &self.config.install_dir)?;

        Ok(StagedUpdate {
            version: verified.envelope.version,
            staging_dir,
            updater_promoted,
        })
    }

    /// Runs [`Self::check`] followed by [`Self::download_and_stage`] if a newer release exists.
    pub fn check_and_stage(&self) -> Result<Option<StagedUpdate>, UpdateError> {
        let Some(available) = self.check()? else {
            return Ok(None);
        };
        self.download_and_stage(&available).map(Some)
    }
}

/// Returns `true` when `<working_dir>/tupdates/temp/ready` exists and is ready to install.
#[must_use]
pub fn has_ready_update(working_dir: &Path) -> bool {
    working_dir
        .join(TUPDATES_DIR_NAME)
        .join("temp")
        .join("ready")
        .is_file()
}

/// Deletes `<working_dir>/tupdates`, matching `ClearAll` in `core/update_checker.cpp`.
pub fn clear_updates(working_dir: &Path) -> Result<(), UpdateError> {
    let dir = working_dir.join(TUPDATES_DIR_NAME);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Options used to construct the `rustgram-updater` command-line invocation when applying an update.
#[derive(Debug, Clone, Default)]
pub struct UpdaterLaunchOptions {
    /// Parent process ID for `-finishprocess`.
    pub parent_pid: Option<u32>,
    /// Parent process creation timestamp (Windows `FILETIME` ticks) for `-finishprocess`.
    pub parent_created: Option<u64>,
    /// Custom `-workdir` to forward back to `rustgram.exe`.
    pub workdir: Option<PathBuf>,
    /// Custom `-key` to forward back to `rustgram.exe`.
    pub key: Option<String>,
    /// Executable filename to launch (defaults to `rustgram.exe`).
    pub exe_name: Option<String>,
    /// Forward `-autostart`.
    pub autostart: bool,
    /// Forward `-debug`.
    pub debug: bool,
    /// Forward `-startintray`.
    pub start_in_tray: bool,
    /// Pass `-justupdate` (do not relaunch after applying).
    pub just_update: bool,
}

/// Builds the argument vector for launching `rustgram-updater.exe`.
#[must_use]
pub fn build_updater_launch_args(opts: &UpdaterLaunchOptions) -> Vec<String> {
    let mut args = vec!["-update".to_string()];
    if let (Some(pid), Some(created)) = (opts.parent_pid, opts.parent_created) {
        args.push("-finishprocess".to_string());
        args.push(pid.to_string());
        args.push(created.to_string());
    }
    if opts.autostart {
        args.push("-autostart".to_string());
    }
    if opts.debug {
        args.push("-debug".to_string());
    }
    if opts.start_in_tray {
        args.push("-startintray".to_string());
    }
    if opts.just_update {
        args.push("-justupdate".to_string());
    }
    if let Some(ref workdir) = opts.workdir {
        args.push("-workdir".to_string());
        args.push(workdir.to_string_lossy().into_owned());
    }
    if let Some(ref key) = opts.key {
        args.push("-key".to_string());
        args.push(key.clone());
    }
    if let Some(ref exe_name) = opts.exe_name
        && !exe_name.eq_ignore_ascii_case(DEFAULT_EXE_NAME)
    {
        args.push("-exename".to_string());
        args.push(exe_name.clone());
    }
    args
}
