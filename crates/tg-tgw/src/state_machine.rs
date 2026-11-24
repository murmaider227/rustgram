//! Widget lifecycle state machine, CPU throttle meter, kept-picture memory budget, and remote
//! helper circuit breaker.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_runtime.h/.cpp`, `tgw_widget_meter.h/.cpp`,
//! `tgw_kept_budget.h/.cpp`, and `tgw_remote.h/.cpp`.

use std::collections::VecDeque;

use crate::error::{Result, TgwError};
use crate::model::{ContentSafeArea, HeightChange, HeightLayout, ResizeAnchor, WidgetTheme};

/// Maximum simultaneously alive widget runtimes across chats (`kMaxAlive = 8` in `tgw_runtime.cpp:66`).
pub const MAX_ALIVE_WIDGETS: usize = 8;

/// Maximum consecutive crashes on the same content serial before permanent stop (`kMaxCrashes = 2`).
pub const MAX_WIDGET_CRASHES: u32 = 2;

/// Maximum diagnostic note lines retained per widget (`kMaxNotes = 100`).
pub const MAX_LOG_NOTES: usize = 100;

/// Maximum characters per diagnostic note line (`kMaxLogLine = 240`).
pub const MAX_LOG_LINE: usize = 240;

/// Maximum kept offscreen widget picture bytes (`KeptBudget::kPictureLimit = 48 MiB`).
pub const KEPT_PICTURE_LIMIT_BYTES: i64 = 48 * 1024 * 1024;

/// Maximum consecutive helper launch/runtime failures before disabling (`kMaxFailures = 3`).
pub const MAX_HELPER_FAILURES: u32 = 3;

/// Helper uptime threshold in ms that resets consecutive failure count (`kFailureRunAge = 60_000`).
pub const HELPER_FAILURE_RUN_AGE_MS: u64 = 60_000;

/// Helper idle shutdown timeout in ms (`kIdleTimeout = 30_000`).
pub const HELPER_IDLE_TIMEOUT_MS: u64 = 30_000;

/// Re-export of the full CPU/frame/memory watchdog meter (`crate::meter::WidgetMeter`).
pub use crate::meter::WidgetMeter;

/// Lifecycle state of a widget runtime (`Tgw::Runtime::State` in `tgw_runtime.h:80-88`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RuntimeState {
    /// Created but not yet requested to start (or reset after recoverable release).
    #[default]
    Idle,
    /// Waiting for sandbox resources or a free alive slot (`MAX_ALIVE_WIDGETS`).
    Waiting,
    /// Constructing host/renderer asynchronously.
    Constructing,
    /// Running and accepting interactive input and frame updates.
    Live,
    /// Evicted/unloaded while keeping its last rendered still picture if available.
    Released,
    /// Stopped by crash limit, host policy, or renderer stop control.
    Stopped,
    /// Failed to initialize resources or construct the sandbox host.
    Failed,
}

/// Persistent per-message memory across view rebuilds (`Tgw::MessageMemory` in `tgw_runtime.cpp:53-62`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageMemory {
    /// Last known auto-height in CSS pixels.
    pub height: Option<f64>,
    /// Last unpacked HTML bytes (used to detect `A -> B -> A` edits).
    pub html: Vec<u8>,
    /// Monotonic serial incremented whenever `html` changes.
    pub content_serial: u64,
    /// Consecutive crash count for the current `content_serial`.
    pub crashes: u32,
    /// Anchor edge preserved during auto-height transitions.
    pub anchor: ResizeAnchor,
}

impl MessageMemory {
    /// Updates remembered HTML, bumping `content_serial` and clearing `crashes` if changed
    /// (`RememberHtml` in `tgw_runtime.cpp:173-179`).
    pub fn remember_html(&mut self, html: &[u8]) -> u64 {
        if self.html.as_slice() != html {
            self.html = html.to_vec();
            self.content_serial = self.content_serial.saturating_add(1);
            self.crashes = 0;
        }
        self.content_serial
    }
}

/// Deterministic state machine for one sandboxed widget instance (`Tgw::Runtime`).
#[derive(Debug, Clone)]
pub struct WidgetStateMachine {
    state: RuntimeState,
    generation: u64,
    content_serial: u64,
    memory: MessageMemory,
    visible: bool,
    painted: bool,
    has_still: bool,
    stop_reason: Option<String>,
    stop_fatal: bool,
    height_layout: Option<HeightLayout>,
    width: i32,
    max_height: f64,
    safe_area: ContentSafeArea,
    theme: Option<WidgetTheme>,
    notes: VecDeque<String>,
}

