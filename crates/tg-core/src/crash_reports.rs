//! Port of `core/crash_reports.h` and `core/crash_reports.cpp`.
//!
//! Tracks unclean shutdowns via the `<working_dir>/tdata/working` state flag / dump file,
//! records key-value crash annotations, writes plain-text crash reports (version, OS, thread,
//! backtrace, and recent log lines) into `<working_dir>/tdata/dumps/` on Rust panics and
//! Windows unhandled structured exceptions (`SetUnhandledExceptionFilter`), and flushes logs
//! before terminating. Upload is disabled by default (`CrashUploadConfig::default()`).

use std::backtrace::Backtrace;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use jiff::ZonedDateTime;
use thiserror::Error;

use crate::globals::{GlobalConfig, Platform};
use crate::logs::{self, LogManager};
use crate::version::{APP_BETA_VERSION, APP_VERSION, APP_VERSION_STR};

/// Maximum bytes read back from a previous crash dump (`crash_reports.cpp:439`).
pub const MAX_CRASH_DUMP_BYTES: usize = 256 * 1024;

/// Default number of trailing log lines included in each crash report.
pub const DEFAULT_CRASH_LOG_TAIL_LINES: usize = 40;

static DUMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Errors returned by crash-report initialization or I/O.
#[derive(Debug, Error)]
pub enum CrashReportError {
    /// Filesystem I/O error.
    #[error("crash report I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Category of crash recorded by [`CrashReportManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    /// Rust thread panic captured via `std::panic::set_hook`.
    Panic,
    /// Native Windows SEH unhandled exception captured via `SetUnhandledExceptionFilter`.
    WindowsException,
}

/// Optional crash-report upload configuration.
///
/// Defaults to disabled (`enabled: false`, `endpoint: None`) so no crash data is ever
/// uploaded automatically.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CrashUploadConfig {
    /// Whether uploading crash reports is explicitly enabled (defaults to `false`).
    pub enabled: bool,
    /// Optional upload endpoint URL (defaults to `None`).
    pub endpoint: Option<String>,
}

impl CrashUploadConfig {
    /// Returns a disabled upload configuration (no endpoint, never uploads).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            endpoint: None,
        }
    }

    /// Returns `true` only when upload is explicitly enabled AND a non-empty endpoint is set.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled
            && self
                .endpoint
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty())
    }
}

/// Outcome of [`CrashReportManager::start`] (`CrashReports::StartResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashStartResult {
    /// Clean startup: `tdata/working` did not exist and has now been created.
    Started,
    /// Previous launch crashed or exited uncleanly; contains the bytes read from `tdata/working`
    /// (may be empty if the process was killed before writing a dump).
    PreviousCrash(Vec<u8>),
}

/// Status returned by [`CrashReportManager::restart`] (`CrashReports::Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashStatus {
    /// Working flag file opened and ready.
    Started,
    /// Could not open `tdata/working` for writing.
    CantOpen,
}

/// Manages the `tdata/working` crash-state flag, `tdata/dumps/` archive, annotations,
/// log tail capture, and optional upload configuration (disabled by default).
#[derive(Debug)]
pub struct CrashReportManager {
    working_dir: PathBuf,
    report_path: PathBuf,
    dumps_dir: PathBuf,
    annotations: Mutex<BTreeMap<String, String>>,
    report_file: Mutex<Option<File>>,
    logger: Mutex<Option<Arc<LogManager>>>,
    upload_config: Mutex<CrashUploadConfig>,
    active: AtomicBool,
}

impl CrashReportManager {
    /// Creates a manager rooted at `working_dir` (uses `tdata/working` and `tdata/dumps`).
    #[must_use]
    pub fn new(working_dir: impl Into<PathBuf>) -> Arc<Self> {
        let working_dir = working_dir.into();
        let tdata = working_dir.join("tdata");
        Arc::new(Self {
            report_path: tdata.join("working"),
            dumps_dir: tdata.join("dumps"),
            working_dir,
            annotations: Mutex::new(BTreeMap::new()),
            report_file: Mutex::new(None),
            logger: Mutex::new(None),
            upload_config: Mutex::new(CrashUploadConfig::disabled()),
            active: AtomicBool::new(false),
        })
    }

