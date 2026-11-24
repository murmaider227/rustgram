//! Port of `settings.h`, `settings.cpp`, and scale constants from `style_core_scale.h`.
//!
//! Typed runtime configuration replacing tdesktop's global `g*` / `c*` variables.

use std::path::PathBuf;

use crate::version::{APP_ALPHA_VERSION, APP_BETA_VERSION};

/// Auto-detect interface scale from system DPI (`style::kScaleAuto`).
pub const SCALE_AUTO: i32 = 0;
/// Minimum allowed UI scale percentage (`style::kScaleMin`).
pub const SCALE_MIN: i32 = 50;
/// Default 100% UI scale (`style::kScaleDefault`).
pub const SCALE_DEFAULT: i32 = 100;
/// Maximum allowed UI scale percentage (`style::kScaleMax`).
pub const SCALE_MAX: i32 = 300;
/// Maximum UI scale always allowed regardless of screen ratio (`style::kScaleAlwaysAllowMax`).
pub const SCALE_ALWAYS_ALLOW_MAX: i32 = 200;

/// Host operating system / platform variant (port of `base::Platform` / `cPlatform`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Windows desktop (x64 / x86 / ARM64).
    Windows,
    /// macOS.
    MacOs,
    /// Linux / Unix desktop.
    Linux,
    /// Other / unrecognized platform.
    Other,
}

impl Platform {
    /// Detects the compile-time target platform.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    /// Returns the platform tag written into crash reports (`CrashReports::PlatformString`).
    #[must_use]
    pub const fn crash_report_name(self) -> &'static str {
        match self {
            Self::Windows => {
                if cfg!(target_arch = "aarch64") {
                    "WindowsARM64"
                } else if cfg!(target_pointer_width = "64") {
                    "Windows64Bit"
                } else {
                    "Windows32Bit"
                }
            }
            Self::MacOs => "MacOS",
            Self::Linux => "Linux",
            Self::Other => "Unknown",
        }
    }
}

/// UI text layout direction (`Qt::LayoutDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutDirection {
    /// Left-to-right text and layout.
    #[default]
    LeftToRight,
    /// Right-to-left text and layout.
    RightToLeft,
}

/// Application startup mode parsed from command-line switches (`LaunchMode` in `settings.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchMode {
    /// Standard interactive startup.
    #[default]
    Normal,
    /// Started automatically at OS login (`-autostart`).
    AutoStart,
    /// Fix-previous migration helper mode (`-fixprevious`).
    FixPrevious,
    /// Stop running instance and clean up OS integration (`-cleanup`).
    Cleanup,
}

