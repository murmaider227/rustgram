//! Frame-time, CPU-budget, and memory watchdog emitting throttle/suspend verdicts for sandboxed
//! HTML widgets.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_widget_meter.h/.cpp` and `tgw_painter.h`.

use std::collections::VecDeque;

use crate::error::{Result, TgwError};
use crate::wire::ControlMessage;

/// Free CPU time per pass before throttling applies (`kThrottleFree = 0.016` s = 16 ms).
pub const METER_THROTTLE_FREE_SECS: f64 = 0.016;

/// Single-pass throttle hold multiplier (`kThrottleFactor = 3.0`).
pub const METER_THROTTLE_FACTOR: f64 = 3.0;

/// Sustained overload throttle hold multiplier (`kSustainedFactor = 19.0`).
pub const METER_SUSTAINED_FACTOR: f64 = 19.0;

/// Rolling CPU measurement window in seconds (`kSustainedWindow = 2.0` s).
pub const METER_SUSTAINED_WINDOW_SECS: f64 = 2.0;

/// Sustained CPU duty-cycle threshold (`kSustainedLimit = 0.1` = 10% over 2 s).
pub const METER_SUSTAINED_LIMIT_RATIO: f64 = 0.1;

/// Heavy single-pass threshold in seconds (`kHeavyPass = 0.05` s = 50 ms).
pub const METER_HEAVY_PASS_SECS: f64 = 0.05;

/// Initial load grace period during which CPU charges do not throttle (`kLoadGrace = 1.0` s).
pub const METER_LOAD_GRACE_SECS: f64 = 1.0;

/// Immediate-response throttle exemption window after user input (`kInputImmediate = 0.25` s).
pub const METER_INPUT_IMMEDIATE_SECS: f64 = 0.25;

/// Default hard single-frame raster timeout triggering immediate suspension (`kMaxRasterTime = 1.0` s).
pub const DEFAULT_SINGLE_FRAME_SUSPEND_SECS: f64 = 1.0;

/// Default consecutive slow frames (`> METER_HEAVY_PASS_SECS`) before suspending the widget.
pub const DEFAULT_MAX_CONSECUTIVE_SLOW_FRAMES: u32 = 8;

/// Default accumulated throttle hold ceiling in ms before suspending the widget.
pub const DEFAULT_SUSPEND_HOLD_MS: u64 = 5_000;

/// Default memory soft pressure threshold (64 MiB — matches per-backend canvas limit).
pub const DEFAULT_MEMORY_SOFT_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// Default memory hard ceiling triggering suspension (128 MiB — matches total canvas limit).
pub const DEFAULT_MEMORY_HARD_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// Default hold duration in ms applied when memory crosses the soft threshold.
pub const DEFAULT_MEMORY_PRESSURE_HOLD_MS: u64 = 250;

/// Tunable watchdog budget thresholds for [`WidgetMeter`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterConfig {
    /// Free per-pass CPU time in seconds (default `0.016`).
    pub throttle_free_secs: f64,
    /// Single over-budget pass hold multiplier (default `3.0`).
    pub throttle_factor: f64,
    /// Sustained overload hold multiplier (default `19.0`).
    pub sustained_factor: f64,
    /// Rolling window duration in seconds (default `2.0`).
    pub sustained_window_secs: f64,
    /// Sustained CPU ratio over `sustained_window_secs` (default `0.10`).
    pub sustained_limit_ratio: f64,
    /// Heavy pass threshold in seconds (default `0.050`).
    pub heavy_pass_secs: f64,
    /// Load grace period in seconds (default `1.0`).
    pub load_grace_secs: f64,
    /// Post-input throttle exemption in seconds (default `0.25`).
    pub input_immediate_secs: f64,
    /// Hard single-frame timeout in seconds triggering [`MeterVerdict::Suspend`] (default `1.0`).
    pub single_frame_suspend_secs: f64,
    /// Consecutive slow frames (`> heavy_pass_secs`) triggering [`MeterVerdict::Suspend`] (default `8`).
    pub max_consecutive_slow_frames: u32,
    /// Accumulated hold ceiling in ms triggering [`MeterVerdict::Suspend`] (default `5000`).
    pub suspend_hold_ms: u64,
    /// Soft memory pressure threshold in bytes (default 64 MiB).
    pub memory_soft_limit_bytes: usize,
    /// Hard memory ceiling in bytes triggering [`MeterVerdict::Suspend`] (default 128 MiB).
    pub memory_hard_limit_bytes: usize,
}