    /// Path to the active-session / crash-dump state file (`tdata/working`).
    #[must_use]
    pub fn report_path(&self) -> &Path {
        &self.report_path
    }

    /// Path to the archived crash dumps directory (`tdata/dumps`).
    #[must_use]
    pub fn dumps_dir(&self) -> &Path {
        &self.dumps_dir
    }

    /// Attaches a [`LogManager`] whose recent log lines will be included in crash reports.
    ///
    /// If not attached, [`Self::write_crash_report`] falls back to [`LogManager::global`].
    pub fn attach_logger(&self, logger: Arc<LogManager>) {
        if let Ok(mut guard) = self.logger.lock() {
            *guard = Some(logger);
        }
    }

    /// Returns a snapshot of the crash upload configuration (disabled by default).
    #[must_use]
    pub fn upload_config(&self) -> CrashUploadConfig {
        self.upload_config
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Updates the crash upload configuration (remains local-only unless explicitly enabled).
    pub fn set_upload_config(&self, config: CrashUploadConfig) {
        if let Ok(mut guard) = self.upload_config.lock() {
            *guard = config;
        }
    }

    /// Populates standard process annotations (`CrashReports::StartCatching`).
    pub fn init_default_annotations(&self, config: &GlobalConfig, installation_tag: u64) {
        let version_str = default_version_string(config.alpha_version);
        let launched = report_timestamp(&ZonedDateTime::now());

        self.set_annotation("Binary", &config.exe_name);
        self.set_annotation("Version", &version_str);
        self.set_annotation("Launched", &launched);
        self.set_annotation("Platform", Platform::current().crash_report_name());
        self.set_annotation("OS", &os_version_string());
        self.set_annotation("UserTag", &format!("{installation_tag:x}"));
        let _ = fs::create_dir_all(&self.dumps_dir);
    }

    /// Sets or clears (when trimmed value is empty) a crash report annotation
    /// (`CrashReports::SetAnnotation`).
    pub fn set_annotation(&self, key: impl Into<String>, value: &str) {
        if let Ok(mut map) = self.annotations.lock() {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                map.remove(&key.into());
            } else {
                map.insert(key.into(), value.to_owned());
            }
        }
    }

    /// Removes a crash report annotation (`CrashReports::ClearAnnotation`).
    pub fn clear_annotation(&self, key: &str) {
        if let Ok(mut map) = self.annotations.lock() {
            map.remove(key);
        }
    }