impl WidgetStateMachine {
    /// Creates a new state machine in [`RuntimeState::Idle`] for `html`.
    #[must_use]
    pub fn new(html: &[u8]) -> Self {
        let mut memory = MessageMemory::default();
        let content_serial = memory.remember_html(html);
        Self {
            state: RuntimeState::Idle,
            generation: 0,
            content_serial,
            memory,
            visible: false,
            painted: false,
            has_still: false,
            stop_reason: None,
            stop_fatal: false,
            height_layout: None,
            width: 0,
            max_height: 0.0,
            safe_area: ContentSafeArea::default(),
            theme: None,
            notes: VecDeque::new(),
        }
    }

    /// Returns the current [`RuntimeState`].
    #[must_use]
    pub const fn state(&self) -> RuntimeState {
        self.state
    }

    /// Returns the current construction generation counter.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the content serial this instance was built for.
    #[must_use]
    pub const fn content_serial(&self) -> u64 {
        self.content_serial
    }

    /// Returns the crash count recorded on the current content serial.
    #[must_use]
    pub const fn crash_count(&self) -> u32 {
        self.memory.crashes
    }

    /// Returns `true` if the widget has reached [`MAX_WIDGET_CRASHES`].
    #[must_use]
    pub const fn crashed_out(&self) -> bool {
        self.memory.crashes >= MAX_WIDGET_CRASHES
    }