/// Typed runtime configuration aggregating tdesktop's `settings.cpp` globals.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalConfig {
    /// Detected host platform.
    pub platform: Platform,
    /// Active UI layout direction.
    pub lang_dir: LayoutDirection,

    /// Whether beta updates should be installed (`gInstallBetaVersion`).
    pub install_beta_version: bool,
    /// Active alpha version (`gAlphaVersion`).
    pub alpha_version: u64,
    /// Real alpha build version (`gRealAlphaVersion`).
    pub real_alpha_version: u64,
    /// Closed-alpha private key bytes (`gAlphaPrivateKey`).
    pub alpha_private_key: Vec<u8>,

    /// Launch with OS login (`gAutoStart`).
    pub auto_start: bool,
    /// Start window minimized (`gStartMinimized`).
    pub start_minimized: bool,
    /// Start hidden in the system tray (`gStartInTray`).
    pub start_in_tray: bool,
    /// Windows "Send to" context menu entry enabled (`gSendToMenu`).
    pub send_to_menu: bool,
    /// Selected launch mode (`gLaunchMode`).
    pub launch_mode: LaunchMode,

    /// Resolved application working directory (`gWorkingDir`).
    pub working_dir: PathBuf,
    /// Whether the working directory was explicitly overridden via `-workdir`.
    pub custom_working_dir: bool,
    /// Directory containing the running executable (`cExeDir`).
    pub exe_dir: PathBuf,
    /// File name of the running executable (`cExeName`).
    pub exe_name: String,
    /// Custom `-key` profile name (empty means default `"data"`).
    pub key_file: String,

    /// Last directory opened in a file dialog (`gDialogLastPath`).
    pub dialog_last_path: PathBuf,
    /// Helper directory for file dialogs (`gDialogHelperPath`).
    pub dialog_helper_path: PathBuf,

    /// Automatic update checking enabled (`gAutoUpdate`).
    pub auto_update: bool,
    /// Whether the minimize-to-tray tooltip has been shown (`gSeenTrayTooltip`).
    pub seen_tray_tooltip: bool,
    /// Restarting to install a downloaded update (`gRestartingUpdate`).
    pub restarting_update: bool,
    /// Restarting the application (`gRestarting`).
    pub restarting: bool,
    /// Reopen directly into Settings after restart (`gRestartingToSettings`).
    pub restarting_to_settings: bool,
    /// Unix timestamp of the last update check (`gLastUpdateCheck`).
    pub last_update_check: i32,
    /// Suppress startup update check (`gNoStartUpdate`, `-noupdate`).
    pub no_start_update: bool,
    /// Open directly to Settings (`gStartToSettings`, `-tosettings`).
    pub start_to_settings: bool,
    /// Enable debug logging (`gDebugMode`, `-debug`).
    pub debug_mode: bool,
    /// Connect to Telegram test datacenters (`-testmode`).
    pub test_mode: bool,
    /// Headless test-agent mode (`gTestAgent`, `-testagent`).
    pub test_agent: bool,
    /// Multiple instances permitted / detected (`gManyInstance`, `-many`).
    pub many_instance: bool,
    /// Request running instance to quit (`gQuit`, `-quit`).
    pub quit: bool,
    /// External control interface enabled (`-externalinterface`).
    pub external_interface: bool,

    /// Number of MTP connections per session (`gConnectionsInSession`).
    pub connections_in_session: u32,
    /// Local storage salt bytes (`gLocalSalt`).
    pub local_salt: Vec<u8>,

    /// System-computed screen scale percentage (`gScreenScale`).
    pub screen_scale: i32,
    /// User-configured interface scale percentage (`gConfigScale`).
    pub config_scale: i32,
    /// Effective active UI scale (`cScale`).
    pub scale: i32,
    /// Display device pixel ratio (1..=3).
    pub device_pixel_ratio: i32,

    /// Pending startup URLs (`tg://`, `tonsite://`, etc., `gStartUrls`).
    pub start_urls: Vec<String>,
    /// Pending local file paths to send (`-sendpath` or trailing file paths).
    pub send_paths: Vec<PathBuf>,

    /// Whether a cloud password recovery just completed (`gPasswordRecovered`).
    pub password_recovered: bool,
    /// Number of consecutive wrong local passcode attempts (`gPasscodeBadTries`).
    pub passcode_bad_tries: i32,
    /// Monotonic timestamp (ms) of the last wrong passcode attempt (`gPasscodeLastTry`).
    pub passcode_last_try_ms: i64,

    /// Timestamp when another session was last seen online (`gOtherOnline`).
    pub other_online: i32,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            platform: Platform::current(),
            lang_dir: LayoutDirection::LeftToRight,
            install_beta_version: APP_BETA_VERSION,
            alpha_version: APP_ALPHA_VERSION,
            real_alpha_version: APP_ALPHA_VERSION,
            alpha_private_key: Vec::new(),
            auto_start: false,
            start_minimized: false,
            start_in_tray: false,
            send_to_menu: false,
            launch_mode: LaunchMode::Normal,
            working_dir: PathBuf::new(),
            custom_working_dir: false,
            exe_dir: PathBuf::new(),
            exe_name: String::new(),
            key_file: String::new(),
            dialog_last_path: PathBuf::new(),
            dialog_helper_path: PathBuf::new(),
            auto_update: true,
            seen_tray_tooltip: false,
            restarting_update: false,
            restarting: false,
            restarting_to_settings: false,
            last_update_check: 0,
            no_start_update: false,
            start_to_settings: false,
            debug_mode: false,
            test_mode: false,
            test_agent: false,
            many_instance: false,
            quit: false,
            external_interface: false,
            connections_in_session: 1,
            local_salt: Vec::new(),
            screen_scale: SCALE_DEFAULT,
            config_scale: SCALE_AUTO,
            scale: SCALE_DEFAULT,
            device_pixel_ratio: 1,
            start_urls: Vec::new(),
            send_paths: Vec::new(),
            password_recovered: false,
            passcode_bad_tries: 0,
            passcode_last_try_ms: 0,
            other_online: 0,
        }
    }
}