    /// Returns a snapshot of current annotations.
    #[must_use]
    pub fn annotations(&self) -> BTreeMap<String, String> {
        self.annotations
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Checks whether a previous launch left `tdata/working` behind (`CrashReports::Start`).
    ///
    /// If `tdata/working` exists, returns [`CrashStartResult::PreviousCrash`] with up to
    /// [`MAX_CRASH_DUMP_BYTES`] so the caller can show a crash/restart prompt and then call
    /// [`Self::restart`]. Otherwise calls [`Self::restart`] immediately.
    pub fn start(&self) -> Result<CrashStartResult, CrashReportError> {
        if let Some(parent) = self.report_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.report_path.exists() {
            let mut f = File::open(&self.report_path)?;
            let mut buf = Vec::new();
            io::Read::by_ref(&mut f)
                .take(MAX_CRASH_DUMP_BYTES as u64)
                .read_to_end(&mut buf)?;
            return Ok(CrashStartResult::PreviousCrash(buf));
        }
        match self.restart() {
            CrashStatus::Started => Ok(CrashStartResult::Started),
            CrashStatus::CantOpen => Err(CrashReportError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "could not open '{}' for writing",
                    self.report_path.display()
                ),
            ))),
        }
    }

    /// Creates/truncates `tdata/working` and marks crash reporting active (`CrashReports::Restart`).
    #[must_use]
    pub fn restart(&self) -> CrashStatus {
        if self.active.load(Ordering::SeqCst) {
            return CrashStatus::Started;
        }
        if let Some(parent) = self.report_path.parent()
            && fs::create_dir_all(parent).is_err()
        {
            return CrashStatus::CantOpen;
        }
        let Ok(file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.report_path)
        else {
            return CrashStatus::CantOpen;
        };
        if let Ok(mut guard) = self.report_file.lock() {
            *guard = Some(file);
        }
        self.active.store(true, Ordering::SeqCst);
        CrashStatus::Started
    }

    /// Cleanly finishes crash reporting by closing and removing `tdata/working`
    /// (`CrashReports::Finish`).
    pub fn finish(&self) {
        self.active.store(false, Ordering::SeqCst);
        if let Ok(mut guard) = self.report_file.lock()
            && let Some(mut file) = guard.take()
        {
            let _ = file.flush();
        }
        let _ = fs::remove_file(&self.report_path);
    }

    /// Formats and writes a plain-text panic crash dump into `tdata/working` and archives a copy
    /// in `tdata/dumps/crash_<timestamp>.txt`. Also flushes the logger.
    pub fn write_crash_dump(&self, reason: &str, location: Option<&str>, backtrace: &str) {
        let _ = self.write_crash_report(CrashKind::Panic, reason, location, backtrace);
    }

    /// Formats and writes a crash report (annotations, version, OS, thread, backtrace, and
    /// trailing log lines) into `tdata/working` and archives a copy in `tdata/dumps/`.
    ///
    /// Returns the archived dump path inside `tdata/dumps/` if written successfully.
    pub fn write_crash_report(
        &self,
        kind: CrashKind,
        reason: &str,
        location: Option<&str>,
        backtrace: &str,
    ) -> Option<PathBuf> {
        let attached_logger = self.logger.try_lock().ok().and_then(|g| g.clone());
        if let Some(ref logger) = attached_logger {
            logger.flush_nonblocking();
        }
        logs::flush_for_crash();

        let last_logs = if let Some(ref logger) = attached_logger {
            logger.recent_lines(DEFAULT_CRASH_LOG_TAIL_LINES)
        } else {
            logs::recent_lines_for_crash(DEFAULT_CRASH_LOG_TAIL_LINES)
        };

        let report = self.format_report(kind, reason, location, backtrace, &last_logs);

        // Write into the active `tdata/working` file so next launch detects the crash.
        let wrote_open = if let Ok(mut guard) = self.report_file.try_lock() {
            if let Some(file) = guard.as_mut() {
                file.write_all(report.as_bytes()).is_ok() && file.flush().is_ok()
            } else {
                false
            }
        } else {
            false
        };
        if !wrote_open {
            if let Some(parent) = self.report_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&self.report_path, report.as_bytes());
        }

        // Archive a timestamped copy under `tdata/dumps/`.
        if fs::create_dir_all(&self.dumps_dir).is_ok() {
            let ts = dump_file_timestamp(&ZonedDateTime::now());
            let seq = DUMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let archive_path = self.dumps_dir.join(format!("crash_{ts}_{seq}.txt"));
            if fs::write(&archive_path, report.as_bytes()).is_ok() {
                return Some(archive_path);
            }
        }
        None
    }

    fn format_report(
        &self,
        kind: CrashKind,
        reason: &str,
        location: Option<&str>,
        backtrace: &str,
        last_logs: &[String],
    ) -> String {
        let mut report = String::with_capacity(4096);
        let mut has_version = false;
        let mut has_os = false;

        if let Ok(map) = self.annotations.try_lock() {
            for (k, v) in map.iter() {
                if k == "Version" {
                    has_version = true;
                } else if k == "OS" {
                    has_os = true;
                }
                report.push_str(k);
                report.push_str(": ");
                report.push_str(v);
                report.push('\n');
            }
        }
        if !has_version {
            report.push_str("Version: ");
            report.push_str(&default_version_string(0));
            report.push('\n');
        }
        if !has_os {
            report.push_str("OS: ");
            report.push_str(&os_version_string());
            report.push('\n');
        }

        report.push_str("Thread: ");
        report.push_str(&current_thread_description());
        report.push_str("\nCrashedAt: ");
        report.push_str(&report_timestamp(&ZonedDateTime::now()));
        report.push_str("\nWorkingDir: ");
        report.push_str(&self.working_dir.to_string_lossy());

        match kind {
            CrashKind::Panic => report.push_str("\n\nPanic: "),
            CrashKind::WindowsException => report.push_str("\n\nException: "),
        }
        report.push_str(reason);
        report.push('\n');

        if let Some(loc) = location {
            report.push_str("Location: ");
            report.push_str(loc);
            report.push('\n');
        }
        if !backtrace.is_empty() {
            report.push_str("\nBacktrace:\n");
            report.push_str(backtrace);
            if !backtrace.ends_with('\n') {
                report.push('\n');
            }
        }
        if !last_logs.is_empty() {
            report.push_str("\nLastLogLines:\n");
            for line in last_logs {
                report.push_str(line);
                report.push('\n');
            }
        }
        report
    }
}

