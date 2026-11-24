//! Injectable monotonic clock hook for deterministic headless UI tests.
//!
//! Port origin: test-only time injection analog of tdesktop's `crl::time` overrides.
//! In production builds (when [`set_test_now`] is never called), [`now`] delegates
//! directly to [`std::time::Instant::now`].

use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    static TEST_NOW: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Returns the current monotonic instant, using this thread's virtual test clock when installed
/// and falling back to [`Instant::now`] otherwise.
#[inline]
#[must_use]
pub fn now() -> Instant {
    TEST_NOW.with(|cell| cell.get().unwrap_or_else(Instant::now))
}

/// Initializes the current thread's virtual test clock if it is not already active, returning
/// the active virtual instant.
///
/// Calling this repeatedly on the same thread preserves monotonicity across multiple harnesses.
pub fn init_test_clock() -> Instant {
    TEST_NOW.with(|cell| {
        if let Some(existing) = cell.get() {
            existing
        } else {
            let start = Instant::now();
            cell.set(Some(start));
            start
        }
    })
}

/// Explicitly sets or clears (`None`) the current thread's virtual test clock.
pub fn set_test_now(instant: Option<Instant>) {
    TEST_NOW.with(|cell| cell.set(instant));
}

/// Advances the current thread's virtual test clock by `delta`, initializing it first if unset.
pub fn advance_test_now(delta: Duration) -> Instant {
    TEST_NOW.with(|cell| {
        let next = cell.get().unwrap_or_else(Instant::now) + delta;
        cell.set(Some(next));
        next
    })
}

/// Clears the current thread's virtual test clock so [`now`] resumes reading wall-clock time.
pub fn reset_test_clock() {
    set_test_now(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_stays_frozen_until_advanced() {
        reset_test_clock();
        let t0 = init_test_clock();
        assert_eq!(now(), t0);

        let t1 = advance_test_now(Duration::from_millis(250));
        assert_eq!(t1.saturating_duration_since(t0), Duration::from_millis(250));
        assert_eq!(now(), t1);

        // Re-initializing preserves monotonicity on the same thread.
        assert_eq!(init_test_clock(), t1);

        reset_test_clock();
    }
}
