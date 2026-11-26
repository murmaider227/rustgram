//! Port of `logs.h` and `logs.cpp`.
//!
//! Implements tdesktop's file logging lifecycle (`log_start0..9.txt` -> `log.txt` promotion on
//! single-instance confirmation, 15-minute `DebugLogs/log_HH_MM.txt` and `DebugLogs/mtp_HH_MM.txt`
//! rotation with `YYYYMMDD` day headers, `DEBUG_LOG` / `MTP_LOG` / `TCP_LOG` categories,
//! pre-start in-memory buffering, crash-time flushing) and exposes it as a
//! [`tracing_subscriber::Layer`].

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use chrono::{Datelike, Local, Timelike};
use thiserror::Error;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Tracing target name for `DEBUG_LOG` entries.
pub const TARGET_DEBUG: &str = "debug";
/// Tracing target name for `MTP_LOG` entries.
pub const TARGET_MTP: &str = "mtp";
/// Tracing target name for `TCP_LOG` entries.
pub const TARGET_TCP: &str = "tcp";

/// Number of `log_start0.txt..log_start9.txt` slots (`logs.cpp:195`).
const START_LOG_SLOTS: usize = 10;
/// Debug/MTP log rotation interval in minutes (`switchEach = 15` in `logs.cpp:250`).
const DEBUG_ROTATE_MINUTES: u32 = 15;
/// Maximum number of recent formatted log lines kept in memory for crash dumps.
const RECENT_LOG_RING_CAPACITY: usize = 64;

// MTP DC ID shift constants from `mtproto/core_types.h` and `mtproto/mtp_instance.h`.
const DC_SHIFT: i32 = 10_000;
const TEMPORARY_MAIN_DC: i32 = 1_000;
const CONFIG_DC_SHIFT: i32 = 0x01;
const LOGOUT_DC_SHIFT: i32 = 0x02;
const UPDATER_DC_SHIFT: i32 = 0x03;
const EXPORT_DC_SHIFT: i32 = 0x04;
const EXPORT_MEDIA_DC_SHIFT: i32 = 0x05;
const GROUP_CALL_STREAM_DC_SHIFT: i32 = 0x06;
const TONCENTER_DC_SHIFT: i32 = 0x08;
const WALLET_SHARE_DC_SHIFT: i32 = 0x09;
const MAX_MEDIA_DC_COUNT: i32 = 0x10;
const BASE_DOWNLOAD_DC_SHIFT: i32 = 0x10;
const BASE_UPLOAD_DC_SHIFT: i32 = 0x20;
const DESTROY_KEY_START_DC_SHIFT: i32 = 0x100;

/// Banner written when reopening an existing debug/MTP bucket for the same calendar day.
pub const NEW_INSTANCE_BANNER: &str = "\
----------------------------------------------------------------\n\
NEW LOGGING INSTANCE STARTED!!!\n\
----------------------------------------------------------------\n";

/// Errors returned by log initialization or promotion.
#[derive(Debug, Error)]
pub enum LogError {
    /// Underlying filesystem I/O error.
    #[error("log I/O error: {0}")]
    Io(#[from] io::Error),
    /// `instance_checked()` was called before `start()`.
    #[error("logger has not been started")]
    NotStarted,
}

/// Log stream category (`LogDataType` in `logs.cpp` + `TCP_LOG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    /// Main application log (`log_startN.txt` / `log.txt`, mirrored to `DebugLogs/log_HH_MM.txt`).
    Main,
    /// Verbose debug log (`DebugLogs/log_HH_MM.txt`, `DEBUG_LOG`).
    Debug,
    /// MTProto wire/session log (`DebugLogs/mtp_HH_MM.txt`, `MTP_LOG`).
    Mtp,
    /// TCP transport log (`TCP_LOG`, written to `DebugLogs/log_HH_MM.txt` with `TCP` prefix).
    Tcp,
}

/// Formats a boolean in tdesktop's log style (`Logs::b`).
#[must_use]
pub const fn format_bool(value: bool) -> &'static str {
    if value { "[TRUE]" } else { "[FALSE]" }
}

