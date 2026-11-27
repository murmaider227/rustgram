//! A/V synchronization clock and decoded frame queue with late-frame dropping.
//!
//! Port of timing and frame ring logic from
//! `Telegram/SourceFiles/media/streaming/media_streaming_video_track.cpp`.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::types::{SPEED_MAX, SPEED_MIN, TIME_UNKNOWN, VideoFrame};

/// Default decoded frame queue capacity (4 slots matching `VideoTrack::Shared::kFramesCount`).
pub const DEFAULT_FRAME_QUEUE_CAPACITY: usize = 4;

/// A synchronized `(track_time_ms, world_time_ms)` anchor point (mirrors `TimePoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePoint {
    /// Position along the media timeline in milliseconds.
    pub track_time_ms: i64,
    /// Monotonic wall-clock timestamp in milliseconds.
    pub world_time_ms: i64,
}

impl Default for TimePoint {
    fn default() -> Self {
        Self {
            track_time_ms: TIME_UNKNOWN,
            world_time_ms: TIME_UNKNOWN,
        }
    }
}

impl TimePoint {
    /// Creates a new time point.
    #[must_use]
    pub const fn new(track_time_ms: i64, world_time_ms: i64) -> Self {
        Self {
            track_time_ms,
            world_time_ms,
        }
    }

    /// Returns `true` if both `track_time_ms` and `world_time_ms` are known.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.track_time_ms != TIME_UNKNOWN && self.world_time_ms != TIME_UNKNOWN
    }
}

/// Converts a media track delay into wall-clock milliseconds given playback `speed`
/// (mirrors `WorldTimeDelay` in `media_streaming_video_track.cpp`).
#[must_use]
pub fn world_time_delay(track_delay_ms: i64, speed: f64) -> i64 {
    if track_delay_ms <= 0 || speed <= 0.0 || (speed - 1.0).abs() < f64::EPSILON {
        return track_delay_ms;
    }
    let adjusted = ((track_delay_ms as f64) / speed).round() as i64;
    adjusted.max(1)
}

/// A/V synchronization clock supporting pause/resume, variable playback speed,
/// external audio clock locking, and presentation timeline delays.
#[derive(Debug, Clone)]
pub struct SyncClock {
    sync_point: TimePoint,
    paused_time_ms: i64,
    resumed_time_ms: i64,
    speed: f64,
    sync_by_audio: bool,
    looping_shift_ms: i64,
}

impl Default for SyncClock {
    fn default() -> Self {
        Self::new(0, 1.0, true)
    }
}

impl SyncClock {
    /// Creates a new paused/unstarted sync clock anchored at `initial_track_ms`.
    #[must_use]
    pub fn new(initial_track_ms: i64, speed: f64, sync_by_audio: bool) -> Self {
        Self {
            sync_point: TimePoint::new(initial_track_ms, TIME_UNKNOWN),
            paused_time_ms: TIME_UNKNOWN,
            resumed_time_ms: TIME_UNKNOWN,
            speed: speed.clamp(SPEED_MIN, SPEED_MAX),
            sync_by_audio,
            looping_shift_ms: 0,
        }
    }

    /// Starts or resumes playback at `world_now_ms` (`VideoTrackObject::resume`).
    pub fn resume(&mut self, world_now_ms: i64) {
        self.resumed_time_ms = world_now_ms;
        if self.paused_time_ms != TIME_UNKNOWN {
            let delta = (world_now_ms - self.paused_time_ms).max(0);
            if self.sync_point.world_time_ms != TIME_UNKNOWN {
                self.sync_point.world_time_ms += delta;
            } else {
                self.sync_point.world_time_ms = world_now_ms;
            }
            self.paused_time_ms = TIME_UNKNOWN;
        } else {
            self.sync_point.world_time_ms = world_now_ms;
        }
    }

    /// Pauses playback at `world_now_ms` (`VideoTrackObject::pause`).
    pub fn pause(&mut self, world_now_ms: i64) {
        if self.paused_time_ms == TIME_UNKNOWN {
            self.paused_time_ms = world_now_ms;
        }
    }

