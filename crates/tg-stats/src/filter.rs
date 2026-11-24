//! Line visibility filter controller and alpha transition animation.
//!
//! Port of `Telegram/SourceFiles/statistics/chart_lines_filter_controller.{h,cpp}`
//! and toggle guard logic from `statistics/widgets/chart_lines_filter_widget.cpp`.

use std::collections::BTreeMap;

use crate::anim::{AnimValue, ease_in_cubic};

#[derive(Debug, Clone)]
struct Entry {
    enabled: bool,
    started_at: i64,
    alpha: f64,
    anim: AnimValue,
    dt_current: f64,
}

/// Manages per-line enabled state and animated opacity (`alpha` in `0.0..=1.0`).
///
/// Mirrors `Statistic::LinesFilterController`: lines not present in the internal
/// map are implicitly enabled with `alpha == 1.0`.
#[derive(Debug, Clone)]
pub struct LinesFilterController {
    entries: BTreeMap<i32, Entry>,
    is_finished: bool,
}

impl Default for LinesFilterController {
    fn default() -> Self {
        Self::new()
    }
}

impl LinesFilterController {
    /// Creates a new filter controller with all lines enabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            is_finished: true,
        }
    }

    /// Sets whether line `id` is enabled, starting an alpha transition at timestamp `now`.
    ///
    /// Note: `now` must be non-zero for `tick()` to advance the entry (matching C++ `if (!entry.startedAt) continue`).
    pub fn set_enabled(&mut self, id: i32, enabled: bool, now: i64) {
        match self.entries.get_mut(&id) {
            None => {
                self.entries.insert(
                    id,
                    Entry {
                        enabled,
                        started_at: now,
                        alpha: 1.0,
                        anim: AnimValue::new(
                            if enabled { 0.0 } else { 1.0 },
                            if enabled { 1.0 } else { 0.0 },
                        ),
                        dt_current: 0.0,
                    },
                );
            }
            Some(entry) if entry.enabled != enabled => {
                entry.enabled = enabled;
                entry.started_at = now;
                entry.dt_current = 0.0;
                entry.anim.start(if enabled { 1.0 } else { 0.0 });
            }
            Some(_) => {}
        }
        self.is_finished = false;
    }

    /// Attempts to toggle line `id`, refusing (returning `false`) if disabling it would leave
    /// zero enabled lines among `all_line_ids`.
    ///
    /// Port of the single-remaining-line shake guard in `ChartLinesFilterWidget::fillButtons`.
    pub fn try_toggle(&mut self, id: i32, all_line_ids: &[i32], now: i64) -> bool {
        let next_enabled = !self.is_enabled(id);
        if !next_enabled {
            let another_enabled = all_line_ids
                .iter()
                .any(|&other| other != id && self.is_enabled(other));
            if !another_enabled {
                return false;
            }
        }
        self.set_enabled(id, next_enabled, now);
        true
    }

    /// Returns `true` if all active line-alpha transitions have completed.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.is_finished
    }

    /// Returns whether line `id` is logically enabled (defaults to `true`).
    #[must_use]
    pub fn is_enabled(&self, id: i32) -> bool {
        self.entries.get(&id).is_none_or(|e| e.enabled)
    }

    /// Returns the current interpolated opacity of line `id` in `0.0..=1.0` (defaults to `1.0`).
    #[must_use]
    pub fn alpha(&self, id: i32) -> f64 {
        self.entries.get(&id).map_or(1.0, |e| e.alpha)
    }

    /// Advances all active transitions by `dt_speed` (in `0.0..=1.0`) using `ease_in_cubic`.
    pub fn tick(&mut self, dt_speed: f64) {
        let mut finished_count = 0usize;
        let mut ids_to_remove = Vec::new();

        for (&id, entry) in &mut self.entries {
            if entry.started_at == 0 {
                continue;
            }
            entry.dt_current = (entry.dt_current + dt_speed).min(1.0);
            entry.anim.update(entry.dt_current, ease_in_cubic);
            entry.alpha = entry.anim.current().clamp(0.0, 1.0);
            if entry.alpha == 1.0 && entry.enabled {
                ids_to_remove.push(id);
            }
            if entry.anim.current() == entry.anim.to() {
                finished_count += 1;
                entry.anim.finish();
            }
        }

        self.is_finished = finished_count == self.entries.len();
        for id in ids_to_remove {
            self.entries.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_and_reenable_animation_follows_ease_in_cubic() {
        let mut filter = LinesFilterController::new();
        assert!(filter.is_enabled(1));
        assert_eq!(filter.alpha(1), 1.0);
        assert!(filter.is_finished());

        filter.set_enabled(1, false, 1);
        assert!(!filter.is_enabled(1));
        assert!(!filter.is_finished());

        // At dt = 0.5, easeInCubic(-1, 0.5) = -0.125 -> alpha = 0.875.
        filter.tick(0.5);
        assert!((filter.alpha(1) - 0.875).abs() < 1e-12);
        assert!(!filter.is_finished());

        filter.tick(0.5);
        assert_eq!(filter.alpha(1), 0.0);
        assert!(filter.is_finished());

        // Re-enable -> should animate back to 1.0 and remove entry when done.
        filter.set_enabled(1, true, 10);
        filter.tick(0.5);
        assert!((filter.alpha(1) - 0.125).abs() < 1e-12);
        filter.tick(0.5);
        assert_eq!(filter.alpha(1), 1.0);
        assert!(filter.is_finished());
        assert!(filter.entries.is_empty());
    }

    #[test]
    fn cannot_disable_last_enabled_line() {
        let mut filter = LinesFilterController::new();
        let ids = [1, 2];
        assert!(filter.try_toggle(1, &ids, 1));
        assert!(!filter.is_enabled(1));
        assert!(!filter.try_toggle(2, &ids, 2));
        assert!(filter.is_enabled(2));
    }
}