impl Default for MeterConfig {
    fn default() -> Self {
        Self {
            throttle_free_secs: METER_THROTTLE_FREE_SECS,
            throttle_factor: METER_THROTTLE_FACTOR,
            sustained_factor: METER_SUSTAINED_FACTOR,
            sustained_window_secs: METER_SUSTAINED_WINDOW_SECS,
            sustained_limit_ratio: METER_SUSTAINED_LIMIT_RATIO,
            heavy_pass_secs: METER_HEAVY_PASS_SECS,
            load_grace_secs: METER_LOAD_GRACE_SECS,
            input_immediate_secs: METER_INPUT_IMMEDIATE_SECS,
            single_frame_suspend_secs: DEFAULT_SINGLE_FRAME_SUSPEND_SECS,
            max_consecutive_slow_frames: DEFAULT_MAX_CONSECUTIVE_SLOW_FRAMES,
            suspend_hold_ms: DEFAULT_SUSPEND_HOLD_MS,
            memory_soft_limit_bytes: DEFAULT_MEMORY_SOFT_LIMIT_BYTES,
            memory_hard_limit_bytes: DEFAULT_MEMORY_HARD_LIMIT_BYTES,
        }
    }
}

/// Reason a widget was throttled or suspended by [`WidgetMeter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchdogReason {
    /// Single pass exceeded the free frame budget (`> 16 ms`).
    FrameBudgetExceeded,
    /// Rolling CPU consumption exceeded sustained duty cycle with repeated heavy passes.
    SustainedCpuOverload,
    /// Single frame exceeded the hard raster deadline (`>= 1000 ms`).
    FrameTimeout,
    /// Too many consecutive slow frames occurred in a row.
    ConsecutiveSlowFrames,
    /// Accumulated CPU throttle hold exceeded the suspend ceiling.
    HoldCeilingExceeded,
    /// Memory footprint exceeded the soft throttle threshold.
    MemoryPressure,
    /// Memory footprint exceeded the hard memory ceiling.
    MemoryBudgetExceeded,
}

/// Budget watchdog verdict emitted by [`WidgetMeter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterVerdict {
    /// Widget is within all CPU, frame-time, and memory budgets.
    Allow,
    /// Widget should be throttled for `hold_ms` milliseconds.
    Throttle {
        /// Recommended hold duration in milliseconds.
        hold_ms: u64,
        /// Triggering watchdog condition.
        reason: WatchdogReason,
    },
    /// Widget breached a hard safety ceiling and must be suspended/stopped.
    Suspend {
        /// Triggering watchdog condition.
        reason: WatchdogReason,
        /// Human-readable diagnostic suitable for [`ControlMessage::Stopped`].
        detail: String,
    },
}

impl MeterVerdict {
    /// Returns `true` if work is admitted.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Returns `true` if the verdict requests throttling.
    #[must_use]
    pub const fn is_throttled(&self) -> bool {
        matches!(self, Self::Throttle { .. })
    }

    /// Returns `true` if the verdict requires suspending the widget.
    #[must_use]
    pub const fn is_suspended(&self) -> bool {
        matches!(self, Self::Suspend { .. })
    }

    /// Converts a [`MeterVerdict::Suspend`] into a hosted wire [`ControlMessage::Stopped`] frame.
    #[must_use]
    pub fn to_stop_control(&self) -> Option<ControlMessage> {
        match self {
            Self::Suspend { detail, .. } => Some(ControlMessage::Stopped(detail.clone())),
            _ => None,
        }
    }
}

/// Diagnostic snapshot of a [`WidgetMeter`] at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct MeterSnapshot {
    /// Current throttle hold in milliseconds (`0` if admitted or exempted).
    pub held_for_ms: u64,
    /// Sum of charged CPU seconds within the rolling window.
    pub window_cpu_secs: f64,
    /// Count of heavy passes (`> heavy_pass_secs`) in the rolling window.
    pub heavy_passes_in_window: u32,
    /// Count of consecutive slow frames observed.
    pub consecutive_slow_frames: u32,
    /// Last reported memory usage in bytes.
    pub memory_bytes: usize,
    /// Evaluated watchdog verdict.
    pub verdict: MeterVerdict,
}

