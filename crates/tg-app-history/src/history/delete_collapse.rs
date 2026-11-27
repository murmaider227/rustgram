//! The hole a deleted message leaves closes smoothly (TGB-3011).
//!
//! Port of the collapse half of tdesktop `Ui::ThanosEffectController`
//! (`ui/effects/thanos_effect_controller.cpp`: `notePendingRemoval`, `flushRemovals`,
//! `startCollapseAnimation`, `collapseAnimationCallback`, `syncCollapseGapsToHost`). A removed
//! message's height stays in the list as a gap that shrinks to zero with `anim::halfSine` over
//! 400..600 ms. The view keeps its scroll baseline: at the bottom it stays at the bottom, else it
//! scrolls up by what has collapsed, so the messages below the hole stay where they are and the
//! ones above slide down into it.
//!
//! Internally the gaps are kept in the list's current coordinates (each removal is mapped to
//! where the next message now starts) instead of tdesktop's coordinates of the first capture
//! plus a running shift; the visible result is the same and a second deletion during the
//! animation needs no correction pass.

use tg_ui::anim::half_sine;

/// `kBaseDuration`.
pub(crate) const BASE_DURATION_MS: f64 = 400.0;
/// `kPerPixelDuration`.
pub(crate) const PER_PIXEL_DURATION_MS: f64 = 0.15;
/// `kMaxDuration`.
pub(crate) const MAX_DURATION_MS: f64 = 600.0;

/// One removed message, in item coordinates (`HistoryListItem::y`) before the removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Removal {
    /// Top of the removed item.
    pub top: i32,
    /// Height of the removed item.
    pub height: i32,
    /// It may leave a gap: a regular, non-service message not below the view
    /// (`captureOnRemoval` and `PendingRemoval::below`).
    pub animated: bool,
}