/// Formats a byte slice as uppercase space-separated hex (`Logs::mb`).
#[must_use]
pub fn format_memory_buffer(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(bytes.len() * 3 - 1);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Expands a shifted MTP DC ID into tdesktop's human-readable tag (`logs.cpp:528-564`).
#[must_use]
pub fn expand_dc_id(dc: i32) -> String {
    let bare_raw = dc % DC_SHIFT;
    let is_temp = bare_raw >= TEMPORARY_MAIN_DC;
    let bare = if is_temp {
        bare_raw - TEMPORARY_MAIN_DC
    } else {
        bare_raw
    };
    let prefix = if is_temp {
        format!("temporary_{bare}")
    } else {
        bare.to_string()
    };
    let shift = dc / DC_SHIFT;
    match shift {
        0 => format!("{prefix}_main"),
        EXPORT_DC_SHIFT => format!("{prefix}_export"),
        EXPORT_MEDIA_DC_SHIFT => format!("{prefix}_export_download"),
        CONFIG_DC_SHIFT => format!("{prefix}_config_enumeration"),
        LOGOUT_DC_SHIFT => format!("{prefix}_logout_guest"),
        UPDATER_DC_SHIFT => format!("{prefix}_download_update"),
        GROUP_CALL_STREAM_DC_SHIFT => format!("{prefix}_stream"),
        TONCENTER_DC_SHIFT => format!("{prefix}_toncenter"),
        WALLET_SHARE_DC_SHIFT => format!("{prefix}_wallet_share"),
        s if (BASE_DOWNLOAD_DC_SHIFT..BASE_DOWNLOAD_DC_SHIFT + MAX_MEDIA_DC_COUNT).contains(&s) => {
            format!("{prefix}_download{}", s - BASE_DOWNLOAD_DC_SHIFT)
        }
        s if (BASE_UPLOAD_DC_SHIFT..BASE_UPLOAD_DC_SHIFT + MAX_MEDIA_DC_COUNT).contains(&s) => {
            format!("{prefix}_upload{}", s - BASE_UPLOAD_DC_SHIFT)
        }
        s if s >= DESTROY_KEY_START_DC_SHIFT => {
            format!("{prefix}_key_destroyer{}", s - DESTROY_KEY_START_DC_SHIFT)
        }
        s => format!("{prefix}_unknown{s}"),
    }
}

static THREAD_COUNTER: AtomicU64 = AtomicU64::new(0);
static ENTRY_INDEX: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static THREAD_LOG_ID: u64 = THREAD_COUNTER.fetch_add(1, Ordering::Relaxed);
    static WRITING_ENTRY: Cell<bool> = const { Cell::new(false) };
}

/// Returns `true` if the current thread is inside a log write (re-entrancy guard `Logs::WritingEntry`).
#[must_use]
pub fn writing_entry() -> bool {
    WRITING_ENTRY.with(Cell::get)
}

struct WritingScope;

impl WritingScope {
    fn enter() -> Self {
        WRITING_ENTRY.with(|flag| flag.set(true));
        Self
    }
}

impl Drop for WritingScope {
    fn drop(&mut self) {
        WRITING_ENTRY.with(|flag| flag.set(false));
    }
}

/// Formats `[HH:MM:SS.mmm TT-NNNNNNN]` matching `_logsEntryStart()` in `logs.cpp:64-71`.
fn format_debug_entry_start() -> String {
    let now = Local::now();
    let tid = THREAD_LOG_ID.with(|&id| id);
    let idx = ENTRY_INDEX.fetch_add(1, Ordering::Relaxed) + 1;
    let millis = now.timestamp_subsec_millis();
    format!(
        "[{:02}:{:02}:{:02}.{:03} {:02}-{:07}]",
        now.hour(),
        now.minute(),
        now.second(),
        millis,
        tid,
        idx
    )
}

/// Formats `[YYYY.MM.DD HH:MM:SS] msg\n` matching `Logs::writeMain()` in `logs.cpp:501-508`.
fn format_main_line(msg: &str) -> String {
    let now = Local::now();
    format!(
        "[{:04}.{:02}.{:02} {:02}:{:02}:{:02}] {}\n",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        msg
    )
}

#[derive(Debug)]
struct MainFileState {
    file: Option<File>,
    path: Option<PathBuf>,
}

#[derive(Debug)]
struct DebugFilesState {
    debug_file: Option<File>,
    mtp_file: Option<File>,
    current_part: i32,
    current_day: i32,
}

