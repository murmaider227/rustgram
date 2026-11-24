//! Pure viewport read-metrics visibility tracker and batching queue.
//!
//! Port origin: `Telegram/SourceFiles/history/view/history_view_read_metrics_tracker.{h,cpp}`
//! and `Telegram/SourceFiles/api/api_read_metrics.{h,cpp}`.

use std::collections::{HashMap, HashSet};

use tg_tl::{enums as tl_enums, types as tl_types};

use crate::stand_in::types::{MsgId, PeerId};

/// Entry and exit visibility grace period (`kGracePeriod = 300ms`).
pub const READ_METRICS_GRACE_PERIOD_MS: u64 = 300;

/// User activity window (`kActivityTimeout = 15s`).
pub const READ_METRICS_ACTIVITY_TIMEOUT_MS: u64 = 15_000;

/// Hard per-item tracking duration cap (`kMaxTrackingDuration = 5 min`).
pub const READ_METRICS_MAX_TRACKING_DURATION_MS: u64 = 5 * 60 * 1_000;

/// Minimum accumulated time in view required before reporting (`kMinReportThreshold = 300ms`).
pub const READ_METRICS_MIN_REPORT_THRESHOLD_MS: u64 = 300;

/// Batching delay before flushing queued metrics via `messages.reportReadMetrics` (`kSendTimeout = 5s`).
pub const READ_METRICS_SEND_TIMEOUT_MS: u64 = 5_000;

/// One finalized message visibility metric ready to serialize as `inputMessageReadMetric`
/// (`Api::FinalizedReadMetric`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedReadMetric {
    /// Server message id.
    pub msg_id: MsgId,
    /// Non-zero random 64-bit view session identifier.
    pub view_id: i64,
    /// Total time in viewport (ms).
    pub time_in_view_ms: i32,
    /// Active (non-idle) time in viewport (ms).
    pub active_time_in_view_ms: i32,
    /// `round(max_item_height * 1000 / max_viewport_height)`.
    pub height_to_viewport_ratio_permille: i32,
    /// `clamp(round((seen_bottom - seen_top) * 1000 / max_item_height), 0, 1000)`.
    pub seen_range_ratio_permille: i32,
}

impl FinalizedReadMetric {
    /// Converts this metric into the wire TL `InputMessageReadMetric` enum.
    #[must_use]
    pub const fn to_tl(self) -> tl_enums::InputMessageReadMetric {
        tl_enums::InputMessageReadMetric::InputMessageReadMetric(tl_types::InputMessageReadMetric {
            msg_id: self.msg_id.0,
            view_id: self.view_id,
            time_in_view_ms: self.time_in_view_ms,
            active_time_in_view_ms: self.active_time_in_view_ms,
            height_to_viewport_ratio_permille: self.height_to_viewport_ratio_permille,
            seen_range_ratio_permille: self.seen_range_ratio_permille,
        })
    }
}

#[derive(Debug, Clone)]
struct TrackedItem {
    view_id: i64,
    entry_grace_start: u64,
    tracking_started: u64,
    last_update: u64,
    total_in_view: u64,
    active_in_view: u64,
    seen_top: i32,
    seen_bottom: i32,
    max_item_height: i32,
    max_viewport_height: i32,
    entry_grace_pending: bool,
    exit_grace_pending: bool,
    exit_grace_start: u64,
}

/// Per-chat viewport visibility tracker (`HistoryView::ReadMetricsTracker`).
#[derive(Debug, Clone)]
pub struct ReadMetricsTracker {
    peer_id: PeerId,
    tracked: HashMap<MsgId, TrackedItem>,
    currently_visible: HashSet<MsgId>,
    batch_now: u64,
    batch_viewport_height: i32,
    batch_visible_top: i32,
    batch_visible_bottom: i32,
    batch_visible: HashSet<MsgId>,
    last_activity: Option<u64>,
    app_active: bool,
    screen_active: bool,
    paused: bool,
    paused_since: u64,
    rng_state: u64,
    completed: Vec<FinalizedReadMetric>,
}

impl ReadMetricsTracker {
    /// Creates a new read-metrics tracker for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            tracked: HashMap::new(),
            currently_visible: HashSet::new(),
            batch_now: 0,
            batch_viewport_height: 0,
            batch_visible_top: 0,
            batch_visible_bottom: 0,
            batch_visible: HashSet::new(),
            last_activity: None,
            app_active: true,
            screen_active: true,
            paused: false,
            paused_since: 0,
            rng_state: ((peer_id.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
                ^ 0x517C_C1B7_2722_0A95,
            completed: Vec::new(),
        }
    }