    /// Returns `true` if the widget is stopped (either `Stopped` or `Failed`).
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        matches!(self.state, RuntimeState::Stopped | RuntimeState::Failed)
    }

    /// Returns `true` if the widget is currently live and interactive.
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        matches!(self.state, RuntimeState::Live) && !self.stop_fatal
    }

    /// Returns `true` if this runtime holds heavy resources (`Waiting`/`Constructing`/`Live`/still).
    #[must_use]
    pub const fn is_heavy(&self) -> bool {
        matches!(
            self.state,
            RuntimeState::Waiting | RuntimeState::Constructing | RuntimeState::Live
        ) || self.has_still
    }

    /// Returns the latest stop/failure reason, if any.
    #[must_use]
    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    /// Returns `true` if the stop reason was fatal (no automatic restart on re-visibility).
    #[must_use]
    pub const fn stop_fatal(&self) -> bool {
        self.stop_fatal
    }

    /// Returns the latest [`HeightLayout`], if recorded.
    #[must_use]
    pub const fn height_layout(&self) -> Option<HeightLayout> {
        self.height_layout
    }

    /// Returns the remembered [`MessageMemory`].
    #[must_use]
    pub const fn memory(&self) -> &MessageMemory {
        &self.memory
    }

    /// Returns the recorded diagnostic notes.
    #[must_use]
    pub fn notes(&self) -> &VecDeque<String> {
        &self.notes
    }

    /// Appends a bounded diagnostic note (`addNote` in `tgw_runtime.cpp`).
    pub fn add_note(&mut self, text: impl AsRef<str>) {
        let truncated: String = text.as_ref().chars().take(MAX_LOG_LINE).collect();
        if self.notes.len() >= MAX_LOG_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(truncated);
    }

    /// Marks the widget visible/hidden in the chat viewport.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Marks the widget as painted on screen (needed before starting).
    pub fn mark_painted(&mut self) {
        self.painted = true;
    }

    /// Updates layout width and maximum height.
    pub fn set_size(&mut self, width: i32, max_height: f64) {
        self.width = width;
        self.max_height = max_height;
    }

    /// Updates safe area insets.
    pub fn set_safe_area(&mut self, area: ContentSafeArea) {
        self.safe_area = area;
    }

    /// Updates the theme; returns `true` if the theme actually changed.
    pub fn set_theme(&mut self, theme: WidgetTheme) -> bool {
        if self.theme.as_ref() == Some(&theme) {
            return false;
        }
        self.theme = Some(theme);
        true
    }

    /// Updates widget content HTML; resets crash counter if content changed (`A -> B -> A` safe).
    pub fn update_html(&mut self, html: &[u8]) -> bool {
        let prev = self.content_serial;
        let next = self.memory.remember_html(html);
        if next != prev {
            self.content_serial = next;
            self.stop_reason = None;
            self.stop_fatal = false;
            self.state = RuntimeState::Idle;
            self.generation = self.generation.saturating_add(1);
            self.add_note(format!("content changed to serial {next}"));
            return true;
        }
        false
    }

    /// Returns `true` if this widget is eligible to transition to `Waiting` / `Constructing`.
    #[must_use]
    pub const fn wants_start(&self) -> bool {
        self.visible
            && self.painted
            && !self.crashed_out()
            && matches!(self.state, RuntimeState::Idle | RuntimeState::Released)
    }

    /// Requests starting the widget: `Idle | Released -> Waiting`.
    pub fn request_start(&mut self) -> Result<u64> {
        if self.crashed_out() {
            self.state = RuntimeState::Stopped;
            self.stop_fatal = true;
            return Err(TgwError::InvalidState(
                "widget exceeded maximum crash limit".to_owned(),
            ));
        }
        if !matches!(self.state, RuntimeState::Idle | RuntimeState::Released) {
            return Err(TgwError::InvalidState(format!(
                "cannot request_start from {:?}",
                self.state
            )));
        }
        self.generation = self.generation.saturating_add(1);
        self.state = RuntimeState::Waiting;
        self.add_note(format!("waiting for resources (gen {})", self.generation));
        Ok(self.generation)
    }

    /// Transitions `Waiting -> Constructing` when sandbox resources and an alive slot are ready.
    pub fn begin_construct(&mut self, generation: u64) -> Result<()> {
        if generation != self.generation || self.state != RuntimeState::Waiting {
            return Err(TgwError::InvalidState(format!(
                "begin_construct mismatch: state={:?}, gen={}, expected={}",
                self.state, generation, self.generation
            )));
        }
        self.state = RuntimeState::Constructing;
        self.add_note(format!("constructing widget host (gen {generation})"));
        Ok(())
    }

    /// Transitions `Constructing -> Live` once the sandboxed renderer reports ready.
    pub fn mark_live(&mut self, generation: u64) -> Result<()> {
        if generation != self.generation || self.state != RuntimeState::Constructing {
            return Err(TgwError::InvalidState(format!(
                "mark_live mismatch: state={:?}, gen={}, expected={}",
                self.state, generation, self.generation
            )));
        }
        self.state = RuntimeState::Live;
        self.stop_reason = None;
        self.stop_fatal = false;
        self.add_note("widget live");
        Ok(())
    }

    /// Records a height update reported by the renderer or client resize.
    pub fn record_height(&mut self, height: i32, change: HeightChange, anchor: ResizeAnchor) {
        let serial = self.height_layout.map_or(1, |h| h.serial.saturating_add(1));
        self.memory.height = Some(f64::from(height));
        self.memory.anchor = anchor;
        self.height_layout = Some(HeightLayout {
            serial,
            height,
            change,
            anchor,
        });
    }

    /// Releases an offscreen or evicted live widget while optionally keeping its still frame
    /// (`Runtime::release` in `tgw_runtime.cpp`).
    pub fn release(&mut self, keep_still: bool) {
        self.generation = self.generation.saturating_add(1);
        self.has_still = keep_still;
        if matches!(
            self.state,
            RuntimeState::Waiting | RuntimeState::Constructing | RuntimeState::Live
        ) {
            self.state = RuntimeState::Released;
            self.add_note(if keep_still {
                "released (kept still frame)"
            } else {
                "released"
            });
        }
    }

    /// Unloads heavy state completely (drops still frame too).
    pub fn unload(&mut self) {
        self.visible = false;
        self.painted = false;
        self.release(false);
    }

    /// Handles a runtime stop or crash.
    ///
    /// Mirrors `Runtime::countCrash` / `Runtime::stopForCrashes`:
    /// - If `fatal` is `true`, transitions immediately to [`RuntimeState::Stopped`] with `stop_fatal = true`.
    /// - Otherwise increments crash count; if `crashes >= MAX_WIDGET_CRASHES`, transitions to
    ///   [`RuntimeState::Stopped`] with `stop_fatal = true`; else transitions to [`RuntimeState::Idle`]
    ///   so the scheduler may restart it once.
    pub fn handle_stop(&mut self, reason: impl Into<String>, fatal: bool) {
        let reason = reason.into();
        self.generation = self.generation.saturating_add(1);
        self.stop_reason = Some(reason.clone());
        if fatal {
            self.memory.crashes = MAX_WIDGET_CRASHES;
            self.stop_fatal = true;
            self.state = RuntimeState::Stopped;
            self.add_note(format!("stopped (fatal): {reason}"));
            return;
        }
        self.memory.crashes = self.memory.crashes.saturating_add(1);
        if self.memory.crashes >= MAX_WIDGET_CRASHES {
            self.stop_fatal = true;
            self.state = RuntimeState::Stopped;
            self.add_note(format!(
                "stopped after {} crashes: {reason}",
                self.memory.crashes
            ));
        } else {
            self.stop_fatal = false;
            self.state = RuntimeState::Idle;
            self.add_note(format!(
                "crash #{}, eligible for restart: {reason}",
                self.memory.crashes
            ));
        }
    }

    /// Handles an unrecoverable initialization/resource failure (`Runtime::fail`).
    pub fn fail(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.generation = self.generation.saturating_add(1);
        self.stop_reason = Some(error.clone());
        self.stop_fatal = true;
        self.state = RuntimeState::Failed;
        self.add_note(format!("failed: {error}"));
    }
}