/// Thread-safe tdesktop-compatible log writer supporting startup rotation,
/// 15-minute debug/MTP rotation, and crash-time flushing.
#[derive(Debug)]
pub struct LogManager {
    working_dir: Mutex<PathBuf>,
    debug_enabled: AtomicBool,
    started: AtomicBool,
    instance_checked: AtomicBool,
    start_slot: AtomicI32,
    main: Mutex<MainFileState>,
    debug: Mutex<DebugFilesState>,
    memory_buffer: Mutex<Option<Vec<(LogCategory, String)>>>,
    recent_lines: Mutex<VecDeque<String>>,
}

static GLOBAL_LOGGER: OnceLock<Arc<LogManager>> = OnceLock::new();

impl LogManager {
    /// Creates a new unstarted `LogManager` (entries are buffered in memory until `start()`).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            working_dir: Mutex::new(PathBuf::new()),
            debug_enabled: AtomicBool::new(false),
            started: AtomicBool::new(false),
            instance_checked: AtomicBool::new(false),
            start_slot: AtomicI32::new(-1),
            main: Mutex::new(MainFileState {
                file: None,
                path: None,
            }),
            debug: Mutex::new(DebugFilesState {
                debug_file: None,
                mtp_file: None,
                current_part: -1,
                current_day: -1,
            }),
            memory_buffer: Mutex::new(Some(Vec::new())),
            recent_lines: Mutex::new(VecDeque::with_capacity(RECENT_LOG_RING_CAPACITY)),
        })
    }

    /// Registers `manager` as the global logger used for crash-time flushing.
    pub fn install_global(manager: Arc<Self>) -> bool {
        GLOBAL_LOGGER.set(manager).is_ok()
    }

    /// Returns the globally registered logger, if installed.
    #[must_use]
    pub fn global() -> Option<Arc<Self>> {
        GLOBAL_LOGGER.get().cloned()
    }

    /// Enables or disables `DEBUG_LOG` / `MTP_LOG` / `TCP_LOG` file output.
    pub fn set_debug_enabled(&self, enabled: bool) {
        self.debug_enabled.store(enabled, Ordering::SeqCst);
    }

    /// Returns `true` if debug logging is enabled.
    #[must_use]
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled.load(Ordering::SeqCst)
    }

    /// Returns `true` if `start()` has opened a main startup log file.
    #[must_use]
    pub fn started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Returns `true` if `instance_checked()` has promoted the main log to `log.txt`.
    #[must_use]
    pub fn is_instance_checked(&self) -> bool {
        self.instance_checked.load(Ordering::SeqCst)
    }

    /// Returns the current main log file path (`log_startN.txt` or `log.txt`).
    #[must_use]
    pub fn main_log_path(&self) -> Option<PathBuf> {
        self.main.lock().ok().and_then(|g| g.path.clone())
    }

    /// Opens `log_startN.txt` inside `working_dir` (choosing the first unused slot in `0..10`,
    /// or the oldest by mtime if all 10 exist) and flushes buffered `Main` entries.
    pub fn start(&self, working_dir: &Path) -> Result<PathBuf, LogError> {
        fs::create_dir_all(working_dir)?;
        if let Ok(mut wd) = self.working_dir.lock() {
            *wd = working_dir.to_path_buf();
        }

        let (slot, chosen_path) = pick_start_log_slot(working_dir);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&chosen_path)?;

        if let Ok(mut main) = self.main.lock() {
            main.file = Some(file);
            main.path = Some(chosen_path.clone());
        }
        self.start_slot.store(slot as i32, Ordering::SeqCst);
        self.started.store(true, Ordering::SeqCst);

        // Flush any pre-start Main messages (keeping Debug/Mtp buffered until instance_checked()).
        let buffered_main: Vec<String> = {
            if let Ok(mut guard) = self.memory_buffer.lock() {
                if let Some(buf) = guard.as_mut() {
                    let mut mains = Vec::new();
                    buf.retain(|(cat, msg)| {
                        if *cat == LogCategory::Main {
                            mains.push(msg.clone());
                            false
                        } else {
                            true
                        }
                    });
                    mains
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };
        for line in buffered_main {
            self.write_raw_main(&line);
        }

        self.write_main("Logs started");
        Ok(chosen_path)
    }

    /// Promotes the active `log_startN.txt` to `log.txt`, deletes all other `log_start[0-9].txt`
    /// files, and flushes buffered debug/MTP messages (`Logs::instanceChecked`).
    pub fn instance_checked(&self) -> Result<PathBuf, LogError> {
        if !self.started() {
            return Err(LogError::NotStarted);
        }
        let working_dir = self
            .working_dir
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let target_path = working_dir.join("log.txt");

        let start_path = {
            let mut main = self
                .main
                .lock()
                .map_err(|_| io::Error::other("main log mutex poisoned"))?;
            let Some(start_path) = main.path.clone() else {
                return Err(LogError::NotStarted);
            };
            if let Some(mut f) = main.file.take() {
                let _ = f.flush();
            }
            if target_path.exists() {
                fs::remove_file(&target_path)?;
            }
            if let Err(err) = fs::rename(&start_path, &target_path) {
                // Reopen start file if rename failed.
                if let Ok(f) = OpenOptions::new().append(true).open(&start_path) {
                    main.file = Some(f);
                }
                return Err(LogError::Io(err));
            }
            let reopened = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target_path)?;
            main.file = Some(reopened);
            main.path = Some(target_path.clone());
            start_path
        };

        self.start_slot.store(-1, Ordering::SeqCst);
        self.instance_checked.store(true, Ordering::SeqCst);

        self.write_main(&format!(
            "Moved logging from '{}' to '{}'!",
            start_path.display(),
            target_path.display()
        ));

        // Clean up any leftover log_start[0-9].txt files (logs.cpp:180-188).
        if let Ok(entries) = fs::read_dir(&working_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(name_str) = name.to_str()
                    && is_start_log_filename(name_str)
                {
                    let removed = fs::remove_file(entry.path()).is_ok();
                    self.write_main(&format!(
                        "Old start log '{name_str}' found, deleted: {}",
                        format_bool(removed)
                    ));
                }
            }
        }

        // Flush buffered non-main entries and drop the in-memory buffer.
        let buffered = self
            .memory_buffer
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_default();
        for (cat, msg) in buffered {
            if cat != LogCategory::Main && self.debug_enabled() {
                self.write_raw_debug(cat, &msg);
            }
        }

        if self.debug_enabled() {
            self.write_debug("Debug logs started.");
        }
        Ok(target_path)
    }

    /// Writes a `Main` log line (also mirrored to `DebugLogs` when debug mode is enabled).
    pub fn write_main(&self, message: &str) {
        let line = format_main_line(message);
        self.record_recent_line(&line);
        self.dispatch(LogCategory::Main, &line);
        self.write_debug_inner(message, false);
    }

    /// Writes a `DEBUG_LOG` line (`[HH:MM:SS.mmm TT-NNNNNNN] msg\n`).
    pub fn write_debug(&self, message: &str) {
        self.write_debug_inner(message, true);
    }

    fn write_debug_inner(&self, message: &str, record_in_ring: bool) {
        if !self.debug_enabled() && self.started() {
            return;
        }
        let line = format!("{} {}\n", format_debug_entry_start(), message);
        if record_in_ring && self.debug_enabled() {
            self.record_recent_line(&line);
        }
        self.dispatch(LogCategory::Debug, &line);
    }

    /// Writes a `TCP_LOG` line into the debug log stream.
    pub fn write_tcp(&self, message: &str) {
        if !self.debug_enabled() && self.started() {
            return;
        }
        let line = format!("{} TCP: {}\n", format_debug_entry_start(), message);
        if self.debug_enabled() {
            self.record_recent_line(&line);
        }
        self.dispatch(LogCategory::Tcp, &line);
    }

    /// Writes an `MTP_LOG` line (`[HH:MM:SS.mmm TT-NNNNNNN] (dc:<expanded>) msg\n`).
    pub fn write_mtp(&self, dc: i32, message: &str) {
        if !self.debug_enabled() && self.started() {
            return;
        }
        let expanded = expand_dc_id(dc);
        let line = format!(
            "{} (dc:{}) {}\n",
            format_debug_entry_start(),
            expanded,
            message
        );
        if self.debug_enabled() {
            self.record_recent_line(&line);
        }
        self.dispatch(LogCategory::Mtp, &line);
    }

    /// Returns up to `max_lines` most recent log lines without blocking (crash-safe).
    #[must_use]
    pub fn recent_lines(&self, max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }
        if let Ok(ring) = self.recent_lines.try_lock() {
            let skip = ring.len().saturating_sub(max_lines);
            return ring.iter().skip(skip).cloned().collect();
        }
        Vec::new()
    }

    fn record_recent_line(&self, line: &str) {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return;
        }
        if let Ok(mut ring) = self.recent_lines.try_lock() {
            if ring.len() >= RECENT_LOG_RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(trimmed.to_owned());
        }
    }

    /// Reads back the full text of the current main log (`Logs::full()`).
    #[must_use]
    pub fn full_main_log(&self) -> String {
        if let Some(path) = self.main_log_path()
            && let Ok(text) = fs::read_to_string(path)
        {
            return text;
        }
        if let Ok(guard) = self.memory_buffer.lock()
            && let Some(buf) = guard.as_ref()
        {
            let mut out = String::new();
            for (cat, msg) in buf {
                if *cat == LogCategory::Main {
                    out.push_str(msg);
                }
            }
            return out;
        }
        String::new()
    }

    /// Flushes all open log files to OS buffers (safe to call during crash/panic handling).
    pub fn flush_all(&self) {
        let _scope = WritingScope::enter();
        if let Ok(mut main) = self.main.lock()
            && let Some(file) = main.file.as_mut()
        {
            let _ = file.flush();
        }
        if let Ok(mut debug) = self.debug.lock() {
            if let Some(file) = debug.debug_file.as_mut() {
                let _ = file.flush();
            }
            if let Some(file) = debug.mtp_file.as_mut() {
                let _ = file.flush();
            }
        }
    }

    /// Flushes open log files using non-blocking `try_lock()` so panic/SEH crash handlers
    /// can never deadlock if the crashing thread held a log mutex.
    pub fn flush_nonblocking(&self) {
        let _scope = WritingScope::enter();
        if let Ok(mut main) = self.main.try_lock()
            && let Some(file) = main.file.as_mut()
        {
            let _ = file.flush();
        }
        if let Ok(mut debug) = self.debug.try_lock() {
            if let Some(file) = debug.debug_file.as_mut() {
                let _ = file.flush();
            }
            if let Some(file) = debug.mtp_file.as_mut() {
                let _ = file.flush();
            }
        }
    }

    /// Explicitly closes the main log file before relaunching (`Logs::closeMain`).
    pub fn close_main(&self) {
        self.write_main("Explicitly closing main log and finishing crash handlers.");
        let _scope = WritingScope::enter();
        if let Ok(mut main) = self.main.lock()
            && let Some(mut file) = main.file.take()
        {
            let _ = file.flush();
        }
    }

    /// Closes all open log files (`Logs::finish`).
    pub fn finish(&self) {
        self.flush_all();
        if let Ok(mut main) = self.main.lock() {
            main.file = None;
        }
        if let Ok(mut debug) = self.debug.lock() {
            debug.debug_file = None;
            debug.mtp_file = None;
        }
        if let Ok(mut buf) = self.memory_buffer.lock() {
            *buf = None;
        }
        self.started.store(false, Ordering::SeqCst);
    }

    fn dispatch(&self, category: LogCategory, formatted_line: &str) {
        let ready_for_disk =
            self.started() && (category == LogCategory::Main || self.is_instance_checked());
        if ready_for_disk {
            match category {
                LogCategory::Main => self.write_raw_main(formatted_line),
                LogCategory::Debug | LogCategory::Tcp | LogCategory::Mtp => {
                    if self.debug_enabled() {
                        self.write_raw_debug(category, formatted_line);
                    }
                }
            }
        } else if let Ok(mut guard) = self.memory_buffer.lock()
            && let Some(buf) = guard.as_mut()
        {
            buf.push((category, formatted_line.to_owned()));
        }
    }

    fn write_raw_main(&self, line: &str) {
        let _scope = WritingScope::enter();
        if let Ok(mut main) = self.main.lock()
            && let Some(file) = main.file.as_mut()
        {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    fn write_raw_debug(&self, category: LogCategory, line: &str) {
        let _scope = WritingScope::enter();
        let now = Local::now();
        let day_index = now.year() * 10_000 + (now.month() as i32) * 100 + (now.day() as i32);
        let part = ((now.hour() * 60 + now.minute()) / DEBUG_ROTATE_MINUTES) as i32;
        self.write_raw_debug_at(category, line, day_index, part);
    }

    /// Writes to the debug/MTP bucket for an explicit `(day_index, part)` (testable rotation seam).
    pub(crate) fn write_raw_debug_at(
        &self,
        category: LogCategory,
        line: &str,
        day_index: i32,
        part: i32,
    ) {
        let working_dir = self
            .working_dir
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if let Ok(mut state) = self.debug.lock() {
            if state.current_part != part || state.current_day != day_index {
                state.current_part = part;
                state.current_day = day_index;
                let total_minutes = (part as u32) * DEBUG_ROTATE_MINUTES;
                let postfix = format!("_{:02}_{:02}", total_minutes / 60, total_minutes % 60);
                let debug_dir = working_dir.join("DebugLogs");
                let _ = fs::create_dir_all(&debug_dir);

                let debug_path = debug_dir.join(format!("log{postfix}.txt"));
                let mtp_path = debug_dir.join(format!("mtp{postfix}.txt"));
                state.debug_file = open_rotating_debug_file(&debug_path, day_index).ok();
                state.mtp_file = open_rotating_debug_file(&mtp_path, day_index).ok();
            }

            let target = match category {
                LogCategory::Mtp => state.mtp_file.as_mut(),
                LogCategory::Debug | LogCategory::Tcp | LogCategory::Main => {
                    state.debug_file.as_mut()
                }
            };
            if let Some(file) = target {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }
}

/// Flushes the globally installed [`LogManager`], if any (intended for panic/crash hooks).
pub fn flush_for_crash() {
    if let Some(logger) = LogManager::global() {
        logger.flush_nonblocking();
    }
}

/// Returns up to `max_lines` recent log lines from the globally installed [`LogManager`]
/// without blocking (intended for crash report generation).
#[must_use]
pub fn recent_lines_for_crash(max_lines: usize) -> Vec<String> {
    LogManager::global()
        .map(|logger| logger.recent_lines(max_lines))
        .unwrap_or_default()
}

/// Picks the first non-existent `log_start0.txt..log_start9.txt`, or the oldest by mtime.
fn pick_start_log_slot(working_dir: &Path) -> (usize, PathBuf) {
    let mut oldest_idx = 0usize;
    let mut oldest_time: Option<SystemTime> = None;

    for i in 0..START_LOG_SLOTS {
        let candidate = working_dir.join(format!("log_start{i}.txt"));
        if !candidate.exists() {
            return (i, candidate);
        }
        let mtime = fs::metadata(&candidate)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if oldest_time.is_none_or(|prev| mtime < prev) {
            oldest_time = Some(mtime);
            oldest_idx = i;
        }
    }

    (
        oldest_idx,
        working_dir.join(format!("log_start{oldest_idx}.txt")),
    )
}

/// Returns `true` for exact `log_start[0-9].txt` filenames (`logs.cpp:183-185`).
fn is_start_log_filename(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("log_start") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 5 && bytes[0].is_ascii_digit() && &bytes[1..] == b".txt"
}

/// Opens a 15-minute debug/MTP file, appending with `NEW_INSTANCE_BANNER` if the file already
/// starts with today's `day_index`, or truncating and writing `<day_index>\n` otherwise.
fn open_rotating_debug_file(path: &Path, day_index: i32) -> io::Result<File> {
    let mut same_day = false;
    if path.exists()
        && let Ok(existing) = File::open(path)
    {
        let mut reader = BufReader::new(existing);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok()
            && first_line.trim().parse::<i32>().ok() == Some(day_index)
        {
            same_day = true;
        }
    }

    let mut file = if same_day {
        OpenOptions::new().create(true).append(true).open(path)?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?
    };

    if same_day {
        file.write_all(NEW_INSTANCE_BANNER.as_bytes())?;
    } else {
        writeln!(file, "{day_index}")?;
    }
    file.flush()?;
    Ok(file)
}

/// `tracing_subscriber::Layer` routing `tracing` events into a [`LogManager`] in tdesktop format.
#[derive(Debug, Clone)]
pub struct TdLogLayer {
    manager: Arc<LogManager>,
}

impl TdLogLayer {
    /// Creates a new tracing layer backed by `manager`.
    #[must_use]
    pub fn new(manager: Arc<LogManager>) -> Self {
        Self { manager }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    dc: Option<i32>,
    extras: Vec<(String, String)>,
}

impl Visit for MessageVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "dc" {
            self.dc = Some(value as i32);
        } else {
            self.extras
                .push((field.name().to_owned(), value.to_string()));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "dc" {
            self.dc = Some(value as i32);
        } else {
            self.extras
                .push((field.name().to_owned(), value.to_string()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        } else {
            self.extras
                .push((field.name().to_owned(), value.to_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.extras
                .push((field.name().to_owned(), format!("{value:?}")));
        }
    }
}

impl MessageVisitor {
    fn finish(self) -> (String, Option<i32>) {
        let mut out = self.message.unwrap_or_default();
        for (k, v) in self.extras {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&k);
            out.push('=');
            out.push_str(&v);
        }
        (out, self.dc)
    }
}

impl<S: Subscriber> Layer<S> for TdLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if writing_entry() {
            return;
        }
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let (msg, dc) = visitor.finish();

        let target = meta.target();
        if target == TARGET_MTP || target.starts_with("mtp::") {
            self.manager.write_mtp(dc.unwrap_or(0), &msg);
        } else if target == TARGET_TCP || target.starts_with("tcp::") {
            self.manager.write_tcp(&msg);
        } else if target == TARGET_DEBUG
            || target.starts_with("debug::")
            || *meta.level() == Level::DEBUG
            || *meta.level() == Level::TRACE
        {
            self.manager.write_debug(&msg);
        } else {
            let formatted = match *meta.level() {
                Level::ERROR => format!("ERROR: {msg}"),
                Level::WARN => format!("WARNING: {msg}"),
                _ => msg,
            };
            self.manager.write_main(&formatted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt;

    /// TGB-2908: the `saved_side` diagnostics are INFO events with a custom target; they must
    /// land in the main `log.txt` (not the debug-only logs) together with their fields.
    #[test]
    fn info_event_with_custom_target_reaches_main_log_with_fields() {
        let dir = TempDir::new().unwrap();
        let logger = Arc::new(LogManager::new());
        logger.start(dir.path()).unwrap();
        logger.instance_checked().unwrap();
        let subscriber = tracing_subscriber::registry().with(TdLogLayer::new(Arc::clone(&logger)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "saved_side",
                stage = "layout",
                msg_id = 42,
                side = "in",
                "saved messages: layout side"
            );
        });
        let content = logger.full_main_log();
        assert!(content.contains("saved messages: layout side"), "{content}");
        assert!(content.contains("stage=layout"), "{content}");
        assert!(content.contains("side=in"), "{content}");
    }

    #[test]
    fn start_log_promotion_and_cleanup() {
        let dir = TempDir::new().unwrap();
        // Pre-populate stale log_start0.txt and log_start2.txt plus old log.txt.
        fs::write(dir.path().join("log_start0.txt"), "stale0\n").unwrap();
        fs::write(dir.path().join("log_start2.txt"), "stale2\n").unwrap();
        fs::write(dir.path().join("log.txt"), "old main log\n").unwrap();

        let logger = LogManager::new();
        logger.set_debug_enabled(true);
        // Write before start -> buffered in memory.
        logger.write_main("Early message");
        logger.write_debug("Early debug");

        // Should pick log_start1.txt (first unused slot).
        let start_path = logger.start(dir.path()).unwrap();
        assert_eq!(start_path.file_name().unwrap(), "log_start1.txt");
        assert!(start_path.exists());

        let main_path = logger.instance_checked().unwrap();
        assert_eq!(main_path, dir.path().join("log.txt"));
        assert!(!dir.path().join("log_start0.txt").exists());
        assert!(!dir.path().join("log_start1.txt").exists());
        assert!(!dir.path().join("log_start2.txt").exists());

        let content = logger.full_main_log();
        assert!(content.contains("Early message"));
        assert!(content.contains("Logs started"));
        assert!(content.contains("Moved logging from"));
        assert!(content.contains("Old start log 'log_start0.txt' found, deleted: [TRUE]"));
    }

    #[test]
    fn debug_and_mtp_15_minute_rotation_and_same_day_banner() {
        let dir = TempDir::new().unwrap();
        let logger = LogManager::new();
        logger.set_debug_enabled(true);
        logger.start(dir.path()).unwrap();
        logger.instance_checked().unwrap();

        // Part 4 = 01:00, day 20261122
        logger.write_raw_debug_at(
            LogCategory::Debug,
            "[01:00:00.001 00-0000001] first\n",
            20261122,
            4,
        );
        logger.write_raw_debug_at(
            LogCategory::Mtp,
            "[01:00:00.002 00-0000002] (dc:2_main) hello\n",
            20261122,
            4,
        );

        let log_01_00 = dir.path().join("DebugLogs").join("log_01_00.txt");
        let mtp_01_00 = dir.path().join("DebugLogs").join("mtp_01_00.txt");
        assert!(log_01_00.exists());
        assert!(mtp_01_00.exists());
        assert!(
            fs::read_to_string(&log_01_00)
                .unwrap()
                .starts_with("20261122\n")
        );
        assert!(
            fs::read_to_string(&mtp_01_00)
                .unwrap()
                .contains("(dc:2_main) hello")
        );

        // Rotate to part 5 = 01:15
        logger.write_raw_debug_at(
            LogCategory::Debug,
            "[01:15:00.001 00-0000003] second\n",
            20261122,
            5,
        );
        assert!(dir.path().join("DebugLogs").join("log_01_15.txt").exists());

        // Reopen part 4 on same day -> appends NEW_INSTANCE_BANNER instead of truncating.
        logger.write_raw_debug_at(
            LogCategory::Debug,
            "[01:01:00.001 00-0000004] resumed\n",
            20261122,
            4,
        );
        let reopened = fs::read_to_string(&log_01_00).unwrap();
        assert!(reopened.contains("first"));
        assert!(reopened.contains("NEW LOGGING INSTANCE STARTED!!!"));
        assert!(reopened.contains("resumed"));

        // Reopen part 4 on next day -> truncates and writes new day index.
        logger.write_raw_debug_at(
            LogCategory::Debug,
            "[01:00:00.001 00-0000005] nextday\n",
            20261123,
            4,
        );
        let next_day = fs::read_to_string(&log_01_00).unwrap();
        assert!(next_day.starts_with("20261123\n"));
        assert!(!next_day.contains("first"));
    }

    #[test]
    fn tracing_layer_routes_categories() {
        let dir = TempDir::new().unwrap();
        let logger = LogManager::new();
        logger.set_debug_enabled(true);
        logger.start(dir.path()).unwrap();
        logger.instance_checked().unwrap();

        let subscriber = tracing_subscriber::registry().with(TdLogLayer::new(Arc::clone(&logger)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("app booted");
            tracing::warn!("low disk");
            tracing::debug!("verbose detail");
            tracing::info!(target: TARGET_MTP, dc = 2, "sent ping");
            tracing::debug!(target: TARGET_TCP, "connected socket");
        });

        logger.flush_all();
        let main = logger.full_main_log();
        assert!(main.contains("app booted"));
        assert!(main.contains("WARNING: low disk"));
        assert!(!main.contains("verbose detail"));

        let debug_dir = dir.path().join("DebugLogs");
        let files: Vec<String> = fs::read_dir(&debug_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| fs::read_to_string(e.path()).unwrap_or_default())
            .collect();
        let combined = files.join("\n");
        assert!(combined.contains("verbose detail"));
        assert!(combined.contains("(dc:2_main) sent ping"));
        assert!(combined.contains("TCP: connected socket"));
    }

    #[test]
    fn all_ten_start_slots_occupied_reuses_valid_slot() {
        let dir = TempDir::new().unwrap();
        for i in 0..START_LOG_SLOTS {
            fs::write(
                dir.path().join(format!("log_start{i}.txt")),
                format!("slot {i}\n"),
            )
            .unwrap();
        }
        let logger = LogManager::new();
        let start_path = logger.start(dir.path()).unwrap();
        let name = start_path.file_name().unwrap().to_str().unwrap();
        assert!(is_start_log_filename(name));
        let main = logger.instance_checked().unwrap();
        assert_eq!(main, dir.path().join("log.txt"));
        for i in 0..START_LOG_SLOTS {
            assert!(!dir.path().join(format!("log_start{i}.txt")).exists());
        }
    }

    #[test]
    fn expand_dc_id_matches_tdesktop() {
        assert_eq!(expand_dc_id(2), "2_main");
        assert_eq!(
            expand_dc_id(2 + DC_SHIFT * CONFIG_DC_SHIFT),
            "2_config_enumeration"
        );
        assert_eq!(
            expand_dc_id(4 + DC_SHIFT * (BASE_DOWNLOAD_DC_SHIFT + 3)),
            "4_download3"
        );
        assert_eq!(
            expand_dc_id(DC_SHIFT * (BASE_UPLOAD_DC_SHIFT + 1)),
            "0_upload1"
        );
        assert_eq!(expand_dc_id(TEMPORARY_MAIN_DC + 2), "temporary_2_main");
        assert_eq!(format_memory_buffer(&[0x0A, 0xFF, 0x00]), "0A FF 00");
    }
}