impl GlobalConfig {
    /// Returns `true` when right-to-left layout is active (`rtl()`).
    #[must_use]
    pub fn is_rtl(&self) -> bool {
        self.lang_dir == LayoutDirection::RightToLeft
    }

    /// Returns the tdata key/profile name (`cDataFile()`): `key_file` if non-empty, else `"data"`.
    #[must_use]
    pub fn data_file(&self) -> &str {
        if self.key_file.is_empty() {
            "data"
        } else {
            &self.key_file
        }
    }

    /// Returns `dialog_helper_path` if non-empty, otherwise `exe_dir` (`cDialogHelperPathFinal`).
    #[must_use]
    pub fn dialog_helper_path_final(&self) -> &PathBuf {
        if self.dialog_helper_path.as_os_str().is_empty() {
            &self.exe_dir
        } else {
            &self.dialog_helper_path
        }
    }

    /// Returns `true` if another local passcode attempt is allowed at `now_ms` (`passcodeCanTry`).
    ///
    /// Exact backoff schedule from `settings.h`:
    /// - `< 3` bad tries: immediate
    /// - `3`: 5s, `4`: 10s, `5`: 15s, `6`: 20s, `7`: 25s, `>= 8`: 30s.
    #[must_use]
    pub fn passcode_can_try(&self, now_ms: i64) -> bool {
        if self.passcode_bad_tries < 3 {
            return true;
        }
        let dt = now_ms.saturating_sub(self.passcode_last_try_ms);
        let required = match self.passcode_bad_tries {
            3 => 5_000,
            4 => 10_000,
            5 => 15_000,
            6 => 20_000,
            7 => 25_000,
            _ => 30_000,
        };
        dt >= required
    }

    /// Evaluates a scale setting against `screen_scale` (`cEvalScale`).
    #[must_use]
    pub fn eval_scale(&self, scale: i32) -> i32 {
        if scale == SCALE_AUTO {
            self.screen_scale
        } else {
            scale
        }
    }

    /// Validates `config_scale` and updates `scale` (`ValidateScale`).
    pub fn validate_scale(&mut self) {
        self.config_scale = check_scale(self.config_scale);
        let evaluated = self.eval_scale(self.config_scale);
        self.scale = if evaluated == SCALE_AUTO {
            SCALE_DEFAULT
        } else {
            evaluated.clamp(SCALE_MIN, SCALE_MAX)
        };
    }
}

/// Validates a scale value into `SCALE_AUTO` or `SCALE_MIN..=SCALE_MAX` (`style::CheckScale`).
#[must_use]
pub fn check_scale(scale: i32) -> i32 {
    if scale == SCALE_AUTO {
        SCALE_AUTO
    } else if (SCALE_MIN..=SCALE_MAX).contains(&scale) {
        scale
    } else {
        SCALE_AUTO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passcode_backoff_matches_tdesktop() {
        let mut cfg = GlobalConfig {
            passcode_last_try_ms: 1_000,
            passcode_bad_tries: 2,
            ..Default::default()
        };
        assert!(cfg.passcode_can_try(1_000));

        for (tries, wait_ms) in [
            (3, 5_000),
            (4, 10_000),
            (5, 15_000),
            (6, 20_000),
            (7, 25_000),
            (10, 30_000),
        ] {
            cfg.passcode_bad_tries = tries;
            assert!(!cfg.passcode_can_try(1_000 + wait_ms - 1));
            assert!(cfg.passcode_can_try(1_000 + wait_ms));
        }
    }

    #[test]
    fn scale_validation() {
        let mut cfg = GlobalConfig {
            screen_scale: 150,
            config_scale: SCALE_AUTO,
            ..Default::default()
        };
        cfg.validate_scale();
        assert_eq!(cfg.scale, 150);

        cfg.config_scale = 125;
        cfg.validate_scale();
        assert_eq!(cfg.scale, 125);

        cfg.config_scale = 999;
        cfg.validate_scale();
        assert_eq!(cfg.config_scale, SCALE_AUTO);
        assert_eq!(cfg.scale, 150);
    }

    #[test]
    fn data_file_defaults_to_data() {
        let mut cfg = GlobalConfig::default();
        assert_eq!(cfg.data_file(), "data");
        cfg.key_file = "work".to_owned();
        assert_eq!(cfg.data_file(), "work");
    }
}
