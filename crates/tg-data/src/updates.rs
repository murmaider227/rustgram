//! Port of `data/data_pts_waiter.h`, `data/data_pts_waiter.cpp`,
//! `api/api_updates.h`, and `api/api_updates.cpp` (update ordering & gap state machine).
//!
//! Pure, deterministic, TL-independent state machine managing:
//! - Common account `pts` / `qts` / `seq` / `date` state
//! - Per-channel `pts` waiters and short-polling for active channels
//! - Exact tdesktop gap detection (`last == count`, `last < count` -> 1 ms,
//!   `last > count` -> 1000 ms) and ordered buffering
//! - `seq` gap buffering with 1000 ms timeout and cascading drain
//! - `getDifference` / `getChannelDifference` triggering, slice continuation,
//!   `channelDifferenceTooLong`, and exponential failure backoff (1s..64s)
//! - Explicit millisecond deadlines instead of wall-clock timers for full unit testability.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::id::{ChannelId, TimeId};

/// 1-second wait for skipped `pts` or `seq` in updates (`PtsWaiter::kWaitForSkippedTimeout`).
pub const WAIT_FOR_SKIPPED_TIMEOUT_MS: u64 = 1_000;

/// 1-millisecond fast trigger when `last < count` (`PtsWaiter::check`).
pub const FAST_GAP_TIMEOUT_MS: u64 = 1;

/// 1-second default short-poll interval for active channels (`kWaitForChannelGetDifference`).
pub const WAIT_FOR_CHANNEL_GET_DIFF_MS: u64 = 1_000;

/// 60-second idle timeout before pinging server (`kNoUpdatesTimeout`).
pub const NO_UPDATES_TIMEOUT_MS: u64 = 60_000;

/// 60-second idle timeout after wake from sleep before pinging server
/// (`kNoUpdatesAfterSleepTimeout`).
pub const NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS: u64 = 60_000;

/// Default limit passed to `updates.getChannelDifference` (`kChannelGetDifferenceLimit`).
pub const CHANNEL_GET_DIFF_LIMIT: i32 = 100;

/// Initial failure backoff in seconds (`_failDifferenceTimeout = 1`).
pub const FAIL_DIFF_INITIAL_BACKOFF_SECS: u64 = 1;

/// Maximum failure backoff in seconds (`if (timeout < 64) timeout *= 2`).
pub const FAIL_DIFF_MAX_BACKOFF_SECS: u64 = 64;

/// Binary magic header for [`PersistedUpdatesState`].
const PERSISTED_STATE_MAGIC: &[u8; 4] = b"TGUP";

/// Binary format version for [`PersistedUpdatesState`].
const PERSISTED_STATE_VERSION: u32 = 1;

/// Safety cap on persisted channel PTS entries when deserializing from disk.
const MAX_PERSISTED_CHANNEL_PTS: usize = 100_000;

/// Snapshot of persistent updates state (`pts`, `qts`, `date`, `seq`, and per-channel `pts`)
/// saved across restarts so the client resumes with `updates.getDifference` instead of
/// dropping missed updates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersistedUpdatesState {
    /// Confirmed common account `pts`.
    pub pts: i32,
    /// Confirmed common account `qts` (`-1` if uninitialized).
    pub qts: i32,
    /// Confirmed server `date`.
    pub date: TimeId,
    /// Confirmed `seq` (`0` on fresh session).
    pub seq: i32,
    /// Confirmed per-channel `pts` watermarks.
    pub channel_pts: BTreeMap<ChannelId, i32>,
}

impl PersistedUpdatesState {
    /// Returns `true` if this state has a valid initialized common `pts` (`pts > 0`).
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.pts > 0
    }

    /// Serializes this state into a versioned, CRC32-checked binary buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let count = self.channel_pts.len().min(MAX_PERSISTED_CHANNEL_PTS);
        let mut buf = Vec::with_capacity(28 + count * 12 + 4);
        buf.extend_from_slice(PERSISTED_STATE_MAGIC);
        buf.extend_from_slice(&PERSISTED_STATE_VERSION.to_le_bytes());
        buf.extend_from_slice(&self.pts.to_le_bytes());
        buf.extend_from_slice(&self.qts.to_le_bytes());
        buf.extend_from_slice(&self.date.to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&(count as u32).to_le_bytes());
        for (&channel_id, &pts) in self.channel_pts.iter().take(count) {
            buf.extend_from_slice(&channel_id.0.to_le_bytes());
            buf.extend_from_slice(&pts.to_le_bytes());
        }
        let checksum = crc32fast::hash(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf
    }

    /// Deserializes a versioned, CRC32-checked [`PersistedUpdatesState`] buffer.
    ///
    /// Returns `None` if the data is truncated, corrupted, has an unknown version,
    /// or fails checksum validation.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        // Header (28 bytes) + CRC32 (4 bytes) = 32 bytes minimum.
        if data.len() < 32 {
            return None;
        }
        let payload_len = data.len() - 4;
        let expected_crc = u32::from_le_bytes(data[payload_len..].try_into().ok()?);
        let actual_crc = crc32fast::hash(&data[..payload_len]);
        if expected_crc != actual_crc {
            return None;
        }
        if &data[0..4] != PERSISTED_STATE_MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(data[4..8].try_into().ok()?);
        if version != PERSISTED_STATE_VERSION {
            return None;
        }
        let pts = i32::from_le_bytes(data[8..12].try_into().ok()?);
        let qts = i32::from_le_bytes(data[12..16].try_into().ok()?);
        let date = i32::from_le_bytes(data[16..20].try_into().ok()?);
        let seq = i32::from_le_bytes(data[20..24].try_into().ok()?);
        let count = u32::from_le_bytes(data[24..28].try_into().ok()?) as usize;
        if count > MAX_PERSISTED_CHANNEL_PTS {
            return None;
        }
        let expected_payload_len = 28usize.checked_add(count.checked_mul(12)?)?;
        if payload_len != expected_payload_len {
            return None;
        }
        let mut channel_pts = BTreeMap::new();
        let mut cursor = 28usize;
        for _ in 0..count {
            let cid = u64::from_le_bytes(data[cursor..cursor + 8].try_into().ok()?);
            let ch_pts = i32::from_le_bytes(data[cursor + 8..cursor + 12].try_into().ok()?);
            cursor += 12;
            if ch_pts > 0 {
                channel_pts.insert(ChannelId(cid), ch_pts);
            }
        }
        Some(Self {
            pts,
            qts,
            date,
            seq,
            channel_pts,
        })
    }
}