    /// Returns `true` when the clock is paused or not yet started.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused_time_ms != TIME_UNKNOWN || self.resumed_time_ms == TIME_UNKNOWN
    }

    /// Returns current playback speed multiplier.
    #[must_use]
    pub const fn speed(&self) -> f64 {
        self.speed
    }

    /// Changes playback speed without jumping the current track position
    /// (`VideoTrackObject::setSpeed`).
    pub fn set_speed(&mut self, speed: f64, world_now_ms: i64) {
        let clamped = speed.clamp(SPEED_MIN, SPEED_MAX);
        if self.sync_point.is_valid() {
            self.sync_point = self.track_time(world_now_ms);
        }
        self.speed = clamped;
    }

    /// Updates the anchor from an external audio mixer timestamp when
    /// `sync_by_audio` is enabled and `point.world_time_ms > resumed_time_ms`.
    pub fn sync_to_audio(&mut self, point: TimePoint) {
        if self.sync_by_audio
            && point.is_valid()
            && self.resumed_time_ms != TIME_UNKNOWN
            && point.world_time_ms > self.resumed_time_ms
        {
            self.sync_point = point;
        }
    }

    /// Shifts the presentation timeline forward by `delay_ms` (`addTimelineDelay`).
    pub fn add_timeline_delay(&mut self, delay_ms: i64) {
        if delay_ms != 0 && self.sync_point.is_valid() {
            self.sync_point.world_time_ms += delay_ms;
        }
    }

    /// Records a loop wrap-around by adding `duration_ms` to the cumulative shift.
    pub fn advance_loop(&mut self, duration_ms: i64) {
        self.looping_shift_ms = self.looping_shift_ms.saturating_add(duration_ms);
    }

    /// Returns the cumulative looping shift in milliseconds.
    #[must_use]
    pub const fn looping_shift_ms(&self) -> i64 {
        self.looping_shift_ms
    }

    /// Resets the clock to a seek target `track_time_ms`.
    pub fn seek(&mut self, track_time_ms: i64, world_now_ms: Option<i64>) {
        self.looping_shift_ms = 0;
        self.sync_point.track_time_ms = track_time_ms;
        if let Some(now) = world_now_ms {
            if self.is_paused() {
                self.sync_point.world_time_ms = now;
                if self.paused_time_ms != TIME_UNKNOWN {
                    self.paused_time_ms = now;
                }
            } else {
                self.sync_point.world_time_ms = now;
                self.resumed_time_ms = now;
            }
        }
    }

    /// Computes the current [`TimePoint`] at wall-clock `world_now_ms`
    /// (`VideoTrackObject::trackTime`).
    #[must_use]
    pub fn track_time(&self, world_now_ms: i64) -> TimePoint {
        let effective_world = if self.paused_time_ms != TIME_UNKNOWN {
            self.paused_time_ms
        } else {
            world_now_ms
        };
        if !self.sync_point.is_valid() {
            return TimePoint::new(self.sync_point.track_time_ms, effective_world);
        }
        let elapsed_world = effective_world - self.sync_point.world_time_ms;
        let scaled = ((elapsed_world as f64) * self.speed).round() as i64;
        TimePoint::new(self.sync_point.track_time_ms + scaled, effective_world)
    }

    /// Computes the wall-clock timestamp when a frame at `frame_pts_ms` should be shown.
    #[must_use]
    pub fn display_world_time(&self, frame_pts_ms: i64, world_now_ms: i64) -> i64 {
        let now = self.track_time(world_now_ms);
        let track_left = frame_pts_ms - now.track_time_ms;
        let adjusted = ((track_left as f64) / self.speed).round() as i64;
        now.world_time_ms + adjusted
    }
}

/// Outcome of polling [`FrameQueue::pop_ready`].
#[derive(Debug, Clone)]
pub enum FramePopOutcome {
    /// A frame is ready to be presented now (along with how many late frames were dropped).
    Ready {
        /// The selected frame to display, shared with
        /// [`crate::streaming::StreamingPlayer::current_video_frame`] instead of copied (a 1080p
        /// RGBA frame is 8 MB).
        frame: Arc<VideoFrame>,
        /// Number of older stale frames skipped to catch up to the clock.
        dropped: usize,
    },
    /// The next frame is scheduled for the future; wait `delay_ms` wall-clock ms.
    Wait {
        /// Wall-clock delay in milliseconds before the next frame is due.
        delay_ms: i64,
    },
    /// Queue is empty; decoder needs to produce more frames.
    Empty,
}

/// Bounded decoded video frame queue with tdesktop's late-frame dropping semantics
/// (`VideoTrack::IsStale` + `prepareState` + `waitForMarkAsShown`).
#[derive(Debug)]
pub struct FrameQueue {
    frames: VecDeque<VideoFrame>,
    capacity: usize,
    wait_for_mark_as_shown: bool,
    awaiting_shown: bool,
    dropped_total: u64,
}