    /// Returns the peer id tracked by this instance.
    #[must_use]
    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Returns the number of items currently being tracked (including grace periods).
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.tracked.len()
    }

    fn next_view_id(&mut self) -> i64 {
        loop {
            let mut x = self.rng_state;
            if x == 0 {
                x = 0x2545_F491_4F6C_DD1D;
            }
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.rng_state = x;
            let val = x.wrapping_mul(0x2545_F491_4F6C_DD1D) as i64;
            if val != 0 {
                return val;
            }
        }
    }

    /// Begins a visible-items frame batch (`ReadMetricsTracker::startBatch`).
    pub fn start_batch(&mut self, visible_top: i32, visible_bottom: i32, now_ms: u64) {
        self.batch_now = now_ms;
        self.sync(now_ms);
        self.batch_viewport_height = (visible_bottom - visible_top).max(0);
        self.batch_visible_top = visible_top;
        self.batch_visible_bottom = visible_bottom;
        self.batch_visible.clear();
    }

    /// Records one visible item in the active batch (`ReadMetricsTracker::push`).
    ///
    /// Only regular server messages with view counts (`is_regular && has_views && msg_id.0 > 0`)
    /// are tracked, matching `ReadMetricsTracker::ShouldTrack`.
    pub fn push(
        &mut self,
        msg_id: MsgId,
        is_regular: bool,
        has_views: bool,
        item_top: i32,
        item_height: i32,
    ) {
        if !is_regular || !has_views || msg_id.0 <= 0 || item_height <= 0 {
            return;
        }
        self.batch_visible.insert(msg_id);

        let clipped_top = item_top.max(self.batch_visible_top) - item_top;
        let clipped_bottom = (item_top + item_height).min(self.batch_visible_bottom) - item_top;

        let new_entry = || TrackedItem {
            view_id: 0,
            entry_grace_start: self.batch_now,
            tracking_started: 0,
            last_update: 0,
            total_in_view: 0,
            active_in_view: 0,
            seen_top: clipped_top,
            seen_bottom: clipped_bottom,
            max_item_height: item_height,
            max_viewport_height: self.batch_viewport_height,
            entry_grace_pending: true,
            exit_grace_pending: false,
            exit_grace_start: 0,
        };

        let mut replace_with_fresh = false;
        if let Some(tracked) = self.tracked.get_mut(&msg_id) {
            if tracked.exit_grace_pending {
                if self.batch_now.saturating_sub(tracked.exit_grace_start)
                    >= READ_METRICS_GRACE_PERIOD_MS
                {
                    let finalized = Self::build_finalized(msg_id, tracked);
                    if let Some(m) = finalized {
                        self.completed.push(m);
                    }
                    replace_with_fresh = true;
                } else {
                    tracked.exit_grace_pending = false;
                    tracked.exit_grace_start = 0;
                    tracked.last_update = self.batch_now;
                }
            }
            if !replace_with_fresh {
                tracked.seen_top = tracked.seen_top.min(clipped_top);
                tracked.seen_bottom = tracked.seen_bottom.max(clipped_bottom);
                tracked.max_item_height = tracked.max_item_height.max(item_height);
                tracked.max_viewport_height =
                    tracked.max_viewport_height.max(self.batch_viewport_height);
                return;
            }
        }
        self.tracked.insert(msg_id, new_entry());
    }

    /// Finishes the current visibility batch (`ReadMetricsTracker::endBatch`) and returns any
    /// metrics finalized during this frame.
    pub fn end_batch(&mut self) -> Vec<FinalizedReadMetric> {
        let batch_visible = &self.batch_visible;
        let batch_now = self.batch_now;
        self.tracked.retain(|msg_id, tracked| {
            if batch_visible.contains(msg_id) {
                return true;
            }
            if tracked.entry_grace_pending {
                return false;
            }
            if !tracked.exit_grace_pending {
                tracked.exit_grace_pending = true;
                tracked.exit_grace_start = batch_now;
            }
            true
        });
        self.currently_visible = std::mem::take(&mut self.batch_visible);
        std::mem::take(&mut self.completed)
    }

    /// Advances timers and returns any finalized metrics at `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> Vec<FinalizedReadMetric> {
        self.sync(now_ms);
        std::mem::take(&mut self.completed)
    }

    /// Registers user input activity (`ReadMetricsTracker::registerActivity`).
    pub fn register_activity(&mut self, now_ms: u64) {
        if !self.app_active || !self.screen_active {
            return;
        }
        if let Some(deadline) = self.active_until()
            && deadline < now_ms
        {
            self.sync(now_ms);
        }
        self.last_activity = Some(now_ms);
    }

    /// Updates application focus state (`appDeactivatedValue`).
    pub fn set_app_active(&mut self, active: bool, now_ms: u64) {
        if self.app_active == active {
            return;
        }
        self.app_active = active;
        self.refresh_paused(now_ms);
    }

    /// Updates screen/tab active state (`ReadMetricsTracker::setScreenActive`).
    pub fn set_screen_active(&mut self, active: bool, now_ms: u64) {
        if self.screen_active == active {
            return;
        }
        self.screen_active = active;
        self.refresh_paused(now_ms);
    }

    /// Finalizes all currently tracked items (`ReadMetricsTracker::finalizeAll`), e.g. on peer switch.
    pub fn finalize_all(&mut self, now_ms: u64) -> Vec<FinalizedReadMetric> {
        self.sync(now_ms);
        let drained: Vec<(MsgId, TrackedItem)> = self.tracked.drain().collect();
        for (msg_id, tracked) in drained {
            if let Some(m) = Self::build_finalized(msg_id, &tracked) {
                self.completed.push(m);
            }
        }
        self.currently_visible.clear();
        std::mem::take(&mut self.completed)
    }

    fn active_until(&self) -> Option<u64> {
        self.last_activity
            .map(|t| t.saturating_add(READ_METRICS_ACTIVITY_TIMEOUT_MS))
    }

    fn sync(&mut self, now_ms: u64) {
        let active_until = self.active_until();
        self.process_transitions(now_ms, active_until);
        self.accumulate(now_ms, active_until);
    }

    fn process_transitions(&mut self, now_ms: u64, active_until: Option<u64>) {
        let mut to_remove = Vec::new();
        let mut need_view_id = Vec::new();
        let paused = self.paused;
        let currently_visible = &self.currently_visible;

        for (&msg_id, tracked) in &mut self.tracked {
            if tracked.entry_grace_pending
                && !paused
                && now_ms.saturating_sub(tracked.entry_grace_start) >= READ_METRICS_GRACE_PERIOD_MS
            {
                tracked.entry_grace_pending = false;
                tracked.tracking_started = tracked.entry_grace_start;
                tracked.last_update = tracked.tracking_started;
                need_view_id.push(msg_id);
            }
            if tracked.exit_grace_pending
                && !paused
                && now_ms.saturating_sub(tracked.exit_grace_start) >= READ_METRICS_GRACE_PERIOD_MS
            {
                if let Some(m) = Self::build_finalized(msg_id, tracked) {
                    self.completed.push(m);
                }
                to_remove.push(msg_id);
                continue;
            }
            if tracked.view_id != 0 && !tracked.entry_grace_pending && !tracked.exit_grace_pending {
                let deadline = tracked
                    .tracking_started
                    .saturating_add(READ_METRICS_MAX_TRACKING_DURATION_MS);
                if now_ms >= deadline {
                    if !paused
                        && currently_visible.contains(&msg_id)
                        && tracked.last_update < deadline
                    {
                        Self::add_elapsed(tracked, tracked.last_update, deadline, active_until);
                        tracked.last_update = deadline;
                    }
                    if let Some(m) = Self::build_finalized(msg_id, tracked) {
                        self.completed.push(m);
                    }
                    to_remove.push(msg_id);
                }
            }
        }

        for msg_id in need_view_id {
            let vid = self.next_view_id();
            if let Some(t) = self.tracked.get_mut(&msg_id) {
                t.view_id = vid;
            }
        }
        for msg_id in to_remove {
            self.tracked.remove(&msg_id);
        }
    }

    fn accumulate(&mut self, now_ms: u64, active_until: Option<u64>) {
        let paused = self.paused;
        let currently_visible = &self.currently_visible;
        for (msg_id, tracked) in &mut self.tracked {
            if tracked.view_id == 0
                || tracked.entry_grace_pending
                || tracked.exit_grace_pending
                || tracked.last_update == 0
            {
                continue;
            }
            if paused || !currently_visible.contains(msg_id) {
                tracked.last_update = now_ms;
                continue;
            }
            Self::add_elapsed(tracked, tracked.last_update, now_ms, active_until);
            tracked.last_update = now_ms;
        }
    }

    fn refresh_paused(&mut self, now_ms: u64) {
        let next_paused = !self.app_active || !self.screen_active;
        if self.paused == next_paused {
            return;
        }
        if next_paused {
            let active_until = self.active_until();
            self.process_transitions(now_ms, active_until);
            self.accumulate(now_ms, active_until);
            self.paused_since = now_ms;
            for tracked in self.tracked.values_mut() {
                if tracked.entry_grace_pending || tracked.exit_grace_pending {
                    continue;
                }
                tracked.last_update = now_ms;
            }
        } else {
            let delta = now_ms.saturating_sub(self.paused_since);
            for tracked in self.tracked.values_mut() {
                if tracked.entry_grace_pending {
                    tracked.entry_grace_start = if tracked.entry_grace_start < self.paused_since {
                        tracked.entry_grace_start.saturating_add(delta)
                    } else {
                        now_ms
                    };
                } else if tracked.exit_grace_pending {
                    tracked.exit_grace_start = if tracked.exit_grace_start < self.paused_since {
                        tracked.exit_grace_start.saturating_add(delta)
                    } else {
                        now_ms
                    };
                } else if tracked.view_id != 0 {
                    tracked.last_update = now_ms;
                }
            }
            self.paused_since = 0;
        }
        self.paused = next_paused;
    }

    fn add_elapsed(tracked: &mut TrackedItem, from: u64, till: u64, active_until: Option<u64>) {
        if till <= from {
            return;
        }
        tracked.total_in_view = tracked.total_in_view.saturating_add(till - from);
        if let Some(au) = active_until
            && au > from
        {
            let active_span = till.min(au).saturating_sub(from);
            tracked.active_in_view = tracked
                .active_in_view
                .saturating_add(active_span)
                .min(tracked.total_in_view);
        }
    }

    fn build_finalized(msg_id: MsgId, tracked: &TrackedItem) -> Option<FinalizedReadMetric> {
        if tracked.view_id == 0 || tracked.total_in_view < READ_METRICS_MIN_REPORT_THRESHOLD_MS {
            return None;
        }
        let height_ratio = if tracked.max_viewport_height > 0 {
            ((f64::from(tracked.max_item_height) * 1000.0) / f64::from(tracked.max_viewport_height))
                .round() as i32
        } else {
            0
        };
        let seen_range = if tracked.max_item_height > 0 {
            let span = (tracked.seen_bottom - tracked.seen_top).max(0);
            (((f64::from(span) * 1000.0) / f64::from(tracked.max_item_height)).round() as i32)
                .clamp(0, 1000)
        } else {
            0
        };
        Some(FinalizedReadMetric {
            msg_id,
            view_id: tracked.view_id,
            time_in_view_ms: tracked.total_in_view.min(i32::MAX as u64) as i32,
            active_time_in_view_ms: tracked.active_in_view.min(i32::MAX as u64) as i32,
            height_to_viewport_ratio_permille: height_ratio,
            seen_range_ratio_permille: seen_range,
        })
    }
}