// =============================================================================
// Kept-Picture Memory Budget (`tgw_kept_budget.h/.cpp`)
// =============================================================================

/// One kept still-picture entry for eviction planning (`KeptBudget::Entry` in `tgw_kept_budget.h:82-86`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptPictureEntry {
    /// Backing pixel bytes.
    pub pixels: i64,
    /// Unloaded serial (`0` means currently visible on screen).
    pub unloaded_serial: u64,
    /// Tie-breaking work ID.
    pub work_id: u64,
}

/// Eviction plan computed by [`plan_kept_picture_trim`] (`KeptBudget::Trim`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeptPictureTrim {
    /// Indices into the input slice whose offscreen still frames should be dropped.
    pub drop_indices: Vec<usize>,
    /// Total unloaded offscreen picture bytes before trimming.
    pub unloaded_bytes: i64,
    /// Total visible picture bytes (never evicted).
    pub visible_bytes: i64,
}

/// Plans eviction of oldest unloaded offscreen still pictures until `unloaded_bytes <= 48 MiB`
/// (`KeptBudget::PlanTrim` in `tgw_kept_budget.cpp:125-157`).
#[must_use]
pub fn plan_kept_picture_trim(stills: &[KeptPictureEntry]) -> KeptPictureTrim {
    let mut trim = KeptPictureTrim::default();
    let mut candidates = Vec::new();
    for (idx, entry) in stills.iter().enumerate() {
        let pixels = entry.pixels.max(0);
        if entry.unloaded_serial == 0 {
            trim.visible_bytes = trim.visible_bytes.saturating_add(pixels);
        } else if pixels > 0 {
            trim.unloaded_bytes = trim.unloaded_bytes.saturating_add(pixels);
            candidates.push(idx);
        }
    }
    let mut total = trim.unloaded_bytes;
    if total <= KEPT_PICTURE_LIMIT_BYTES {
        return trim;
    }
    candidates.sort_by(|&a, &b| {
        let left = &stills[a];
        let right = &stills[b];
        left.unloaded_serial
            .cmp(&right.unloaded_serial)
            .then_with(|| left.work_id.cmp(&right.work_id))
    });
    for idx in candidates {
        trim.drop_indices.push(idx);
        total = total.saturating_sub(stills[idx].pixels.max(0));
        if total <= KEPT_PICTURE_LIMIT_BYTES {
            break;
        }
    }
    trim
}

// =============================================================================
// Remote Helper Placement & Failure Circuit Breaker (`tgw_remote.h/.cpp`)
// =============================================================================

/// Where widget engines run (`Tgw::EnginePlace` in `tgw_remote.h:17-21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnginePlace {
    /// In-process (debug/test mode only).
    InProcess,
    /// Out-of-process sandboxed helper / WebView2 process.
    Helper,
    /// Disabled / unsupported on this OS version.
    Nowhere,
}

/// Tracks helper process launch failures and automatic disable threshold (`tgw_remote.cpp:449-457`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HelperCircuitBreaker {
    failures: u32,
    disabled_reason: Option<String>,
}

impl HelperCircuitBreaker {
    /// Creates an enabled circuit breaker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an unexpected helper failure after `uptime_ms`.
    pub fn record_failure(&mut self, uptime_ms: u64) {
        self.failures = if uptime_ms >= HELPER_FAILURE_RUN_AGE_MS {
            1
        } else {
            self.failures.saturating_add(1)
        };
        if self.failures >= MAX_HELPER_FAILURES && self.disabled_reason.is_none() {
            self.disabled_reason = Some(format!(
                "the widget helper failed {} times in a row",
                self.failures
            ));
        }
    }

    /// Resets failure counter after a clean intentional shutdown.
    pub fn record_clean_shutdown(&mut self) {
        self.failures = 0;
    }

    /// Permanently disables helper starts if a child process could not be reaped.
    pub fn mark_unreaped(&mut self) {
        if self.disabled_reason.is_none() {
            self.disabled_reason = Some("the widget helper was not reaped".to_owned());
        }
    }

    /// Returns `Some(reason)` if the helper has been disabled by consecutive failures.
    #[must_use]
    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }

    /// Returns current consecutive failure count.
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }
}