/// `dd.MM.yyyy hh:mm:ss` local time for the `Launched` / `CrashedAt` report fields.
fn report_timestamp(now: &ZonedDateTime) -> String {
    now.strftime("%d.%m.%Y %H:%M:%S").to_string()
}

/// `yyyyMMdd_hhmmss_zzz` local time in archived dump file names.
fn dump_file_timestamp(now: &ZonedDateTime) -> String {
    now.strftime("%Y%m%d_%H%M%S_%3f").to_string()
}

fn default_version_string(alpha_version: u64) -> String {
    if alpha_version != 0 {
        format!("{alpha_version} alpha")
    } else if APP_BETA_VERSION {
        format!("{APP_VERSION} ({APP_VERSION_STR}) beta")
    } else {
        format!("{APP_VERSION} ({APP_VERSION_STR})")
    }
}

/// Returns a concise host OS and architecture description for crash reports.
#[must_use]
pub fn os_version_string() -> String {
    #[cfg(windows)]
    {
        if let Some((major, minor, build)) = win_exception::query_windows_version() {
            return format!(
                "Windows {major}.{minor}.{build} ({}, {})",
                std::env::consts::ARCH,
                Platform::current().crash_report_name()
            );
        }
    }
    format!(
        "{} ({}, {})",
        std::env::consts::OS,
        std::env::consts::ARCH,
        Platform::current().crash_report_name()
    )
}

/// Describes the currently executing thread (name, Rust `ThreadId`, and OS thread ID on Windows).
#[must_use]
pub fn current_thread_description() -> String {
    let current = thread::current();
    let name = current.name().unwrap_or("<unnamed>");
    let id = current.id();
    #[cfg(windows)]
    {
        let os_tid = win_exception::current_os_thread_id();
        format!("{name} ({id:?}, tid={os_tid})")
    }
    #[cfg(not(windows))]
    {
        format!("{name} ({id:?})")
    }
}

/// Returns the canonical Win32 exception name for common SEH status codes.
#[must_use]
pub const fn windows_exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
        0xC000_008C => "EXCEPTION_ARRAY_BOUNDS_EXCEEDED",
        0x8000_0003 => "EXCEPTION_BREAKPOINT",
        0x8000_0002 => "EXCEPTION_DATATYPE_MISALIGNMENT",
        0xC000_008E => "EXCEPTION_FLT_DIVIDE_BY_ZERO",
        0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION",
        0xC000_0006 => "EXCEPTION_IN_PAGE_ERROR",
        0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_0095 => "EXCEPTION_INT_OVERFLOW",
        0xC000_0096 => "EXCEPTION_PRIV_INSTRUCTION",
        0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
        _ => "EXCEPTION_UNKNOWN",
    }
}

/// Formats a Windows SEH exception record into a human-readable summary line.
#[must_use]
pub fn format_windows_exception(code: u32, address: usize, flags: u32) -> String {
    let name = windows_exception_name(code);
    format!("{name} (0x{code:08X}) at 0x{address:016X} (flags=0x{flags:08X})")
}

