//! `tg-core`: non-GUI application-lifetime core of Rustgram (Rust port of Telegram Desktop).
//!
//! Modules and their tdesktop C++ origins:
//! - [`version`][] — `core/version.h`
//! - [`globals`][] — `settings.h` / `settings.cpp`
//! - [`settings_proxy`][] — `core/core_settings_proxy.h` / `.cpp` + `mtproto/mtproto_proxy_data.h`
//! - [`settings`][] — `core/core_settings.h` / `.cpp`
//! - [`launcher`][] — `core/launcher.h` / `.cpp` + `platform/win/launcher_win.cpp`
//! - [`logs`][] — `logs.h` / `logs.cpp` (exposed as a `tracing_subscriber::Layer`)
//! - [`domain`][] — `main/main_domain.h` / `.cpp` + non-network `main/main_account.h`
//! - [`persistence`][] — `tg-storage` adapters for `CoreSettings`, `GlobalConfig`, and `Domain`
//! - [`crash_reports`][] — `core/crash_reports.h` / `.cpp`
//! - [`update_checker`][] — `core/update_checker.h` / `.cpp` (decision logic & version comparison)
//! - [`sandbox`][] — `core/sandbox.h` / `.cpp` (single-instance IPC protocol & trait)

pub mod crash_reports;
pub mod domain;
pub mod globals;
pub mod launcher;
pub mod local_url;
pub mod logs;
pub mod persistence;
pub mod sandbox;
pub mod settings;
pub mod settings_proxy;
pub mod update_checker;
pub mod version;

pub use crash_reports::{
    CrashKind, CrashReportError, CrashReportManager, CrashStartResult, CrashStatus,
    CrashUploadConfig, install_crash_handlers, install_panic_hook,
    install_unhandled_exception_filter,
};
pub use domain::{
    Account, AccountSession, Domain, DomainError, DomainStorage, DomainStorageError, Environment,
    LoadedDomainState, MAX_ACCOUNTS, MemoryDomainStorage, PREMIUM_MAX_ACCOUNTS, StartResult,
};
pub use globals::{
    GlobalConfig, LaunchMode, LayoutDirection, Platform, SCALE_ALWAYS_ALLOW_MAX, SCALE_AUTO,
    SCALE_DEFAULT, SCALE_MAX, SCALE_MIN, check_scale,
};
pub use launcher::{
    LauncherError, ParsedArgs, RelaunchAction, ResolvedWorkingDir, WorkDirEnv, WorkingDirOrigin,
    apply_to_config, build_relaunch_args, parse_arguments, relaunch_self, resolve_working_dir,
};
pub use local_url::{
    LocalUrlRoute, MAX_LOCAL_URL_LEN, parse_local_url, parse_proxy_link, try_convert_url_to_local,
};
pub use logs::{LogCategory, LogError, LogManager, TdLogLayer};
pub use persistence::TdataDomainStorage;
pub use sandbox::{
    IncomingBatch, SingleInstanceCommand, SingleInstanceCoordinator, SingleInstanceError,
    SingleInstanceResponse, SingleInstanceStartup, SingleInstanceTransport,
};
pub use settings::{AutoplaySettings, CoreSettings, PowerSavingFlags, SettingsError};
pub use settings_proxy::{
    ProxyData, ProxySettingsError, ProxySettingsMode, ProxyType, SettingsProxy,
};
pub use tg_storage::qdatastream::{
    DataStreamError, QDataStreamReader, QDataStreamWriter, QtColor, QtColor as RgbaColor,
};
pub use update_checker::{
    ReleaseInfo, SkipReason, UpdateChannel, UpdateCheckPolicy, UpdateChecker, UpdateDecision,
    UpdateError, UpdateProgress, UpdateSource, UpdateState,
};
pub use version::{
    APP_ALPHA_VERSION, APP_BETA_VERSION, APP_FILE, APP_ID, APP_NAME, APP_VERSION, APP_VERSION_STR,
    AppVersion, PORTABLE_FOLDER_NAME, TDESKTOP_COMPAT_VERSION,
};