/// Low-level `pts` gap detector and out-of-order update buffer (`PtsWaiter`).
///
/// Generic over update payload `T` so callers can feed any TL or domain update
/// representation.
#[derive(Clone, Debug)]
pub struct PtsWaiter<T> {
    queue: BTreeMap<u64, T>,
    buffered_spans: BTreeSet<(i32, i32)>,
    good: i32,
    last: i32,
    count: i32,
    requesting: bool,
    waiting_for_skipped: bool,
    waiting_for_short_poll: bool,
    skipped_key: u32,
}

impl<T> Default for PtsWaiter<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Decision produced by `PtsWaiter::ingest`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtsIngestResult<T> {
    /// Update was already applied (`pts <= good && pts_count > 0`) or ignored while requesting.
    Skip,
    /// Apply these payloads immediately in order.
    Apply {
        /// Payloads ready to apply (either the single incoming update or the drained queue).
        payloads: Vec<T>,
        /// Optional timer change (`Some(Some(ms))` = schedule/shorten, `Some(None)` = cancel).
        skipped_timer_ms: Option<Option<u64>>,
    },
    /// Update was buffered waiting for missing `pts`; caller must arm a deadline of `wait_ms`.
    Buffered {
        /// Delay in milliseconds (`1` for `last < count`, `1000` for `last > count`).
        wait_ms: u64,
    },
}

impl<T> PtsWaiter<T> {
    /// Creates an uninitialized `PtsWaiter` (`good = 0`).
    pub fn new() -> Self {
        Self {
            queue: BTreeMap::new(),
            buffered_spans: BTreeSet::new(),
            good: 0,
            last: 0,
            count: 0,
            requesting: false,
            waiting_for_skipped: false,
            waiting_for_short_poll: false,
            skipped_key: 0,
        }
    }

    /// Initializes or resets `pts` state (`PtsWaiter::init`).
    pub fn init(&mut self, pts: i32) {
        self.good = pts;
        self.last = pts;
        self.count = pts;
        self.waiting_for_skipped = false;
        self.clear_skipped();
    }

    /// Returns `true` if `pts` has been initialized (`_good > 0`).
    #[inline]
    pub const fn inited(&self) -> bool {
        self.good > 0
    }

    /// Returns the last contiguous confirmed `pts` (`PtsWaiter::current`).
    #[inline]
    pub const fn current(&self) -> i32 {
        self.good
    }

    /// Returns `true` if a `getDifference` / `getChannelDifference` request is in flight.
    #[inline]
    pub const fn requesting(&self) -> bool {
        self.requesting
    }

    /// Sets the in-flight requesting flag (`PtsWaiter::setRequesting`).
    /// Setting `true` clears any buffered skipped updates.
    pub fn set_requesting(&mut self, requesting: bool) {
        self.requesting = requesting;
        if requesting {
            self.waiting_for_skipped = false;
            self.clear_skipped();
        }
    }

    /// Returns `true` if currently waiting for skipped `pts` updates.
    #[inline]
    pub const fn waiting_for_skipped(&self) -> bool {
        self.waiting_for_skipped
    }

    /// Returns `true` if currently waiting on a channel short-poll timer.
    #[inline]
    pub const fn waiting_for_short_poll(&self) -> bool {
        self.waiting_for_short_poll
    }

    /// Sets the short-poll waiting flag.
    #[inline]
    pub fn set_waiting_for_short_poll(&mut self, waiting: bool) {
        self.waiting_for_short_poll = waiting;
    }

    /// Returns the number of currently buffered out-of-order updates.
    #[inline]
    pub fn buffered_len(&self) -> usize {
        self.queue.len()
    }

    /// Clears the buffered skipped queue (`PtsWaiter::clearSkippedUpdates`).
    pub fn clear_skipped(&mut self) {
        self.queue.clear();
        self.buffered_spans.clear();
    }

    fn next_pts_key(&mut self, pts: i32) -> u64 {
        self.skipped_key = self.skipped_key.wrapping_add(1);
        ((pts as u32 as u64) << 32) | (self.skipped_key as u64)
    }

    /// Processes an incoming update with `(pts, pts_count)` using the exact rules
    /// of `PtsWaiter::updated` + `PtsWaiter::check` + `PtsWaiter::updateAndApply`,
    /// additionally deduplicating exact `(pts, pts_count)` repeats already buffered
    /// during an open gap so duplicate deliveries cannot falsely close a gap.
    pub fn ingest(&mut self, pts: i32, pts_count: i32, payload: T) -> PtsIngestResult<T> {
        if self.requesting {
            return PtsIngestResult::Skip;
        }
        if pts <= self.good && pts_count > 0 {
            return PtsIngestResult::Skip;
        }
        if pts_count > 0 && self.buffered_spans.contains(&(pts, pts_count)) {
            return PtsIngestResult::Skip;
        }
        if !self.inited() {
            self.init(pts);
            return PtsIngestResult::Apply {
                payloads: vec![payload],
                skipped_timer_ms: Some(None),
            };
        }

        self.last = self.last.max(pts);
        self.count = self.count.saturating_add(pts_count);

        if self.last == self.count {
            self.good = self.last;
            if !self.waiting_for_skipped || self.queue.is_empty() {
                let timer_change = if self.waiting_for_skipped {
                    self.waiting_for_skipped = false;
                    Some(None)
                } else {
                    None
                };
                self.buffered_spans.clear();
                PtsIngestResult::Apply {
                    payloads: vec![payload],
                    skipped_timer_ms: timer_change,
                }
            } else {
                let key = self.next_pts_key(pts);
                self.queue.insert(key, payload);
                self.waiting_for_skipped = false;
                self.buffered_spans.clear();
                let payloads = std::mem::take(&mut self.queue).into_values().collect();
                PtsIngestResult::Apply {
                    payloads,
                    skipped_timer_ms: Some(None),
                }
            }
        } else {
            let wait_ms = if self.last < self.count {
                FAST_GAP_TIMEOUT_MS
            } else {
                WAIT_FOR_SKIPPED_TIMEOUT_MS
            };
            self.waiting_for_skipped = true;
            if pts_count == 0 {
                // In C++: `return !count;` -> pts_count == 0 applies immediately
                // while still keeping the skipped timer armed for the gap.
                PtsIngestResult::Apply {
                    payloads: vec![payload],
                    skipped_timer_ms: Some(Some(wait_ms)),
                }
            } else {
                self.buffered_spans.insert((pts, pts_count));
                let key = self.next_pts_key(pts);
                self.queue.insert(key, payload);
                PtsIngestResult::Buffered { wait_ms }
            }
        }
    }
}