impl FrameQueue {
    /// Creates a new frame queue with `capacity` (minimum 1) and `wait_for_mark_as_shown`.
    #[must_use]
    pub fn new(capacity: usize, wait_for_mark_as_shown: bool) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            wait_for_mark_as_shown,
            awaiting_shown: false,
            dropped_total: 0,
        }
    }

    /// Returns `true` when the queue has fewer frames than its capacity.
    #[must_use]
    pub fn needs_frame(&self) -> bool {
        self.frames.len() < self.capacity
    }

    /// Number of decoded frames currently buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Returns `true` if no frames are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The oldest queued frame.
    #[must_use]
    pub fn front(&self) -> Option<&VideoFrame> {
        self.frames.front()
    }

    /// Presentation time of the oldest queued frame.
    #[must_use]
    pub fn front_pts(&self) -> Option<i64> {
        self.frames.front().map(|f| f.pts_ms)
    }

    /// Total number of late frames dropped across the lifetime of this queue.
    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    /// Enables or disables waiting for [`FrameQueue::mark_frame_shown`] before advancing.
    pub fn set_wait_for_mark_as_shown(&mut self, wait: bool) {
        self.wait_for_mark_as_shown = wait;
        if !wait {
            self.awaiting_shown = false;
        }
    }

    /// Marks the last popped frame as displayed on screen (`markFrameShown`).
    pub fn mark_frame_shown(&mut self) -> bool {
        if self.awaiting_shown {
            self.awaiting_shown = false;
            true
        } else {
            false
        }
    }

    /// Clears all buffered frames (e.g. on seek or stop).
    pub fn clear(&mut self) {
        self.frames.clear();
        self.awaiting_shown = false;
    }

    /// Pushes a decoded frame, keeping monotonic PTS ordering if out-of-order frames occur.
    pub fn push(&mut self, frame: VideoFrame) {
        if let Some(back) = self.frames.back()
            && frame.pts_ms < back.pts_ms
        {
            let pos = self
                .frames
                .iter()
                .position(|f| f.pts_ms > frame.pts_ms)
                .unwrap_or(self.frames.len());
            self.frames.insert(pos, frame);
            return;
        }
        self.frames.push_back(frame);
    }

    /// Returns `true` if `frame` is strictly behind `track_time_ms` (`VideoTrack::IsStale`).
    #[must_use]
    pub const fn is_stale(frame: &VideoFrame, track_time_ms: i64) -> bool {
        frame.pts_ms < track_time_ms
    }

    /// Drops queued frames that a later queued frame already replaces at `track_time_ms`,
    /// keeping the most recent due frame (`VideoTrack::Shared::prepareState` with
    /// `dropStaleFrames`). Does nothing with `wait_for_mark_as_shown`. Returns how many frames
    /// were dropped.
    pub fn drop_stale(&mut self, track_time_ms: i64) -> usize {
        if self.wait_for_mark_as_shown {
            return 0;
        }
        let mut dropped = 0usize;
        while self.frames.len() >= 2
            && self
                .frames
                .get(1)
                .is_some_and(|next| next.pts_ms <= track_time_ms)
            && self
                .frames
                .front()
                .is_some_and(|f| Self::is_stale(f, track_time_ms))
        {
            self.frames.pop_front();
            dropped += 1;
        }
        self.dropped_total += dropped as u64;
        dropped
    }

    /// Selects the frame to display at `track_time_ms` and playback `speed`.
    ///
    /// When `!wait_for_mark_as_shown` and multiple frames are queued behind `track_time_ms`,
    /// older stale frames are dropped while always retaining the most recent due frame so
    /// playback smoothly catches up instead of falling behind.
    pub fn pop_ready(&mut self, track_time_ms: i64, speed: f64) -> FramePopOutcome {
        if self.wait_for_mark_as_shown && self.awaiting_shown {
            return FramePopOutcome::Wait { delay_ms: 1 };
        }
        let Some(front) = self.frames.front() else {
            return FramePopOutcome::Empty;
        };
        if front.pts_ms > track_time_ms {
            let delay_ms = world_time_delay(front.pts_ms - track_time_ms, speed).max(1);
            return FramePopOutcome::Wait { delay_ms };
        }

        let dropped = self.drop_stale(track_time_ms);
        let Some(frame) = self.frames.pop_front() else {
            return FramePopOutcome::Empty;
        };
        if self.wait_for_mark_as_shown {
            self.awaiting_shown = true;
        }
        FramePopOutcome::Ready {
            frame: Arc::new(frame),
            dropped,
        }
    }
}