/// Extracts a human-readable panic message from [`PanicHookInfo`].
fn panic_payload_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

/// Installs a global Rust panic hook that flushes logs, writes a text crash dump to
/// `<working_dir>/tdata/working` and `<working_dir>/tdata/dumps/` (leaving the working flag
/// in place so the next launch detects the crash), and chains to the previous panic hook.
pub fn install_panic_hook(manager: Arc<CrashReportManager>) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let message = panic_payload_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let bt = Backtrace::force_capture().to_string();
        let _ = manager.write_crash_report(CrashKind::Panic, &message, location.as_deref(), &bt);
        previous(info);
    }));
}

/// Installs a Windows top-level unhandled exception filter (`SetUnhandledExceptionFilter`)
/// that records native SEH crashes to `<working_dir>/tdata/working` and `tdata/dumps/`.
///
/// No-op on non-Windows targets.
pub fn install_unhandled_exception_filter(manager: Arc<CrashReportManager>) {
    #[cfg(windows)]
    win_exception::install(manager);
    #[cfg(not(windows))]
    let _ = manager;
}

/// Installs both the Rust panic hook and the Windows unhandled exception filter.
pub fn install_crash_handlers(manager: Arc<CrashReportManager>) {
    install_panic_hook(Arc::clone(&manager));
    install_unhandled_exception_filter(manager);
}

#[cfg(windows)]
mod win_exception {
    use std::backtrace::Backtrace;
    use std::mem::size_of;
    use std::panic;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER, SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;

    use super::{CrashKind, CrashReportManager, format_windows_exception};

    /// Win32 SEH filter return value (`excpt.h`: `EXCEPTION_CONTINUE_SEARCH = 0`).
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    static ACTIVE_MANAGER: Mutex<Option<Arc<CrashReportManager>>> = Mutex::new(None);
    static PREVIOUS_FILTER: Mutex<LPTOP_LEVEL_EXCEPTION_FILTER> = Mutex::new(None);
    static REPORTING_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

    pub(super) fn current_os_thread_id() -> u32 {
        // SAFETY: `GetCurrentThreadId` takes no parameters and always returns the calling thread ID.
        unsafe { GetCurrentThreadId() }
    }