/// Incoming envelope for a common-PTS update (`updateNewMessage`, `updateShortMessage`,
/// `updateEditMessage`, `updateDeleteMessages`, `updateReadHistoryInbox`, etc.).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommonUpdateEnvelope<T> {
    /// Update `pts`.
    pub pts: i32,
    /// Update `pts_count` (`0` for non-incrementing updates).
    pub pts_count: i32,
    /// Optional server `date` carried by short updates.
    pub date: Option<TimeId>,
    /// Optional `qts` carried by secret/bot/qts updates.
    pub qts: Option<i32>,
    /// Update payload.
    pub payload: T,
}

/// Incoming envelope for a channel-PTS update (`updateNewChannelMessage`,
/// `updateEditChannelMessage`, `updateDeleteChannelMessages`, etc.).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelUpdateEnvelope<T> {
    /// Target channel ID.
    pub channel_id: ChannelId,
    /// Channel `pts`.
    pub pts: i32,
    /// Channel `pts_count`.
    pub pts_count: i32,
    /// Update payload.
    pub payload: T,
}

/// Buffered sequence-numbered updates container (`updates` or `updatesCombined`).
#[derive(Clone, Debug)]
struct SeqEntry<T> {
    seq_end: i32,
    date: TimeId,
    payload: T,
}

/// Per-channel PTS and retry state.
#[derive(Clone, Debug)]
struct ChannelState<T> {
    waiter: PtsWaiter<T>,
    active: bool,
    fail_backoff_secs: u64,
}

impl<T> Default for ChannelState<T> {
    fn default() -> Self {
        Self {
            waiter: PtsWaiter::new(),
            active: false,
            fail_backoff_secs: FAIL_DIFF_INITIAL_BACKOFF_SECS,
        }
    }
}

/// Output action emitted by `UpdatesStateMachine`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateAction<T> {
    /// Apply a common/seq update payload in order.
    Apply(T),
    /// Apply a channel update payload in order.
    ApplyChannel {
        /// Target channel ID.
        channel_id: ChannelId,
        /// Payload to apply.
        payload: T,
    },
    /// Update was duplicate/stale or ignored while a difference request is in flight.
    Skip,
    /// Update was buffered waiting for missing `pts` or `seq`.
    Buffer,
    /// Caller should invoke `updates.getDifference(pts, date, qts)`.
    RequestDifference {
        /// Local `pts`.
        pts: i32,
        /// Local `date`.
        date: TimeId,
        /// Local `qts`.
        qts: i32,
    },
    /// Caller should invoke `updates.getChannelDifference(channel_id, pts, force, limit)`.
    RequestChannelDifference {
        /// Target channel ID.
        channel_id: ChannelId,
        /// Local channel `pts`.
        pts: i32,
        /// `true` on PTS gap/retry, `false` on routine active-chat short poll.
        force: bool,
        /// Maximum updates/messages requested (`CHANNEL_GET_DIFF_LIMIT = 100`).
        limit: i32,
    },
    /// Server returned `updates.channelDifferenceTooLong`: caller should mark
    /// the channel history not-loaded-at-bottom and reload its dialog/topics.
    ChannelTooLongReset {
        /// Target channel ID.
        channel_id: ChannelId,
    },
    /// Server returned `updates.differenceTooLong`: common `pts` was reset to `pts`
    /// and callers should refresh dialogs / visible histories.
    DifferenceTooLongReset {
        /// New server `pts` watermark.
        pts: i32,
    },
}

/// Pure, deterministic Telegram updates state machine (`Api::Updates` + `PtsWaiter`).
///
/// Time is passed explicitly as monotonic milliseconds `now_ms: u64`. Inspect
/// `next_deadline()` to schedule a single wake-up timer, then call `on_tick(now_ms)`.
#[derive(Clone, Debug)]
pub struct UpdatesStateMachine<T> {
    pts_waiter: PtsWaiter<T>,
    date: TimeId,
    qts: i32,
    seq: i32,
    by_seq_updates: BTreeMap<i32, SeqEntry<T>>,
    seq_deadline: Option<u64>,
    qts_queue: BTreeMap<i32, T>,
    qts_deadline: Option<u64>,
    min_channel_deadline: Option<u64>,
    common_pts_deadline: Option<u64>,
    common_fail_deadline: Option<u64>,
    common_fail_backoff_secs: u64,
    channels: HashMap<ChannelId, ChannelState<T>>,
    channel_pts_deadlines: BTreeMap<ChannelId, u64>,
    channel_fail_deadlines: BTreeMap<ChannelId, u64>,
    last_update_ms: Option<u64>,
}

impl<T> Default for UpdatesStateMachine<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> UpdatesStateMachine<T> {
    /// Creates a new uninitialized `UpdatesStateMachine`.
    ///
    /// Matches `Api::Updates` constructor: `_updatesQts = -1`, `_ptsWaiter.setRequesting(true)`
    /// until `init_state` is called with the result of `updates.getState`.
    pub fn new() -> Self {
        let mut pts_waiter = PtsWaiter::new();
        pts_waiter.set_requesting(true);
        Self {
            pts_waiter,
            date: 0,
            qts: -1,
            seq: 0,
            by_seq_updates: BTreeMap::new(),
            seq_deadline: None,
            qts_queue: BTreeMap::new(),
            qts_deadline: None,
            min_channel_deadline: None,
            common_pts_deadline: None,
            common_fail_deadline: None,
            common_fail_backoff_secs: FAIL_DIFF_INITIAL_BACKOFF_SECS,
            channels: HashMap::new(),
            channel_pts_deadlines: BTreeMap::new(),
            channel_fail_deadlines: BTreeMap::new(),
            last_update_ms: None,
        }
    }