#[derive(Debug, Clone, Copy)]
struct MeterCharge {
    at_secs: f64,
    cost_secs: f64,
    heavy: u32,
}

/// Per-widget frame-time, CPU-budget, and memory watchdog (`Tgw::WidgetMeter`).
///
/// Faithfully ports `tgw_widget_meter.cpp`'s 1-second load grace, 16 ms free pass budget, 3x
/// single-pass hold, 19x sustained heavy-pass hold, 250 ms post-input immediate exemption, and
/// nested operation accumulation, while additionally emitting explicit [`MeterVerdict`]
/// throttle/suspend decisions for slow-frame and memory-budget enforcement.
#[derive(Debug, Clone)]
pub struct WidgetMeter {
    config: MeterConfig,
    charges: VecDeque<MeterCharge>,
    first_secs: Option<f64>,
    until_secs: f64,
    last_input_secs: Option<f64>,
    last_throttle_reason: Option<WatchdogReason>,
    consecutive_slow_frames: u32,
    memory_bytes: usize,
    suspended: Option<(WatchdogReason, String)>,
    operations: u32,
    pending_spent_secs: f64,
}

impl Default for WidgetMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetMeter {
    /// Creates a fresh meter with default tdesktop budgets.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(MeterConfig::default())
    }

    /// Creates a fresh meter with custom [`MeterConfig`] thresholds.
    #[must_use]
    pub fn with_config(config: MeterConfig) -> Self {
        Self {
            config,
            charges: VecDeque::new(),
            first_secs: None,
            until_secs: 0.0,
            last_input_secs: None,
            last_throttle_reason: None,
            consecutive_slow_frames: 0,
            memory_bytes: 0,
            suspended: None,
            operations: 0,
            pending_spent_secs: 0.0,
        }
    }

    /// Returns the active [`MeterConfig`].
    #[must_use]
    pub const fn config(&self) -> &MeterConfig {
        &self.config
    }

    /// Notes user input at `now_secs`, granting a 250 ms throttle exemption
    /// (`WidgetMeter::input` in `tgw_widget_meter.cpp:73-78`).
    pub fn note_input(&mut self, now_secs: f64) {
        if now_secs.is_finite() {
            self.last_input_secs = Some(now_secs);
        }
    }

    /// Returns `true` if the widget is currently within the post-input throttle exemption window.
    #[must_use]
    pub fn has_input_exemption(&self, now_secs: f64) -> bool {
        self.last_input_secs
            .is_some_and(|input| now_secs - input <= self.config.input_immediate_secs)
    }

    /// Returns how many milliseconds the widget is currently held for at `now_secs` (`0` if admitted)
    /// (`WidgetMeter::heldFor` in `tgw_widget_meter.cpp:80-87`).
    #[must_use]
    pub fn held_for_ms(&self, now_secs: f64) -> u64 {
        if !now_secs.is_finite() || now_secs >= self.until_secs {
            return 0;
        }
        if self.has_input_exemption(now_secs) {
            return 0;
        }
        let ms = (self.until_secs - now_secs) * 1000.0;
        (ms - 1e-9).ceil().max(1.0) as u64
    }

    /// Begins a nested operation scope (`WidgetMeter::operation` in `tgw_widget_meter.cpp:30-37`).
    ///
    /// While an operation is open, inner work slices are always admitted and their CPU time is
    /// accumulated and charged once the outermost operation closes via [`Self::end_operation`].
    pub fn begin_operation(&mut self) {
        self.operations = self.operations.saturating_add(1);
    }

    /// Closes a nested operation scope and charges accumulated CPU time when the outermost
    /// operation completes (`WidgetMeter::close` in `tgw_widget_meter.cpp:124-128`).
    pub fn end_operation(&mut self, now_secs: f64) {
        if self.operations == 0 {
            return;
        }
        self.operations -= 1;
        if self.operations == 0 {
            let spent = std::mem::take(&mut self.pending_spent_secs);
            self.charge(now_secs, spent);
        }
    }

    /// Checks whether a work slice is admitted (`WidgetMeter::admit` in `tgw_widget_meter.cpp:102-113`).
    ///
    /// - If suspended, always returns `Err(TgwError::ResourceLimit(...))`.
    /// - If inside an operation (`begin_operation`), returns `Ok(true)`.
    /// - If held and `refuse_held` is `true`, returns `Err(TgwError::ResourceLimit(...))`.
    /// - If held and `refuse_held` is `false`, returns `Ok(false)`.
    pub fn admit_work(&self, now_secs: f64, refuse_held: bool) -> Result<bool> {
        if let Some((_, detail)) = &self.suspended {
            return Err(TgwError::ResourceLimit(detail.clone()));
        }
        if self.operations > 0 {
            return Ok(true);
        }
        let held = self.held_for_ms(now_secs);
        if held == 0 {
            Ok(true)
        } else if refuse_held {
            Err(TgwError::ResourceLimit(format!(
                "a request while held for {held} ms"
            )))
        } else {
            Ok(false)
        }
    }

    /// Records CPU work spent by a work slice (`WidgetMeter::spend` in `tgw_widget_meter.cpp:115-122`).
    pub fn spend_work(&mut self, now_secs: f64, spent_secs: f64) {
        if !spent_secs.is_finite() || spent_secs <= 0.0 {
            return;
        }
        if self.operations > 0 {
            self.pending_spent_secs += spent_secs;
        } else {
            self.charge(now_secs, spent_secs);
        }
    }

    /// Charges `spent_secs` of CPU work at `now_secs`
    /// (`WidgetMeter::charge` in `tgw_widget_meter.cpp:130-167`).
    pub fn charge(&mut self, now_secs: f64, spent_secs: f64) {
        if !now_secs.is_finite() || !spent_secs.is_finite() {
            return;
        }
        let first = *self.first_secs.get_or_insert(now_secs);
        if spent_secs <= 0.0 || (now_secs - first) < self.config.load_grace_secs {
            return;
        }
        self.prune_window(now_secs);

        if let Some(last) = self.charges.back_mut()
            && (last.at_secs - now_secs).abs() < f64::EPSILON
        {
            last.cost_secs += spent_secs;
            if spent_secs > self.config.heavy_pass_secs {
                last.heavy = last.heavy.saturating_add(1);
            }
        } else {
            self.charges.push_back(MeterCharge {
                at_secs: now_secs,
                cost_secs: spent_secs,
                heavy: u32::from(spent_secs > self.config.heavy_pass_secs),
            });
        }

        if spent_secs <= self.config.throttle_free_secs {
            return;
        }

        let (total, heavy) = self.window_totals();
        let sustained = total
            > self.config.sustained_limit_ratio * self.config.sustained_window_secs
            && heavy >= 2;
        let factor = if sustained {
            self.last_throttle_reason = Some(WatchdogReason::SustainedCpuOverload);
            self.config.sustained_factor
        } else {
            self.last_throttle_reason = Some(WatchdogReason::FrameBudgetExceeded);
            self.config.throttle_factor
        };
        self.until_secs = self.until_secs.max(now_secs) + spent_secs * factor;
    }

    /// Records one rendered frame duration (`frame_secs`) at `now_secs`, updating CPU throttling
    /// and slow-frame suspension checks, and returns the resulting [`MeterVerdict`].
    pub fn record_frame(&mut self, now_secs: f64, frame_secs: f64) -> MeterVerdict {
        if !now_secs.is_finite() || !frame_secs.is_finite() || frame_secs < 0.0 {
            return self.verdict(now_secs);
        }

        let first = *self.first_secs.get_or_insert(now_secs);
        let past_grace = (now_secs - first) >= self.config.load_grace_secs;

        // Hard single-frame raster timeout applies even before grace expires (DoS protection).
        if frame_secs >= self.config.single_frame_suspend_secs && self.suspended.is_none() {
            let ms = (frame_secs * 1000.0).round() as u64;
            let limit_ms = (self.config.single_frame_suspend_secs * 1000.0).round() as u64;
            self.suspended = Some((
                WatchdogReason::FrameTimeout,
                format!("widget frame took {ms} ms, exceeding hard limit of {limit_ms} ms"),
            ));
            return self.verdict(now_secs);
        }

        self.charge(now_secs, frame_secs);

        if past_grace {
            if frame_secs > self.config.heavy_pass_secs {
                self.consecutive_slow_frames = self.consecutive_slow_frames.saturating_add(1);
            } else {
                self.consecutive_slow_frames = 0;
            }

            if self.suspended.is_none() {
                if self.consecutive_slow_frames >= self.config.max_consecutive_slow_frames {
                    self.suspended = Some((
                        WatchdogReason::ConsecutiveSlowFrames,
                        format!(
                            "widget exceeded slow-frame limit ({} consecutive frames over {:.0} ms)",
                            self.consecutive_slow_frames,
                            self.config.heavy_pass_secs * 1000.0
                        ),
                    ));
                } else {
                    let raw_hold_ms =
                        ((self.until_secs - now_secs).max(0.0) * 1000.0).ceil() as u64;
                    if raw_hold_ms >= self.config.suspend_hold_ms {
                        self.suspended = Some((
                            WatchdogReason::HoldCeilingExceeded,
                            format!(
                                "widget CPU throttle hold reached {raw_hold_ms} ms (ceiling {} ms)",
                                self.config.suspend_hold_ms
                            ),
                        ));
                    }
                }
            }
        }

        self.verdict(now_secs)
    }

    /// Updates tracked memory usage (bytes) and evaluates memory pressure / hard-ceiling verdicts.
    pub fn update_memory(&mut self, bytes: usize, now_secs: f64) -> MeterVerdict {
        self.memory_bytes = bytes;
        if bytes > self.config.memory_hard_limit_bytes && self.suspended.is_none() {
            self.suspended = Some((
                WatchdogReason::MemoryBudgetExceeded,
                format!(
                    "widget memory {bytes} bytes exceeded hard limit of {} bytes",
                    self.config.memory_hard_limit_bytes
                ),
            ));
        }
        self.verdict(now_secs)
    }

    /// Evaluates the current [`MeterVerdict`] at `now_secs` without mutating state.
    #[must_use]
    pub fn verdict(&self, now_secs: f64) -> MeterVerdict {
        if let Some((reason, detail)) = &self.suspended {
            return MeterVerdict::Suspend {
                reason: *reason,
                detail: detail.clone(),
            };
        }

        let cpu_hold = self.held_for_ms(now_secs);
        if cpu_hold > 0 {
            let reason = self
                .last_throttle_reason
                .unwrap_or(WatchdogReason::FrameBudgetExceeded);
            return MeterVerdict::Throttle {
                hold_ms: cpu_hold,
                reason,
            };
        }

        if self.memory_bytes > self.config.memory_soft_limit_bytes
            && !self.has_input_exemption(now_secs)
        {
            return MeterVerdict::Throttle {
                hold_ms: DEFAULT_MEMORY_PRESSURE_HOLD_MS,
                reason: WatchdogReason::MemoryPressure,
            };
        }

        MeterVerdict::Allow
    }

    /// Returns a diagnostic [`MeterSnapshot`] at `now_secs`.
    #[must_use]
    pub fn snapshot(&self, now_secs: f64) -> MeterSnapshot {
        let mut window_cpu_secs = 0.0;
        let mut heavy_passes_in_window = 0u32;
        for entry in &self.charges {
            if now_secs - entry.at_secs <= self.config.sustained_window_secs {
                window_cpu_secs += entry.cost_secs;
                heavy_passes_in_window = heavy_passes_in_window.saturating_add(entry.heavy);
            }
        }
        MeterSnapshot {
            held_for_ms: self.held_for_ms(now_secs),
            window_cpu_secs,
            heavy_passes_in_window,
            consecutive_slow_frames: self.consecutive_slow_frames,
            memory_bytes: self.memory_bytes,
            verdict: self.verdict(now_secs),
        }
    }

    /// Clears a latched suspension state and resets counters (e.g. after widget content update).
    pub fn reset_suspension(&mut self) {
        self.suspended = None;
        self.consecutive_slow_frames = 0;
        self.until_secs = 0.0;
        self.last_throttle_reason = None;
        self.charges.clear();
    }

    fn prune_window(&mut self, now_secs: f64) {
        while let Some(front) = self.charges.front() {
            if now_secs - front.at_secs > self.config.sustained_window_secs {
                self.charges.pop_front();
            } else {
                break;
            }
        }
    }

    fn window_totals(&self) -> (f64, u32) {
        let mut total = 0.0;
        let mut heavy = 0u32;
        for entry in &self.charges {
            total += entry.cost_secs;
            heavy = heavy.saturating_add(entry.heavy);
        }
        (total, heavy)
    }
}