/// Per-session batching queue for `messages.reportReadMetrics` (`Api::ReadMetrics`).
#[derive(Debug, Default)]
pub struct ReadMetricsBatcher {
    pending: HashMap<PeerId, Vec<FinalizedReadMetric>>,
    in_flight: HashSet<PeerId>,
    first_queued_at_ms: Option<u64>,
}

impl ReadMetricsBatcher {
    /// Creates an empty batcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues finalized metrics for `peer_id` (`Api::ReadMetrics::add`).
    pub fn add(&mut self, peer_id: PeerId, metrics: Vec<FinalizedReadMetric>, now_ms: u64) {
        if metrics.is_empty() {
            return;
        }
        self.pending.entry(peer_id).or_default().extend(metrics);
        if self.first_queued_at_ms.is_none() {
            self.first_queued_at_ms = Some(now_ms);
        }
    }

    /// Returns total pending metrics across all peers.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }

    /// Drains ready per-peer batches if `force` is `true` or [`READ_METRICS_SEND_TIMEOUT_MS`]
    /// has elapsed since the first queued metric.
    pub fn drain_ready(
        &mut self,
        now_ms: u64,
        force: bool,
    ) -> Vec<(PeerId, Vec<FinalizedReadMetric>)> {
        let Some(first_ts) = self.first_queued_at_ms else {
            return Vec::new();
        };
        if !force && now_ms.saturating_sub(first_ts) < READ_METRICS_SEND_TIMEOUT_MS {
            return Vec::new();
        }
        let mut ready = Vec::new();
        let peers: Vec<PeerId> = self.pending.keys().copied().collect();
        for peer_id in peers {
            if self.in_flight.contains(&peer_id) {
                continue;
            }
            if let Some(items) = self.pending.remove(&peer_id)
                && !items.is_empty()
            {
                self.in_flight.insert(peer_id);
                ready.push((peer_id, items));
            }
        }
        if self.pending.is_empty() {
            self.first_queued_at_ms = None;
        } else {
            self.first_queued_at_ms = Some(now_ms);
        }
        ready
    }

    /// Marks an in-flight peer request finished (allowing subsequent batches for `peer_id`).
    pub fn finish_peer(&mut self, peer_id: PeerId, now_ms: u64) {
        self.in_flight.remove(&peer_id);
        if !self.pending.is_empty() && self.first_queued_at_ms.is_none() {
            self.first_queued_at_ms = Some(now_ms);
        }
    }
}