    /// Returns the current confirmed common `pts`.
    #[inline]
    pub const fn pts(&self) -> i32 {
        self.pts_waiter.current()
    }

    /// Returns the current server `date`.
    #[inline]
    pub const fn date(&self) -> TimeId {
        self.date
    }

    /// Returns the current `qts`.
    #[inline]
    pub const fn qts(&self) -> i32 {
        self.qts
    }

    /// Returns the current `seq`.
    #[inline]
    pub const fn seq(&self) -> i32 {
        self.seq
    }

    /// Returns `true` if a common `getDifference` request is currently in flight.
    #[inline]
    pub const fn requesting_difference(&self) -> bool {
        self.pts_waiter.requesting()
    }

    /// Returns the confirmed `pts` for `channel_id`, or `None` if uninitialized.
    pub fn channel_pts(&self, channel_id: ChannelId) -> Option<i32> {
        self.channels
            .get(&channel_id)
            .filter(|c| c.waiter.inited())
            .map(|c| c.waiter.current())
    }

    /// Returns `true` if `getChannelDifference` is currently in flight for `channel_id`.
    pub fn channel_requesting(&self, channel_id: ChannelId) -> bool {
        self.channels
            .get(&channel_id)
            .is_some_and(|c| c.waiter.requesting())
    }

    /// Returns the earliest pending timer deadline across all PTS, seq, qts, min-channel,
    /// short-poll, and failure-retry timers, or `None` if no timers are armed.
    pub fn next_deadline(&self) -> Option<u64> {
        let mut min: Option<u64> = None;
        for candidate in [
            self.common_pts_deadline,
            self.seq_deadline,
            self.qts_deadline,
            self.min_channel_deadline,
            self.common_fail_deadline,
        ]
        .into_iter()
        .flatten()
        .chain(self.channel_pts_deadlines.values().copied())
        .chain(self.channel_fail_deadlines.values().copied())
        {
            min = Some(min.map_or(candidate, |cur| cur.min(candidate)));
        }
        min
    }

    /// Returns `true` if no updates have been received for at least `NO_UPDATES_TIMEOUT_MS`
    /// (`Updates::checkLastUpdate`).
    pub fn needs_ping(&self, now_ms: u64) -> bool {
        self.last_update_ms
            .is_some_and(|last| now_ms.saturating_sub(last) > NO_UPDATES_TIMEOUT_MS)
    }

    /// Checks whether an idle keepalive ping should be sent (`Updates::checkLastUpdate`),
    /// advancing `last_update_ms` to `now_ms` when returning `true` so pings are spaced by
    /// `NO_UPDATES_TIMEOUT_MS` / `NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS`.
    pub fn check_last_update(&mut self, after_sleep: bool, now_ms: u64) -> bool {
        let threshold = if after_sleep {
            NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS
        } else {
            NO_UPDATES_TIMEOUT_MS
        };
        if self
            .last_update_ms
            .is_some_and(|last| now_ms.saturating_sub(last) > threshold)
        {
            self.last_update_ms = Some(now_ms);
            true
        } else {
            false
        }
    }

    /// Records that a keepalive ping was sent at `now_ms`.
    pub fn mark_ping_sent(&mut self, now_ms: u64) {
        self.last_update_ms = Some(now_ms);
    }

    /// Returns currently active channels enrolled for short-polling.
    pub fn active_channels(&self) -> Vec<ChannelId> {
        self.channels
            .iter()
            .filter_map(|(&id, st)| st.active.then_some(id))
            .collect()
    }

    /// Exports a persistent snapshot of `(pts, qts, date, seq)` and all initialized
    /// per-channel `pts` values for saving across restarts.
    pub fn export_persisted_state(&self) -> PersistedUpdatesState {
        let channel_pts = self
            .channels
            .iter()
            .filter_map(|(&id, st)| st.waiter.inited().then_some((id, st.waiter.current())))
            .collect();
        PersistedUpdatesState {
            pts: self.pts_waiter.current(),
            qts: self.qts,
            date: self.date,
            seq: self.seq,
            channel_pts,
        }
    }

    /// Restores a previously persisted state snapshot so the caller can resume from
    /// saved watermarks via `updates.getDifference` without losing missed updates.
    pub fn restore_persisted_state(&mut self, state: &PersistedUpdatesState, now_ms: u64) {
        if state.pts > 0 {
            self.pts_waiter.init(state.pts);
        }
        self.date = state.date;
        self.qts = if state.qts > 0 { state.qts } else { -1 };
        self.seq = state.seq;
        self.by_seq_updates.clear();
        self.seq_deadline = None;
        self.qts_queue.clear();
        self.qts_deadline = None;
        self.min_channel_deadline = None;
        self.common_pts_deadline = None;
        self.common_fail_deadline = None;
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        for (&channel_id, &ch_pts) in &state.channel_pts {
            if ch_pts > 0 {
                let entry = self.channels.entry(channel_id).or_default();
                entry.waiter.init(ch_pts);
                entry.waiter.set_requesting(false);
                entry.fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
            }
        }
        self.last_update_ms = Some(now_ms);
        self.pts_waiter.set_requesting(false);
    }

    /// Applies the initial `updates.state` response (`Updates::stateDone`).
    pub fn init_state(
        &mut self,
        pts: i32,
        date: TimeId,
        qts: i32,
        seq: i32,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let mut actions = Vec::new();
        self.min_channel_deadline = None;
        self.qts_queue.clear();
        self.qts_deadline = None;
        self.set_state(pts, date, qts, seq, now_ms, &mut actions);
        self.last_update_ms = Some(now_ms);
        self.pts_waiter.set_requesting(false);
        self.common_pts_deadline = None;
        self.common_fail_deadline = None;
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        actions
    }

    /// Advances the tracked server `date` if newer and no min-channel difference timer
    /// is pending (`Updates::setState` date rule, `api_updates.cpp:367`).
    pub fn advance_date(&mut self, date: TimeId) {
        if self.date < date && self.min_channel_deadline.is_none() {
            self.date = date;
        }
    }