/// Where the view goes on this frame of the collapse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CollapseScroll {
    /// The view was at the bottom when the messages were removed: keep it there.
    Bottom,
    /// Scroll to this offset (`_savedScrollTop - collapsed`, at least zero).
    Top(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GapState {
    /// Item coordinate where the gap starts: items with `y >= at` are pushed down.
    at: i32,
    start_height: i32,
    current_height: i32,
    original_height: i32,
}

/// The animated gaps of removed messages (`ThanosEffectController` collapse state).
#[derive(Debug, Default)]
pub(crate) struct CollapseAnimator {
    gaps: Vec<GapState>,
    started_ms: f64,
    duration_ms: f64,
    saved_scroll_top: f32,
    was_at_bottom: bool,
    restore_scroll_pending: bool,
}

impl CollapseAnimator {
    /// `true` while a gap is still closing.
    #[cfg(test)]
    pub(crate) fn animating(&self) -> bool {
        !self.gaps.is_empty()
    }

    /// Sum of the current gap heights.
    pub(crate) fn total(&self) -> i32 {
        self.gaps.iter().map(|g| g.current_height).sum()
    }

    /// Records the scroll position the collapse is measured from, once per animation
    /// (`ensureScrollBaseline`).
    pub(crate) fn note_scroll_baseline(&mut self, scroll_top: f32, at_bottom: bool) {
        if !self.restore_scroll_pending {
            self.saved_scroll_top = scroll_top;
            self.was_at_bottom = at_bottom;
            self.restore_scroll_pending = true;
        }
    }

    /// Opens gaps for `removals` (sorted by `top`, all the messages removed together). `left`
    /// is how much the list's height actually shrank, shared between the removals by their
    /// heights (`flushRemovals`: neighbours may relayout when a message goes).
    pub(crate) fn flush(&mut self, removals: &[Removal], left: i32, now_ms: f64) {
        if left <= 0 || !removals.iter().any(|r| r.animated) {
            self.reset_baseline_if_idle();
            return;
        }
        let assumed: i64 = removals.iter().map(|r| i64::from(r.height)).sum();
        let mut remaining = left;
        let mut removed_above = 0;
        let count = removals.len();
        let mut opened = Vec::new();
        for (i, removal) in removals.iter().enumerate() {
            let share = if i + 1 == count {
                remaining
            } else if assumed > 0 {
                (i64::from(removal.height) * i64::from(left) / assumed) as i32
            } else {
                0
            };
            remaining -= share;
            // The message's old place, in the list without the removed messages.
            let at = removal.top - removed_above;
            removed_above += removal.height;
            if removal.animated {
                opened.push((at, share));
            }
        }
        // Gaps already closing below a removed message move up with the rest of the list.
        for gap in &mut self.gaps {
            let above: i32 = removals
                .iter()
                .filter(|r| r.top < gap.at)
                .map(|r| r.height)
                .sum();
            gap.at -= above;
        }
        for (at, height) in opened {
            self.start_collapse(height, at, now_ms);
        }
        self.reset_baseline_if_idle();
    }

    /// `startCollapseAnimation`: adds `height` at `at`, merging with a gap at the same place,
    /// and restarts the animation from the current heights.
    fn start_collapse(&mut self, height: i32, at: i32, now_ms: f64) {
        if height <= 0 {
            return;
        }
        for gap in &mut self.gaps {
            gap.start_height = gap.current_height;
        }
        if let Some(gap) = self.gaps.iter_mut().find(|g| g.at == at) {
            gap.start_height += height;
            gap.current_height += height;
            gap.original_height += height;
        } else {
            let index = self.gaps.partition_point(|g| g.at < at);
            self.gaps.insert(
                index,
                GapState {
                    at,
                    start_height: height,
                    current_height: height,
                    original_height: height,
                },
            );
        }
        let total = f64::from(self.total());
        self.duration_ms = (BASE_DURATION_MS + total * PER_PIXEL_DURATION_MS)
            .clamp(BASE_DURATION_MS, MAX_DURATION_MS);
        self.started_ms = now_ms;
    }

    /// Advances the collapse to `now_ms` (`collapseAnimationCallback`). Returns where the view
    /// goes on this frame, or `None` when nothing is collapsing. The frame that finishes the
    /// animation still returns the final scroll.
    pub(crate) fn tick(&mut self, now_ms: f64) -> Option<CollapseScroll> {
        if self.gaps.is_empty() {
            return None;
        }
        let t = if self.duration_ms > 0.0 {
            ((now_ms - self.started_ms) / self.duration_ms).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let progress = half_sine(1.0, t);
        let mut sum_original = 0;
        let mut sum_current = 0;
        for gap in &mut self.gaps {
            // `anim::interpolate(int, int, float64)` rounds.
            gap.current_height = (f64::from(gap.start_height) * (1.0 - progress)).round() as i32;
            sum_original += gap.original_height;
            sum_current += gap.current_height;
        }
        let scroll = if self.was_at_bottom {
            CollapseScroll::Bottom
        } else {
            CollapseScroll::Top(
                (self.saved_scroll_top - (sum_original - sum_current) as f32).max(0.0),
            )
        };
        if t >= 1.0 {
            self.gaps.clear();
            self.restore_scroll_pending = false;
            self.was_at_bottom = false;
        }
        Some(scroll)
    }

    /// The gaps as the list draws them (`syncCollapseGapsToHost`), in item coordinates.
    pub(crate) fn render_gaps(&self) -> Vec<tg_history::CollapseGap> {
        self.gaps
            .iter()
            .filter(|g| g.current_height > 0)
            .map(|g| tg_history::CollapseGap {
                abs_y: g.at,
                height: g.current_height,
            })
            .collect()
    }

    /// Messages were added or dropped above everything (a page loaded at the top, old messages
    /// unloaded): the gaps and the scroll baseline move with them (`shiftGaps`,
    /// `applyPrependBaseline`).
    pub(crate) fn shift(&mut self, delta: i32) {
        if delta == 0 || self.gaps.is_empty() {
            return;
        }
        for gap in &mut self.gaps {
            gap.at += delta;
        }
        if self.restore_scroll_pending && !self.was_at_bottom {
            self.saved_scroll_top += delta as f32;
        }
    }

    /// Drops everything at once (another chat opened, effects turned off).
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// `resetScrollBaseline`: a removal that opened no gap leaves no baseline behind.
    fn reset_baseline_if_idle(&mut self) {
        if self.gaps.is_empty() {
            self.restore_scroll_pending = false;
            self.was_at_bottom = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removal(top: i32, height: i32) -> Removal {
        Removal {
            top,
            height,
            animated: true,
        }
    }

    #[test]
    fn duration_grows_with_the_hole_from_400_to_600_ms() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(0.0, true);
        a.flush(&[removal(100, 60)], 60, 0.0);
        assert!((a.duration_ms - 409.0).abs() < 1e-9);
        let mut b = CollapseAnimator::default();
        b.note_scroll_baseline(0.0, true);
        b.flush(&[removal(100, 2000)], 2000, 0.0);
        assert!((b.duration_ms - MAX_DURATION_MS).abs() < 1e-9);
    }

    #[test]
    fn the_gap_closes_with_half_sine() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(500.0, false);
        a.flush(&[removal(100, 100)], 100, 1000.0);
        assert_eq!(
            a.render_gaps(),
            vec![tg_history::CollapseGap {
                abs_y: 100,
                height: 100
            }]
        );
        // Half way: sin(pi/4) = 0.7071 collapsed.
        let half = a.duration_ms / 2.0;
        assert_eq!(
            a.tick(1000.0 + half),
            Some(CollapseScroll::Top(500.0 - 71.0))
        );
        assert_eq!(a.total(), 29);
        assert_eq!(
            a.tick(1000.0 + a.duration_ms),
            Some(CollapseScroll::Top(400.0))
        );
        assert!(!a.animating());
        assert_eq!(a.tick(5000.0), None);
    }

    #[test]
    fn at_the_bottom_the_view_stays_at_the_bottom() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(900.0, true);
        a.flush(&[removal(1000, 80)], 80, 0.0);
        assert_eq!(a.tick(100.0), Some(CollapseScroll::Bottom));
    }

    #[test]
    fn neighbours_merge_into_one_gap_and_others_keep_their_place() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(0.0, true);
        // Items at 100 (h 40), 140 (h 60) and 300 (h 50) go.
        a.flush(
            &[removal(100, 40), removal(140, 60), removal(300, 50)],
            150,
            0.0,
        );
        assert_eq!(
            a.render_gaps(),
            vec![
                tg_history::CollapseGap {
                    abs_y: 100,
                    height: 100
                },
                tg_history::CollapseGap {
                    abs_y: 200,
                    height: 50
                },
            ]
        );
    }

    #[test]
    fn the_real_shrink_is_shared_by_height_and_hidden_ones_open_no_gap() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(0.0, false);
        let below = Removal {
            top: 900,
            height: 100,
            animated: false,
        };
        // A neighbour lost its date badge too: the list shrank by 210, not 200.
        a.flush(&[removal(100, 100), below], 210, 0.0);
        assert_eq!(
            a.render_gaps(),
            vec![tg_history::CollapseGap {
                abs_y: 100,
                height: 105
            }]
        );
    }

    #[test]
    fn a_second_deletion_restarts_from_the_current_heights() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(400.0, false);
        a.flush(&[removal(500, 100)], 100, 0.0);
        let half = a.duration_ms / 2.0;
        a.tick(half);
        assert_eq!(a.total(), 29);
        // A message above the first gap goes: the first gap moves up by its height.
        a.note_scroll_baseline(0.0, true);
        a.flush(&[removal(200, 40)], 40, half);
        assert_eq!(
            a.render_gaps(),
            vec![
                tg_history::CollapseGap {
                    abs_y: 200,
                    height: 40
                },
                tg_history::CollapseGap {
                    abs_y: 460,
                    height: 29
                },
            ]
        );
        // The baseline of the first deletion stays.
        assert_eq!(a.tick(half), Some(CollapseScroll::Top(400.0 - 71.0)));
    }

    #[test]
    fn nothing_to_animate_leaves_no_baseline() {
        let mut a = CollapseAnimator::default();
        a.note_scroll_baseline(300.0, true);
        a.flush(
            &[Removal {
                top: 10,
                height: 20,
                animated: false,
            }],
            20,
            0.0,
        );
        assert!(!a.animating());
        a.note_scroll_baseline(100.0, false);
        a.flush(&[removal(0, 50)], 50, 0.0);
        assert_eq!(a.tick(0.0), Some(CollapseScroll::Top(100.0)));
    }
}
