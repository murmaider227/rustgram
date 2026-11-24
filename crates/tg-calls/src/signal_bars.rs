//! Call signal quality bars state and geometry logic.
//!
//! Port of `calls/calls_signal_bars.h`, `calls/calls_signal_bars.cpp`,
//! and `Call::kSignalBar*` constants in `calls/calls_call.h:210-215`.

/// Initial signal bar sentinel before media connection reports quality (`kSignalBarStarting`).
pub const SIGNAL_BAR_STARTING: i32 = -1;

/// Terminal signal bar sentinel once the call finishes (`kSignalBarFinished`).
pub const SIGNAL_BAR_FINISHED: i32 = -2;

/// Total number of signal strength bars displayed (`kSignalBarCount`).
pub const SIGNAL_BAR_COUNT: usize = 4;

/// State machine for the call signal strength indicator.
///
/// Mirrors `Calls::SignalBars` (`calls_signal_bars.cpp:56-63`): once transitioned
/// to [`SIGNAL_BAR_FINISHED`], further updates are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalBarsState {
    count: i32,
}

impl Default for SignalBarsState {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalBarsState {
    /// Create a new signal bars tracker starting at [`SIGNAL_BAR_STARTING`].
    pub const fn new() -> Self {
        Self {
            count: SIGNAL_BAR_STARTING,
        }
    }

    /// Raw bar count (`SIGNAL_BAR_STARTING`, `SIGNAL_BAR_FINISHED`, or `0..=4`).
    pub const fn raw_count(&self) -> i32 {
        self.count
    }

    /// Apply a signal bar count update, returning `true` if the visible state changed.
    ///
    /// Exact port of `SignalBars::changed` (`calls_signal_bars.cpp:56-63`).
    pub fn update(&mut self, count: i32) -> bool {
        if self.count == SIGNAL_BAR_FINISHED {
            return false;
        }
        if self.count != count {
            self.count = count;
            true
        } else {
            false
        }
    }

    /// Returns whether bar `index` (`0..SIGNAL_BAR_COUNT`) should render as active (full opacity).
    ///
    /// Matches `(i < _count)` in `SignalBars::paintEvent` (`calls_signal_bars.cpp:39`).
    pub const fn is_bar_active(&self, index: usize) -> bool {
        if index >= SIGNAL_BAR_COUNT || self.count <= 0 {
            return false;
        }
        (index as i32) < self.count
    }

    /// Returns `true` if the call is still starting (no signal sample received yet).
    pub const fn is_starting(&self) -> bool {
        self.count == SIGNAL_BAR_STARTING
    }

    /// Returns `true` if the call has finished and the indicator is frozen.
    pub const fn is_finished(&self) -> bool {
        self.count == SIGNAL_BAR_FINISHED
    }

    /// Compute the height of bar `index` (`0..SIGNAL_BAR_COUNT`) given `min_height` and `max_height`.
    ///
    /// Matches `calls_signal_bars.cpp:40-41`:
    /// `barHeight = min + (max - min) * (i / (kSignalBarCount - 1))`.
    pub fn bar_height(index: usize, min_height: f64, max_height: f64) -> f64 {
        let clamped = index.min(SIGNAL_BAR_COUNT - 1);
        min_height + (max_height - min_height) * (clamped as f64 / (SIGNAL_BAR_COUNT - 1) as f64)
    }
}