    /// Arms `_byMinChannelTimer` (1 s) when a channel update arrives for an unloaded or
    /// `min`-only channel (`api_updates.cpp:1671,1770`).
    ///
    /// While this timer is active, `self.date` will not advance so the ensuing
    /// `updates.getDifference` starts from the earlier date.
    pub fn on_missing_channel(&mut self, now_ms: u64) {
        self.last_update_ms = Some(now_ms);
        if self.pts_waiter.requesting() {
            return;
        }
        if self.min_channel_deadline.is_none() {
            self.min_channel_deadline = Some(now_ms.saturating_add(WAIT_FOR_SKIPPED_TIMEOUT_MS));
        }
    }

    /// Initializes `pts` for a channel (`ChannelData::ptsInit`, `data/data_channel.cpp`).
    ///
    /// If the channel was already marked active via [`Self::set_active_channel`] before `pts`
    /// became known (and its initial short-poll timer already elapsed while uninitialized),
    /// re-arms the short-poll deadline so active-channel `updates.getChannelDifference` polling
    /// resumes (`Updates::addActiveChat`, `api/api_updates.cpp:780-783`).
    pub fn init_channel_pts(&mut self, channel_id: ChannelId, pts: i32) {
        let state = self.channels.entry(channel_id).or_default();
        state.waiter.init(pts);
        if state.active && !state.waiter.requesting() {
            state.waiter.set_waiting_for_short_poll(true);
            let deadline = self
                .last_update_ms
                .unwrap_or(0)
                .saturating_add(WAIT_FOR_CHANNEL_GET_DIFF_MS);
            Self::schedule_min(&mut self.channel_pts_deadlines, channel_id, deadline);
        } else if !state.waiter.waiting_for_short_poll() {
            self.channel_pts_deadlines.remove(&channel_id);
        }
    }

    /// Marks whether a channel is currently open/active for short-polling
    /// (`Updates::addActiveChat`).
    pub fn set_active_channel(&mut self, channel_id: ChannelId, active: bool, now_ms: u64) {
        let state = self.channels.entry(channel_id).or_default();
        if state.active == active {
            return;
        }
        state.active = active;
        if active {
            state.waiter.set_waiting_for_short_poll(true);
            Self::schedule_min(
                &mut self.channel_pts_deadlines,
                channel_id,
                now_ms.saturating_add(WAIT_FOR_CHANNEL_GET_DIFF_MS),
            );
        } else {
            state.waiter.set_waiting_for_short_poll(false);
            if !state.waiter.waiting_for_skipped() {
                self.channel_pts_deadlines.remove(&channel_id);
            }
        }
    }

    /// Handles `new_session_created` from MTProto (`Updates::mtpNewSessionCreated`).
    pub fn on_new_session_created(&mut self) -> Vec<UpdateAction<T>> {
        self.seq = 0;
        let mut actions = Vec::new();
        self.trigger_get_difference(&mut actions);
        actions
    }