    pub(super) fn query_windows_version() -> Option<(u32, u32, u32)> {
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
            dwMajorVersion: 0,
            dwMinorVersion: 0,
            dwBuildNumber: 0,
            dwPlatformId: 0,
            szCSDVersion: [0; 128],
        };
        // SAFETY: `info` is a properly sized, initialized stack `OSVERSIONINFOW` struct.
        let ok = unsafe { GetVersionExW(&mut info) };
        if ok != 0 {
            Some((info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber))
        } else {
            None
        }
    }

    pub(super) fn install(manager: Arc<CrashReportManager>) {
        if let Ok(mut guard) = ACTIVE_MANAGER.lock() {
            *guard = Some(manager);
        }
        // SAFETY: `unhandled_exception_filter` has the exact `unsafe extern "system"` signature
        // required by Win32 `LPTOP_LEVEL_EXCEPTION_FILTER` and never unwinds across FFI.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(unhandled_exception_filter)) };
        if let Ok(mut prev_guard) = PREVIOUS_FILTER.lock() {
            // Avoid chaining to ourselves if `install` is called more than once.
            let is_self = previous
                .is_some_and(|f| f as usize == unhandled_exception_filter as *const () as usize);
            if !is_self {
                *prev_guard = previous;
            }
        }
    }

    /// Win32 top-level unhandled SEH exception callback.
    ///
    /// # Safety
    /// Invoked by the Windows SEH dispatcher when an exception goes unhandled. `exception_info`
    /// may be null or point to a valid `EXCEPTION_POINTERS` structure for the faulting thread.
    unsafe extern "system" fn unhandled_exception_filter(
        exception_info: *const EXCEPTION_POINTERS,
    ) -> i32 {
        if REPORTING_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        let _ = panic::catch_unwind(|| {
            let (code, address, flags) = if !exception_info.is_null() {
                // SAFETY: Checked `exception_info` is non-null; the Windows SEH dispatcher
                // guarantees `EXCEPTION_POINTERS` is readable for the duration of the filter.
                let record_ptr = unsafe { (*exception_info).ExceptionRecord };
                if !record_ptr.is_null() {
                    // SAFETY: Checked `record_ptr` is non-null; it points to the faulting thread's
                    // `EXCEPTION_RECORD` owned by the OS exception dispatcher.
                    let record = unsafe { &*record_ptr };
                    (
                        record.ExceptionCode as u32,
                        record.ExceptionAddress as usize,
                        record.ExceptionFlags,
                    )
                } else {
                    (0, 0, 0)
                }
            } else {
                (0, 0, 0)
            };

            let reason = format_windows_exception(code, address, flags);
            let bt = Backtrace::force_capture().to_string();

            if let Ok(guard) = ACTIVE_MANAGER.try_lock()
                && let Some(ref manager) = *guard
            {
                let _ = manager.write_crash_report(CrashKind::WindowsException, &reason, None, &bt);
            }
        });

        REPORTING_IN_PROGRESS.store(false, Ordering::SeqCst);

        if let Ok(prev_guard) = PREVIOUS_FILTER.try_lock()
            && let Some(prev) = *prev_guard
        {
            // SAFETY: `prev` is the valid `LPTOP_LEVEL_EXCEPTION_FILTER` returned by
            // `SetUnhandledExceptionFilter` when our handler was registered.
            return unsafe { prev(exception_info) };
        }

        EXCEPTION_CONTINUE_SEARCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// TGB-3019: the chrono -> jiff switch keeps the report and dump-name formats byte for byte.
    #[test]
    fn report_and_dump_timestamps_keep_their_format() {
        let at: ZonedDateTime = "2026-03-07T04:05:06.789123456+02:00[+02:00]"
            .parse()
            .expect("fixed zoned literal");
        assert_eq!(report_timestamp(&at), "07.03.2026 04:05:06");
        assert_eq!(dump_file_timestamp(&at), "20260307_040506_789");
        let whole: ZonedDateTime = "2026-12-31T23:59:59+00:00[UTC]"
            .parse()
            .expect("fixed zoned literal");
        assert_eq!(dump_file_timestamp(&whole), "20261231_235959_000");
    }

    #[test]
    fn clean_exit_vs_crash_detection_lifecycle() {
        let dir = TempDir::new().unwrap();
        let mgr = CrashReportManager::new(dir.path());
        let cfg = GlobalConfig::default();
        mgr.init_default_annotations(&cfg, 0x1234_ABCD);
        mgr.set_annotation("Username", "tester");

        // Upload is disabled by default (no network upload).
        let upload = mgr.upload_config();
        assert!(!upload.enabled);
        assert!(upload.endpoint.is_none());
        assert!(!upload.is_active());

        // First launch: clean start creates tdata/working.
        assert_eq!(mgr.start().unwrap(), CrashStartResult::Started);
        assert!(mgr.report_path().exists());

        // Clean shutdown removes tdata/working.
        mgr.finish();
        assert!(!mgr.report_path().exists());

        // Second launch starts cleanly, then simulates a panic dump without calling finish().
        assert_eq!(mgr.start().unwrap(), CrashStartResult::Started);
        mgr.write_crash_dump("boom!", Some("src/main.rs:42:5"), "frame 0\nframe 1");
        drop(mgr);

        // Third launch detects the previous crash and returns the text dump.
        let mgr2 = CrashReportManager::new(dir.path());
        match mgr2.start().unwrap() {
            CrashStartResult::PreviousCrash(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                assert!(text.contains("Username: tester"));
                assert!(text.contains("UserTag: 1234abcd"));
                assert!(text.contains("Version: "));
                assert!(text.contains("OS: "));
                assert!(text.contains("Thread: "));
                assert!(text.contains("Panic: boom!"));
                assert!(text.contains("Location: src/main.rs:42:5"));
                assert!(text.contains("frame 0"));
            }
            CrashStartResult::Started => panic!("expected PreviousCrash"),
        }

        // Archived copy was created in tdata/dumps/.
        let dump_files: Vec<_> = fs::read_dir(mgr2.dumps_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(dump_files.len(), 1);

        // Restarting after acknowledging the crash truncates tdata/working for the new session.
        assert_eq!(mgr2.restart(), CrashStatus::Started);
        assert_eq!(fs::read(mgr2.report_path()).unwrap().len(), 0);
        mgr2.finish();
        assert!(!mgr2.report_path().exists());
    }

    #[test]
    fn crash_report_includes_version_os_thread_backtrace_and_last_log_lines() {
        let dir = TempDir::new().unwrap();
        let logger = LogManager::new();
        logger.start(dir.path()).unwrap();
        logger.write_main("Startup step one");
        logger.write_main("Connecting to datacenter");
        logger.write_main("Fatal invariant violated");

        let mgr = CrashReportManager::new(dir.path());
        mgr.attach_logger(Arc::clone(&logger));
        assert_eq!(mgr.start().unwrap(), CrashStartResult::Started);

        let archive_path = mgr
            .write_crash_report(
                CrashKind::WindowsException,
                &format_windows_exception(0xC000_0005, 0x0000_7FF6_1234_5678, 0),
                None,
                "0: rustgram::render\n1: main",
            )
            .expect("archived crash dump written");

        assert!(archive_path.is_file());
        assert_eq!(archive_path.parent(), Some(mgr.dumps_dir()));

        let text = fs::read_to_string(&archive_path).unwrap();
        assert!(text.contains("Version: "));
        assert!(text.contains("OS: "));
        assert!(text.contains("Thread: "));
        assert!(text.contains("Exception: EXCEPTION_ACCESS_VIOLATION (0xC0000005)"));
        assert!(text.contains("0x00007FF612345678"));
        assert!(text.contains("Backtrace:\n0: rustgram::render"));
        assert!(text.contains("LastLogLines:"));
        assert!(text.contains("Startup step one"));
        assert!(text.contains("Connecting to datacenter"));
        assert!(text.contains("Fatal invariant violated"));

        // Next launch detects previous crash with identical contents.
        let next = CrashReportManager::new(dir.path());
        match next.start().unwrap() {
            CrashStartResult::PreviousCrash(bytes) => {
                let working_text = String::from_utf8_lossy(&bytes);
                assert!(working_text.contains("EXCEPTION_ACCESS_VIOLATION"));
                assert!(working_text.contains("Fatal invariant violated"));
            }
            CrashStartResult::Started => panic!("expected PreviousCrash"),
        }
    }

    #[test]
    fn empty_working_marker_is_detected_as_previous_crash() {
        let dir = TempDir::new().unwrap();
        let mgr = CrashReportManager::new(dir.path());
        assert_eq!(mgr.start().unwrap(), CrashStartResult::Started);
        // Drop without calling finish() (simulates hard process kill / power loss).
        drop(mgr);

        let next = CrashReportManager::new(dir.path());
        assert_eq!(
            next.start().unwrap(),
            CrashStartResult::PreviousCrash(vec![])
        );
    }

    #[test]
    fn crash_upload_config_defaults_off_and_requires_enabled_plus_endpoint() {
        let default_cfg = CrashUploadConfig::default();
        assert!(!default_cfg.is_active());

        let endpoint_only = CrashUploadConfig {
            enabled: false,
            endpoint: Some("https://crash.example.invalid/upload".to_owned()),
        };
        assert!(!endpoint_only.is_active());

        let enabled_empty = CrashUploadConfig {
            enabled: true,
            endpoint: Some("   ".to_owned()),
        };
        assert!(!enabled_empty.is_active());

        let active = CrashUploadConfig {
            enabled: true,
            endpoint: Some("https://crash.example.invalid/upload".to_owned()),
        };
        assert!(active.is_active());
    }
}