    /// Handles a transport reconnect, sleep wake-up, or network change:
    /// resets `seq = 0`, triggers common `updates.getDifference`, and triggers
    /// `updates.getChannelDifference` for every active initialized channel.
    pub fn on_reconnect(&mut self, now_ms: u64) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        self.seq = 0;
        let mut actions = Vec::new();
        self.trigger_get_difference(&mut actions);
        let active: Vec<ChannelId> = self
            .channels
            .iter()
            .filter_map(|(&id, st)| (st.active && st.waiter.inited()).then_some(id))
            .collect();
        for channel_id in active {
            self.trigger_channel_difference(channel_id, false, &mut actions);
        }
        actions
    }

    /// Handles `updatesTooLong` (`Updates::applyUpdates`).
    pub fn on_updates_too_long(&mut self, now_ms: u64) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        let mut actions = Vec::new();
        self.trigger_get_difference(&mut actions);
        actions
    }

    /// Handles `updateChannelTooLong` (`Updates::feedUpdate`).
    pub fn on_channel_too_long(
        &mut self,
        channel_id: ChannelId,
        server_pts: Option<i32>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        let mut actions = Vec::new();
        let should_fetch = self.channels.get(&channel_id).is_some_and(|ch| {
            ch.waiter.inited() && server_pts.is_none_or(|spts| ch.waiter.current() < spts)
        });
        if should_fetch {
            self.trigger_channel_difference(channel_id, false, &mut actions);
        }
        actions
    }

    /// Ingests a common-PTS update (`updateNewMessage`, `updateShortMessage`,
    /// `updateDeleteMessages`, etc.).
    pub fn on_common_update(
        &mut self,
        envelope: CommonUpdateEnvelope<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        let mut actions = Vec::new();
        match self
            .pts_waiter
            .ingest(envelope.pts, envelope.pts_count, envelope.payload)
        {
            PtsIngestResult::Skip => {
                actions.push(UpdateAction::Skip);
            }
            PtsIngestResult::Apply {
                payloads,
                skipped_timer_ms,
            } => {
                if let Some(timer) = skipped_timer_ms {
                    match timer {
                        Some(ms) => Self::schedule_min_opt(
                            &mut self.common_pts_deadline,
                            now_ms.saturating_add(ms),
                        ),
                        None => self.common_pts_deadline = None,
                    }
                }
                for payload in payloads {
                    actions.push(UpdateAction::Apply(payload));
                }
                if let Some(d) = envelope.date
                    && self.date < d
                    && self.min_channel_deadline.is_none()
                {
                    self.date = d;
                }
                if let Some(q) = envelope.qts
                    && q != 0
                    && self.qts < q
                {
                    self.qts = q;
                }
            }
            PtsIngestResult::Buffered { wait_ms } => {
                Self::schedule_min_opt(
                    &mut self.common_pts_deadline,
                    now_ms.saturating_add(wait_ms),
                );
                actions.push(UpdateAction::Buffer);
            }
        }
        actions
    }

    /// Ingests a `qts`-numbered update (`updateNewEncryptedMessage`, `updateChatParticipant`,
    /// `updateChannelParticipant`, `updateBot*`, etc.) with dedupe, ordering, and 1 s gap
    /// recovery via `updates.getDifference`.
    pub fn on_qts_update(&mut self, qts: i32, payload: T, now_ms: u64) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        if self.pts_waiter.requesting() {
            return vec![UpdateAction::Skip];
        }
        if qts <= 0 {
            return vec![UpdateAction::Apply(payload)];
        }
        if self.qts > 0 && qts <= self.qts {
            return vec![UpdateAction::Skip];
        }
        if self.qts <= 0 || qts == self.qts + 1 {
            self.qts = qts;
            let mut actions = vec![UpdateAction::Apply(payload)];
            self.drain_qts_queue(now_ms, &mut actions);
            return actions;
        }
        if self.qts_queue.contains_key(&qts) {
            return vec![UpdateAction::Skip];
        }
        self.qts_queue.insert(qts, payload);
        Self::schedule_min_opt(
            &mut self.qts_deadline,
            now_ms.saturating_add(WAIT_FOR_SKIPPED_TIMEOUT_MS),
        );
        vec![UpdateAction::Buffer]
    }

    /// Ingests a sequence-numbered container (`updates` where `seq_start == seq_end`,
    /// or `updatesCombined` where `seq_start..=seq_end`) (`Updates::applyUpdates`).
    pub fn on_seq_updates(
        &mut self,
        seq_start: i32,
        seq_end: i32,
        date: TimeId,
        payload: T,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        if self.pts_waiter.requesting() {
            return vec![UpdateAction::Skip];
        }
        let mut actions = Vec::new();
        if seq_start != 0 {
            if seq_start <= self.seq {
                return vec![UpdateAction::Skip];
            }
            if seq_start > self.seq + 1 {
                if self.by_seq_updates.contains_key(&seq_start) {
                    return vec![UpdateAction::Skip];
                }
                self.by_seq_updates.insert(
                    seq_start,
                    SeqEntry {
                        seq_end,
                        date,
                        payload,
                    },
                );
                Self::schedule_min_opt(
                    &mut self.seq_deadline,
                    now_ms.saturating_add(WAIT_FOR_SKIPPED_TIMEOUT_MS),
                );
                return vec![UpdateAction::Buffer];
            }
        }

        actions.push(UpdateAction::Apply(payload));
        self.set_state(0, date, self.qts, seq_end, now_ms, &mut actions);
        actions
    }

    /// Ingests a per-channel PTS update (`updateNewChannelMessage`,
    /// `updateEditChannelMessage`, `updateDeleteChannelMessages`, etc.).
    pub fn on_channel_update(
        &mut self,
        envelope: ChannelUpdateEnvelope<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.last_update_ms = Some(now_ms);
        if self.pts_waiter.requesting() {
            return vec![UpdateAction::Skip];
        }
        self.ingest_channel_envelope(envelope, now_ms)
    }

    /// Ingests a per-channel PTS update arriving inside a common `updates.getDifference`
    /// response while common `requesting_difference()` is still active.
    pub fn on_channel_update_from_difference(
        &mut self,
        envelope: ChannelUpdateEnvelope<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.ingest_channel_envelope(envelope, now_ms)
    }

    fn ingest_channel_envelope(
        &mut self,
        envelope: ChannelUpdateEnvelope<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let channel_id = envelope.channel_id;
        let state = self.channels.entry(channel_id).or_default();
        let mut actions = Vec::new();
        match state
            .waiter
            .ingest(envelope.pts, envelope.pts_count, envelope.payload)
        {
            PtsIngestResult::Skip => {
                actions.push(UpdateAction::Skip);
            }
            PtsIngestResult::Apply {
                payloads,
                skipped_timer_ms,
            } => {
                if let Some(timer) = skipped_timer_ms {
                    match timer {
                        Some(ms) => Self::schedule_min(
                            &mut self.channel_pts_deadlines,
                            channel_id,
                            now_ms.saturating_add(ms),
                        ),
                        None => {
                            if !state.waiter.waiting_for_short_poll() {
                                self.channel_pts_deadlines.remove(&channel_id);
                            }
                        }
                    }
                }
                for payload in payloads {
                    actions.push(UpdateAction::ApplyChannel {
                        channel_id,
                        payload,
                    });
                }
            }
            PtsIngestResult::Buffered { wait_ms } => {
                Self::schedule_min(
                    &mut self.channel_pts_deadlines,
                    channel_id,
                    now_ms.saturating_add(wait_ms),
                );
                actions.push(UpdateAction::Buffer);
            }
        }
        actions
    }

    /// Advances time to `now_ms`, firing any expired PTS gap, seq gap, qts gap,
    /// min-channel, short-poll, or failure-retry deadlines.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<UpdateAction<T>> {
        let mut actions = Vec::new();

        // 1. Common PTS gap timer.
        if self.common_pts_deadline.is_some_and(|d| d <= now_ms) {
            self.common_pts_deadline = None;
            self.trigger_get_difference(&mut actions);
        }

        // 2. Seq gap timer.
        if self.seq_deadline.is_some_and(|d| d <= now_ms) {
            self.seq_deadline = None;
            self.trigger_get_difference(&mut actions);
        }

        // 3. QTS gap timer.
        if self.qts_deadline.is_some_and(|d| d <= now_ms) {
            self.qts_deadline = None;
            self.trigger_get_difference(&mut actions);
        }

        // 4. Missing/min channel difference timer (`_byMinChannelTimer`).
        if self.min_channel_deadline.is_some_and(|d| d <= now_ms) {
            self.min_channel_deadline = None;
            self.trigger_get_difference(&mut actions);
        }

        // 5. Common getDifference failure backoff timer.
        if self.common_fail_deadline.is_some_and(|d| d <= now_ms) {
            self.common_fail_deadline = None;
            self.pts_waiter.set_requesting(false);
            self.trigger_get_difference(&mut actions);
        }

        // 6. Per-channel PTS gap / short-poll timers.
        let due_channels: Vec<ChannelId> = self
            .channel_pts_deadlines
            .iter()
            .filter_map(|(&id, &deadline)| (deadline <= now_ms).then_some(id))
            .collect();
        for channel_id in due_channels {
            self.channel_pts_deadlines.remove(&channel_id);
            self.trigger_channel_difference(channel_id, true, &mut actions);
        }

        // 7. Per-channel getChannelDifference failure backoff timers.
        let due_fail_channels: Vec<ChannelId> = self
            .channel_fail_deadlines
            .iter()
            .filter_map(|(&id, &deadline)| (deadline <= now_ms).then_some(id))
            .collect();
        for channel_id in due_fail_channels {
            self.channel_fail_deadlines.remove(&channel_id);
            if let Some(ch) = self.channels.get_mut(&channel_id) {
                ch.waiter.set_requesting(false);
            }
            self.trigger_channel_difference(channel_id, false, &mut actions);
        }

        actions
    }

    /// Handles `updates.differenceEmpty` (`Updates::differenceDone`).
    pub fn on_difference_empty(
        &mut self,
        date: TimeId,
        seq: i32,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        let mut actions = Vec::new();
        let current_pts = self.pts_waiter.current();
        self.set_state(current_pts, date, self.qts, seq, now_ms, &mut actions);
        self.last_update_ms = Some(now_ms);
        self.pts_waiter.set_requesting(false);
        actions
    }

    /// Handles `updates.differenceSlice` (`Updates::differenceDone`).
    ///
    /// Emits all slice payloads as `Apply`, advances state to `intermediate_*`,
    /// and immediately emits the next `RequestDifference`.
    pub fn on_difference_slice(
        &mut self,
        intermediate_pts: i32,
        intermediate_date: TimeId,
        intermediate_qts: i32,
        intermediate_seq: i32,
        payloads: Vec<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        let mut actions: Vec<UpdateAction<T>> =
            payloads.into_iter().map(UpdateAction::Apply).collect();
        self.set_state(
            intermediate_pts,
            intermediate_date,
            intermediate_qts,
            intermediate_seq,
            now_ms,
            &mut actions,
        );
        self.pts_waiter.set_requesting(false);
        self.trigger_get_difference(&mut actions);
        actions
    }

    /// Handles final `updates.difference` (`Updates::differenceDone`).
    pub fn on_difference_final(
        &mut self,
        pts: i32,
        date: TimeId,
        qts: i32,
        seq: i32,
        payloads: Vec<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        let mut actions: Vec<UpdateAction<T>> =
            payloads.into_iter().map(UpdateAction::Apply).collect();
        self.set_state(pts, date, qts, seq, now_ms, &mut actions);
        self.last_update_ms = Some(now_ms);
        self.pts_waiter.set_requesting(false);
        actions
    }

    /// Handles `updates.differenceTooLong`: resets local common `pts` to `pts`,
    /// clears pending gap buffers, and emits [`UpdateAction::DifferenceTooLongReset`].
    pub fn on_difference_too_long(&mut self, pts: i32, now_ms: u64) -> Vec<UpdateAction<T>> {
        self.common_fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        self.by_seq_updates.clear();
        self.seq_deadline = None;
        self.qts_queue.clear();
        self.qts_deadline = None;
        self.min_channel_deadline = None;
        self.common_pts_deadline = None;
        self.common_fail_deadline = None;
        if pts != 0 {
            self.pts_waiter.init(pts);
        }
        self.last_update_ms = Some(now_ms);
        self.pts_waiter.set_requesting(false);
        vec![UpdateAction::DifferenceTooLongReset { pts }]
    }

    /// Handles RPC failure of `updates.getDifference` (`Updates::differenceFail`).
    ///
    /// Schedules exponential retry backoff: 1s, 2s, 4s, ..., capped at 64s.
    pub fn on_difference_fail(&mut self, now_ms: u64) {
        let wait_ms = self.common_fail_backoff_secs.saturating_mul(1_000);
        Self::schedule_min_opt(
            &mut self.common_fail_deadline,
            now_ms.saturating_add(wait_ms),
        );
        if self.common_fail_backoff_secs < FAIL_DIFF_MAX_BACKOFF_SECS {
            self.common_fail_backoff_secs *= 2;
        }
    }

    /// Handles `updates.channelDifferenceEmpty` (`Updates::channelDifferenceDone`).
    pub fn on_channel_difference_empty(
        &mut self,
        channel_id: ChannelId,
        pts: i32,
        is_final: bool,
        timeout_secs: Option<i32>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let state = self.channels.entry(channel_id).or_default();
        state.fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        state.waiter.init(pts);
        state.waiter.set_requesting(false);
        self.finish_channel_difference(channel_id, is_final, timeout_secs, now_ms)
    }

    /// Handles `updates.channelDifference` (`Updates::channelDifferenceDone`).
    pub fn on_channel_difference(
        &mut self,
        channel_id: ChannelId,
        pts: i32,
        is_final: bool,
        timeout_secs: Option<i32>,
        payloads: Vec<T>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let mut actions: Vec<UpdateAction<T>> = payloads
            .into_iter()
            .map(|payload| UpdateAction::ApplyChannel {
                channel_id,
                payload,
            })
            .collect();
        let state = self.channels.entry(channel_id).or_default();
        state.fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        state.waiter.init(pts);
        state.waiter.set_requesting(false);
        actions.extend(self.finish_channel_difference(channel_id, is_final, timeout_secs, now_ms));
        actions
    }

    /// Handles `updates.channelDifferenceTooLong` (`Updates::channelDifferenceDone`).
    pub fn on_channel_difference_too_long(
        &mut self,
        channel_id: ChannelId,
        dialog_pts: Option<i32>,
        is_final: bool,
        timeout_secs: Option<i32>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let state = self.channels.entry(channel_id).or_default();
        state.fail_backoff_secs = FAIL_DIFF_INITIAL_BACKOFF_SECS;
        if let Some(pts) = dialog_pts {
            state.waiter.init(pts);
        }
        state.waiter.set_requesting(false);
        let mut actions = vec![UpdateAction::ChannelTooLongReset { channel_id }];
        actions.extend(self.finish_channel_difference(channel_id, is_final, timeout_secs, now_ms));
        actions
    }

    /// Handles RPC failure of `updates.getChannelDifference` (`Updates::channelDifferenceFail`).
    pub fn on_channel_difference_fail(&mut self, channel_id: ChannelId, now_ms: u64) {
        let state = self.channels.entry(channel_id).or_default();
        let wait_ms = state.fail_backoff_secs.saturating_mul(1_000);
        Self::schedule_min(
            &mut self.channel_fail_deadlines,
            channel_id,
            now_ms.saturating_add(wait_ms),
        );
        if state.fail_backoff_secs < FAIL_DIFF_MAX_BACKOFF_SECS {
            state.fail_backoff_secs *= 2;
        }
    }

    /// Clears all tracked PTS state and pending gap/short-poll/backoff timers for `channel_id`
    /// (e.g. after `channels.deleteChannel` or `400 CHANNEL_PRIVATE` / `CHANNEL_INVALID`,
    /// preventing infinite `getChannelDifference` retry loops on deleted/forbidden channels;
    /// corresponds to clearing `_whenGetDiffByPts`/`_whenGetDiffAfterFail` in
    /// `Updates::channelDifferenceFail` / `Updates::getChannelDifference`, `api/api_updates.cpp:470-478, 725-760`).
    pub fn remove_channel(&mut self, channel_id: ChannelId) {
        self.channels.remove(&channel_id);
        self.channel_pts_deadlines.remove(&channel_id);
        self.channel_fail_deadlines.remove(&channel_id);
    }

    fn finish_channel_difference(
        &mut self,
        channel_id: ChannelId,
        is_final: bool,
        timeout_secs: Option<i32>,
        now_ms: u64,
    ) -> Vec<UpdateAction<T>> {
        let mut actions = Vec::new();
        if !is_final {
            self.trigger_channel_difference(channel_id, false, &mut actions);
        } else if let Some(state) = self.channels.get_mut(&channel_id) {
            if state.active {
                let delay_ms = timeout_secs
                    .filter(|&s| s > 0)
                    .map_or(WAIT_FOR_CHANNEL_GET_DIFF_MS, |s| (s as u64) * 1_000);
                state.waiter.set_waiting_for_short_poll(true);
                Self::schedule_min(
                    &mut self.channel_pts_deadlines,
                    channel_id,
                    now_ms.saturating_add(delay_ms),
                );
            } else {
                state.waiter.set_waiting_for_short_poll(false);
                if !state.waiter.waiting_for_skipped() {
                    self.channel_pts_deadlines.remove(&channel_id);
                }
            }
        }
        actions
    }

    fn set_state(
        &mut self,
        pts: i32,
        date: TimeId,
        qts: i32,
        seq: i32,
        now_ms: u64,
        actions: &mut Vec<UpdateAction<T>>,
    ) {
        if pts != 0 {
            self.pts_waiter.init(pts);
            self.common_pts_deadline = None;
        }
        if self.date < date && self.min_channel_deadline.is_none() {
            self.date = date;
        }
        if qts != 0 && self.qts < qts {
            self.qts = qts;
            self.drain_qts_queue(now_ms, actions);
        }
        if seq != 0 && seq != self.seq {
            let mut cur_seq = seq;
            self.seq = cur_seq;
            self.seq_deadline = None;
            while let Some(&first_seq) = self.by_seq_updates.keys().next() {
                if first_seq <= cur_seq + 1 {
                    let Some(entry) = self.by_seq_updates.remove(&first_seq) else {
                        break;
                    };
                    if first_seq == cur_seq + 1 {
                        actions.push(UpdateAction::Apply(entry.payload));
                        if self.date < entry.date && self.min_channel_deadline.is_none() {
                            self.date = entry.date;
                        }
                        if entry.seq_end != 0 {
                            cur_seq = entry.seq_end;
                            self.seq = cur_seq;
                        }
                    }
                } else {
                    Self::schedule_min_opt(
                        &mut self.seq_deadline,
                        now_ms.saturating_add(WAIT_FOR_SKIPPED_TIMEOUT_MS),
                    );
                    break;
                }
            }
        }
    }

    fn drain_qts_queue(&mut self, now_ms: u64, actions: &mut Vec<UpdateAction<T>>) {
        self.qts_deadline = None;
        while let Some(&first_qts) = self.qts_queue.keys().next() {
            if first_qts <= self.qts + 1 {
                let Some(payload) = self.qts_queue.remove(&first_qts) else {
                    break;
                };
                if first_qts == self.qts + 1 {
                    self.qts = first_qts;
                    actions.push(UpdateAction::Apply(payload));
                }
            } else {
                Self::schedule_min_opt(
                    &mut self.qts_deadline,
                    now_ms.saturating_add(WAIT_FOR_SKIPPED_TIMEOUT_MS),
                );
                break;
            }
        }
    }

    fn trigger_get_difference(&mut self, actions: &mut Vec<UpdateAction<T>>) {
        self.common_pts_deadline = None;
        if self.pts_waiter.requesting() {
            return;
        }
        self.by_seq_updates.clear();
        self.seq_deadline = None;
        self.qts_queue.clear();
        self.qts_deadline = None;
        self.min_channel_deadline = None;
        self.common_fail_deadline = None;
        self.pts_waiter.set_requesting(true);
        actions.push(UpdateAction::RequestDifference {
            pts: self.pts_waiter.current(),
            date: self.date,
            qts: self.qts,
        });
    }

    fn trigger_channel_difference(
        &mut self,
        channel_id: ChannelId,
        from_pts_timer: bool,
        actions: &mut Vec<UpdateAction<T>>,
    ) {
        if !from_pts_timer {
            self.channel_pts_deadlines.remove(&channel_id);
        }
        let Some(state) = self.channels.get_mut(&channel_id) else {
            return;
        };
        if !state.waiter.inited() || state.waiter.requesting() {
            return;
        }
        self.channel_fail_deadlines.remove(&channel_id);
        // Match `Updates::getChannelDifference` (`api/api_updates.cpp:743-748`):
        // `flags` starts with `f_force`; when `from != PtsGapOrShortPoll` (`!from_pts_timer`)
        // and `!channel->ptsWaitingForSkipped()`, `flags` is cleared to `0`.
        let force = if from_pts_timer {
            true
        } else {
            state.waiter.waiting_for_skipped()
        };
        let pts = state.waiter.current();
        state.waiter.set_requesting(true);
        actions.push(UpdateAction::RequestChannelDifference {
            channel_id,
            pts,
            force,
            limit: CHANNEL_GET_DIFF_LIMIT,
        });
    }

    fn schedule_min_opt(slot: &mut Option<u64>, when: u64) {
        *slot = Some(slot.map_or(when, |cur| cur.min(when)));
    }

    fn schedule_min(map: &mut BTreeMap<ChannelId, u64>, channel_id: ChannelId, when: u64) {
        map.entry(channel_id)
            .and_modify(|cur| {
                if when < *cur {
                    *cur = when;
                }
            })
            .or_insert(when);
    }
}
