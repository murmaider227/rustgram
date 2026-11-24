//! Headless single-clip and multi-clip video segment playback clocks, boundary
//! transitions, and throttled timeline seeking.
//!
//! Port of `Telegram/SourceFiles/editor/video/video_segment_player.{h,cpp}` and
//! `Telegram/SourceFiles/editor/video/video_timeline_seeker.{h,cpp}`.

use crate::video::clip::{ClipId, ClipPosition, MultiClipTimeline};

/// Coalescing throttle window in milliseconds when dragging timeline handles or scrubber
/// (`kSeekThrottle` in `video_timeline_seeker.cpp`).
pub const SEEK_THROTTLE_MS: i64 = 120;

/// Playback configuration for a single-clip [`SegmentPlayer`] (`Editor::SegmentPlayerOptions`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentPlayerOptions {
    /// Preserve alpha channel when decoding frames.
    pub keep_alpha: bool,
    /// Audio-only playback mode (used by overlay audio tracks).
    pub audio: bool,
    /// Initial playback volume in `0.0..=1.0`.
    pub volume: f64,
}

impl Default for SegmentPlayerOptions {
    fn default() -> Self {
        Self {
            keep_alpha: false,
            audio: false,
            volume: 1.0,
        }
    }
}

/// Active decoding mode chosen on [`SegmentPlayer::restart`] (`Media::Streaming::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentPlaybackMode {
    /// Video frames only (muted or audio failed).
    Video,
    /// Audio stream only.
    Audio,
    /// Synchronized video + audio.
    Both,
}

/// Events emitted by [`SegmentPlayer`] for UI repaint / playback observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentPlaybackEvent {
    /// Player restarted at `position_ms` in `mode`.
    Restarted {
        /// Restart timestamp in milliseconds.
        position_ms: i64,
        /// Active stream mode.
        mode: SegmentPlaybackMode,
    },
    /// Playback position advanced to `position_ms`.
    PositionUpdated {
        /// Current timestamp in milliseconds.
        position_ms: i64,
    },
    /// Playback paused (via explicit pause or active seek hold).
    Paused,
    /// Playback resumed from held state.
    Resumed,
    /// Audio stream failed and player automatically retried in video-only mode.
    SoundFallback,
    /// Player stopped and released streaming resources.
    Stopped,
}

/// Headless single-clip looping segment player (`Editor::SegmentPlayer`).
#[derive(Debug, Clone)]
pub struct SegmentPlayer {
    options: SegmentPlayerOptions,
    started: bool,
    ready: bool,
    from_ms: i64,
    till_ms: i64,
    position_ms: i64,
    volume: f64,
    sound: bool,
    sound_failed: bool,
    paused_others: bool,
    paused: bool,
    seeking: bool,
    events: Vec<SegmentPlaybackEvent>,
}

impl SegmentPlayer {
    /// Creates a stopped [`SegmentPlayer`] with `options`.
    #[must_use]
    pub fn new(options: SegmentPlayerOptions) -> Self {
        let volume = if options.volume.is_finite() {
            options.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        Self {
            options,
            started: false,
            ready: false,
            from_ms: 0,
            till_ms: 0,
            position_ms: 0,
            volume,
            sound: false,
            sound_failed: false,
            paused_others: false,
            paused: false,
            seeking: false,
            events: Vec::new(),
        }
    }

    /// Starts the player and seeks to `from_ms` (`SegmentPlayer::start`).
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.ready = true;
        self.restart(self.from_ms);
    }

    /// Stops playback and clears ready state (`SegmentPlayer::stop`).
    pub fn stop(&mut self) {
        if !self.started {
            return;
        }
        self.started = false;
        self.ready = false;
        self.events.push(SegmentPlaybackEvent::Stopped);
    }

    /// Returns `true` when the player instance is active (`SegmentPlayer::valid`).
    #[must_use]
    pub const fn valid(&self) -> bool {
        self.started
    }

    /// Returns `true` when playback is ready to produce frames (`SegmentPlayer::ready`).
    #[must_use]
    pub const fn ready(&self) -> bool {
        self.started && self.ready
    }

    /// Segment start timestamp in milliseconds (`SegmentPlayer::from`).
    #[must_use]
    pub const fn from_ms(&self) -> i64 {
        self.from_ms
    }

    /// Segment end timestamp in milliseconds (`0` = end of media, `SegmentPlayer::till`).
    #[must_use]
    pub const fn till_ms(&self) -> i64 {
        self.till_ms
    }

    /// Current playback timestamp in milliseconds (`SegmentPlayer::position`).
    #[must_use]
    pub const fn position_ms(&self) -> i64 {
        self.position_ms
    }

    /// Returns `true` while paused or actively scrubbing (`SegmentPlayer::held`).
    #[must_use]
    pub const fn held(&self) -> bool {
        self.paused || self.seeking
    }

    /// Configured playback volume in `0.0..=1.0`.
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.volume
    }

    /// Returns `true` when sound playback is requested and has not failed.
    #[must_use]
    pub const fn sound_active(&self) -> bool {
        self.options.audio || (self.sound && !self.sound_failed)
    }

    /// Returns `true` if other media playback was paused when starting audible playback.
    #[must_use]
    pub const fn paused_other_playback(&self) -> bool {
        self.paused_others
    }

    /// Active playback mode computed from options and sound state.
    #[must_use]
    pub const fn mode(&self) -> SegmentPlaybackMode {
        if self.options.audio {
            SegmentPlaybackMode::Audio
        } else if self.sound && !self.sound_failed {
            SegmentPlaybackMode::Both
        } else {
            SegmentPlaybackMode::Video
        }
    }

    const fn segment_till(&self) -> i64 {
        if self.till_ms > self.from_ms {
            self.till_ms
        } else {
            i64::MAX
        }
    }

    /// Updates the looping segment `[from_ms, till_ms]` (`SegmentPlayer::setSegment`).
    pub fn set_segment(&mut self, from_ms: i64, till_ms: i64) {
        self.from_ms = from_ms.max(0);
        self.till_ms = if till_ms > self.from_ms { till_ms } else { 0 };
        let outside = self.position_ms < self.from_ms || self.position_ms >= self.segment_till();
        if outside && !self.held() {
            self.restart(self.from_ms);
        }
    }

    /// Restarts playback at `position_ms` clamped into `[from_ms, segment_till]`
    /// (`SegmentPlayer::restart`).
    pub fn restart(&mut self, position_ms: i64) {
        if !self.started {
            return;
        }
        self.position_ms = position_ms.clamp(self.from_ms, self.segment_till());
        let mode = self.mode();
        if mode != SegmentPlaybackMode::Video {
            self.paused_others = true;
        }
        self.events.push(SegmentPlaybackEvent::Restarted {
            position_ms: self.position_ms,
            mode,
        });
        if self.held() {
            self.events.push(SegmentPlaybackEvent::Paused);
        }
        self.events.push(SegmentPlaybackEvent::PositionUpdated {
            position_ms: self.position_ms,
        });
    }

    /// Sets explicit pause state (`SegmentPlayer::setPaused`).
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        let was_held = self.held();
        self.paused = paused;
        self.apply_held(was_held);
    }

    /// Sets active scrubbing hold state (`SegmentPlayer::setSeeking`).
    pub fn set_seeking(&mut self, seeking: bool) {
        if self.seeking == seeking {
            return;
        }
        let was_held = self.held();
        self.seeking = seeking;
        self.apply_held(was_held);
    }

    fn apply_held(&mut self, was_held: bool) {
        if !self.started {
            return;
        }
        let now_held = self.held();
        if now_held == was_held {
            return;
        }
        if now_held {
            self.events.push(SegmentPlaybackEvent::Paused);
        } else {
            self.events.push(SegmentPlaybackEvent::Resumed);
        }
    }

    /// Sets volume multiplier in `0.0..=1.0` (`SegmentPlayer::setVolume`).
    pub fn set_volume(&mut self, volume: f64) {
        let clamped = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume = clamped;
    }

    /// Enables or disables audio output and restarts at current position (`SegmentPlayer::setSound`).
    pub fn set_sound(&mut self, sound: bool) {
        if self.options.audio || self.sound == sound {
            return;
        }
        self.sound = sound;
        if !sound {
            self.paused_others = false;
        }
        self.restart(self.position_ms);
    }

    /// Processes a decoded frame position update (`SegmentPlayer::handlePosition`).
    ///
    /// Automatically loops back to `from_ms` when `position_ms >= segment_till()`.
    pub fn handle_position(&mut self, position_ms: i64) {
        if !self.started {
            return;
        }
        self.position_ms = position_ms;
        if self.held() {
            self.events.push(SegmentPlaybackEvent::Paused);
            return;
        }
        if self.position_ms >= self.segment_till() {
            self.restart(self.from_ms);
            return;
        }
        self.events
            .push(SegmentPlaybackEvent::PositionUpdated { position_ms });
    }

    /// Handles end-of-stream notification by looping back to `from_ms` (`Finished`).
    pub fn handle_finished(&mut self) {
        if self.started {
            self.restart(self.from_ms);
        }
    }

    /// Handles a streaming error with automatic silent video-only fallback retry
    /// (`SegmentPlayer::handleError`).
    pub fn handle_error(&mut self) {
        self.started = false;
        self.ready = false;
        if self.sound && !self.sound_failed && !self.options.audio {
            self.sound_failed = true;
            self.events.push(SegmentPlaybackEvent::SoundFallback);
            self.start();
        } else {
            self.events.push(SegmentPlaybackEvent::Stopped);
        }
    }

    /// Drains accumulated playback events in emission order.
    pub fn drain_events(&mut self) -> Vec<SegmentPlaybackEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Throttled seek coordinator between a trim timeline and a [`SegmentPlayer`]
/// (`Editor::TimelineSeeker` in `video_timeline_seeker.{h,cpp}`).
#[derive(Debug, Clone, Default)]
pub struct TimelineSeeker {
    pending_ms: Option<i64>,
    timer_due_ms: Option<i64>,
    dragging: bool,
}

impl TimelineSeeker {
    /// Creates a new idle timeline seeker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending_ms: None,
            timer_due_ms: None,
            dragging: false,
        }
    }

    /// Returns `true` if a scrub drag is currently in progress.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Returns the coalesced pending seek target, if any.
    #[must_use]
    pub const fn pending_ms(&self) -> Option<i64> {
        self.pending_ms
    }

    /// Schedules a throttled seek at `position_ms` given monotonic clock `now_ms`
    /// (`TimelineSeeker::seek`).
    pub fn request_seek(&mut self, position_ms: i64, now_ms: i64) {
        self.pending_ms = Some(position_ms.max(0));
        if self.timer_due_ms.is_none() {
            self.timer_due_ms = Some(now_ms.saturating_add(SEEK_THROTTLE_MS));
        }
    }

    /// Advances the throttle clock to `now_ms` and dispatches any due seek to `player`.
    ///
    /// Returns the dispatched position if a seek fired.
    pub fn tick(&mut self, now_ms: i64, player: &mut SegmentPlayer) -> Option<i64> {
        let due = self.timer_due_ms?;
        if now_ms < due {
            return None;
        }
        self.timer_due_ms = None;
        let pos = self.pending_ms.take()?;
        player.restart(pos);
        Some(pos)
    }

    /// Begins a drag gesture, holding `player` in seeking state (`dragging = true`).
    pub fn begin_dragging(&mut self, player: &mut SegmentPlayer) {
        self.dragging = true;
        player.set_seeking(true);
    }

    /// Finishes a drag gesture, cancelling pending throttled seeks and resuming playback
    /// inside `[from_ms, till_ms]` (`TimelineSeeker::finishDragging`).
    ///
    /// If the resolved position lands at or past `till_ms`, resumes from `from_ms`.
    pub fn finish_dragging(
        &mut self,
        from_ms: i64,
        till_ms: i64,
        player: &mut SegmentPlayer,
    ) -> i64 {
        self.dragging = false;
        self.timer_due_ms = None;
        let latest = self
            .pending_ms
            .take()
            .unwrap_or_else(|| player.position_ms());
        let lo = from_ms.max(0);
        let hi = till_ms.max(lo);
        let resume = latest.clamp(lo, hi);
        let target = if resume >= hi && hi > lo { lo } else { resume };
        player.set_seeking(false);
        player.restart(target);
        target
    }
}

/// Playback events emitted by [`MultiClipSegmentPlayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiClipPlayerEvent {
    /// Active playback crossed from one clip into another.
    ClipTransition {
        /// Previous clip index, if any.
        from_clip: Option<usize>,
        /// Newly active clip index.
        to_clip: usize,
        /// Newly active clip ID.
        to_clip_id: ClipId,
        /// Global timeline timestamp where the transition occurred (ms).
        global_ms: i64,
        /// Local timestamp inside the newly active clip (ms).
        local_ms: i64,
    },
    /// Explicit seek or scrub repositioned playback.
    Seeked {
        /// Clamped global timestamp (ms).
        global_ms: i64,
        /// Resolved clip index.
        clip_index: usize,
        /// Resolved local timestamp inside the clip (ms).
        local_ms: i64,
    },
    /// Continuous playback reached the end of the timeline and wrapped to `0ms`.
    Looped,
    /// Non-looping playback reached `total_duration_ms` and paused.
    Ended,
}

/// Headless multi-clip playback clock, boundary-transition state machine, and
/// global-to-local timeline seeker.
#[derive(Debug, Clone)]
pub struct MultiClipSegmentPlayer {
    timeline: MultiClipTimeline,
    global_position_ms: i64,
    active_clip: Option<ClipPosition>,
    paused: bool,
    seeking: bool,
    looping: bool,
    playback_rate: f64,
    scrub_pending_ms: Option<i64>,
    scrub_due_ms: Option<i64>,
    events: Vec<MultiClipPlayerEvent>,
}

impl MultiClipSegmentPlayer {
    /// Creates a new headless multi-clip player positioned at `0ms` (initially paused).
    #[must_use]
    pub fn new(timeline: MultiClipTimeline) -> Self {
        let active_clip = timeline.resolve_global_ms(0);
        Self {
            timeline,
            global_position_ms: 0,
            active_clip,
            paused: true,
            seeking: false,
            looping: true,
            playback_rate: 1.0,
            scrub_pending_ms: None,
            scrub_due_ms: None,
            events: Vec::new(),
        }
    }

    /// Borrows the underlying [`MultiClipTimeline`].
    #[must_use]
    pub const fn timeline(&self) -> &MultiClipTimeline {
        &self.timeline
    }

    /// Replaces the timeline and re-clamps the playback clock into the new duration.
    pub fn replace_timeline(&mut self, timeline: MultiClipTimeline) {
        self.timeline = timeline;
        self.sync_position_after_edit();
    }

    /// Mutates the underlying [`MultiClipTimeline`] and immediately reconciles the
    /// playback position and active clip mapping.
    pub fn mutate_timeline<R>(&mut self, edit: impl FnOnce(&mut MultiClipTimeline) -> R) -> R {
        let result = edit(&mut self.timeline);
        self.sync_position_after_edit();
        result
    }

    fn sync_position_after_edit(&mut self) {
        let prev_clip = self.active_clip.map(|c| c.clip_index);
        let total = self.timeline.total_duration_ms();
        self.global_position_ms = self.global_position_ms.clamp(0, total);
        self.active_clip = self.timeline.resolve_global_ms(self.global_position_ms);
        if let Some(pos) = self.active_clip
            && prev_clip != Some(pos.clip_index)
        {
            self.events.push(MultiClipPlayerEvent::ClipTransition {
                from_clip: prev_clip,
                to_clip: pos.clip_index,
                to_clip_id: pos.clip_id,
                global_ms: pos.global_ms,
                local_ms: pos.local_ms,
            });
        }
    }

    /// Total duration across all clips in milliseconds.
    #[must_use]
    pub fn total_duration_ms(&self) -> i64 {
        self.timeline.total_duration_ms()
    }

    /// Current global playhead timestamp in milliseconds.
    #[must_use]
    pub const fn global_position_ms(&self) -> i64 {
        self.global_position_ms
    }

    /// Current resolved `(clip_index, clip_id, local_ms, global_ms)`, or `None` if empty.
    #[must_use]
    pub const fn current_position(&self) -> Option<ClipPosition> {
        self.active_clip
    }

    /// Resolves any global timeline timestamp into `(clip_index, local_ms)`, clamping
    /// out-of-range values into `[0, total_duration_ms()]`.
    #[must_use]
    pub fn resolve_global_ms(&self, global_ms: i64) -> Option<ClipPosition> {
        self.timeline.resolve_global_ms(global_ms)
    }

    /// Maps `(clip_index, local_ms)` back to a global timeline timestamp.
    #[must_use]
    pub fn global_ms_for(&self, clip_index: usize, local_ms: i64) -> Option<i64> {
        self.timeline.global_ms_for(clip_index, local_ms)
    }

    /// Returns `true` when playback is actively advancing (not paused, not scrubbing, non-empty).
    #[must_use]
    pub fn is_playing(&self) -> bool {
        !self.paused && !self.seeking && self.total_duration_ms() > 0
    }

    /// Returns `true` when paused or held by scrubbing.
    #[must_use]
    pub const fn is_held(&self) -> bool {
        self.paused || self.seeking
    }

    /// Returns `true` when looping is enabled.
    #[must_use]
    pub const fn is_looping(&self) -> bool {
        self.looping
    }

    /// Enables or disables loop-at-end playback.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Sets playback speed multiplier (clamped to `0.1..=4.0`).
    pub fn set_playback_rate(&mut self, rate: f64) {
        if rate.is_finite() {
            self.playback_rate = rate.clamp(0.1, 4.0);
        }
    }

    /// Active playback speed multiplier.
    #[must_use]
    pub const fn playback_rate(&self) -> f64 {
        self.playback_rate
    }

    /// Starts or resumes playback.
    pub fn play(&mut self) {
        if self.total_duration_ms() == 0 {
            return;
        }
        if !self.looping && self.global_position_ms >= self.total_duration_ms() {
            let _ = self.seek_global(0);
        }
        self.paused = false;
    }

    /// Pauses playback.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Sets explicit pause state.
    pub fn set_paused(&mut self, paused: bool) {
        if paused {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Effective audio gain (`0.0..=1.0`) of the currently active clip.
    #[must_use]
    pub fn active_audio_gain(&self) -> f64 {
        let Some(pos) = self.active_clip else {
            return 0.0;
        };
        self.timeline
            .get(pos.clip_index)
            .map_or(0.0, |clip| clip.effective_volume())
    }

    /// Immediately seeks to `global_ms` (clamped into `[0, total_duration_ms()]`),
    /// updating active clip state and emitting transition/seek events.
    pub fn seek_global(&mut self, global_ms: i64) -> Option<ClipPosition> {
        let pos = self.timeline.resolve_global_ms(global_ms)?;
        let prev_clip = self.active_clip.map(|c| c.clip_index);
        self.global_position_ms = pos.global_ms;
        self.active_clip = Some(pos);
        if prev_clip != Some(pos.clip_index) {
            self.events.push(MultiClipPlayerEvent::ClipTransition {
                from_clip: prev_clip,
                to_clip: pos.clip_index,
                to_clip_id: pos.clip_id,
                global_ms: pos.global_ms,
                local_ms: pos.local_ms,
            });
        }
        self.events.push(MultiClipPlayerEvent::Seeked {
            global_ms: pos.global_ms,
            clip_index: pos.clip_index,
            local_ms: pos.local_ms,
        });
        Some(pos)
    }

    /// Begins a throttled scrub gesture (pauses clock advancement until [`Self::end_scrub`]).
    pub fn begin_scrub(&mut self) {
        self.seeking = true;
    }

    /// Requests a throttled scrub to `global_ms` at monotonic clock `clock_ms`.
    ///
    /// Dispatches immediately if no throttle window is pending; otherwise coalesces until
    /// [`Self::flush_scrub`] or [`Self::end_scrub`].
    pub fn scrub_to(&mut self, global_ms: i64, clock_ms: i64) -> Option<ClipPosition> {
        self.seeking = true;
        match self.scrub_due_ms {
            None => {
                self.scrub_due_ms = Some(clock_ms.saturating_add(SEEK_THROTTLE_MS));
                self.scrub_pending_ms = None;
                self.seek_global(global_ms)
            }
            Some(due) if clock_ms >= due => {
                self.scrub_due_ms = Some(clock_ms.saturating_add(SEEK_THROTTLE_MS));
                self.scrub_pending_ms = None;
                self.seek_global(global_ms)
            }
            Some(_) => {
                self.scrub_pending_ms = Some(global_ms);
                None
            }
        }
    }

    /// Dispatches any coalesced scrub target whose `SEEK_THROTTLE_MS` deadline has elapsed.
    pub fn flush_scrub(&mut self, clock_ms: i64) -> Option<ClipPosition> {
        let due = self.scrub_due_ms?;
        if clock_ms < due {
            return None;
        }
        self.scrub_due_ms = None;
        let pending = self.scrub_pending_ms.take()?;
        self.seek_global(pending)
    }

    /// Ends a scrub gesture, flushing any pending target and wrapping `>= total_duration_ms`
    /// back to `0ms` when looping.
    pub fn end_scrub(&mut self) -> Option<ClipPosition> {
        self.seeking = false;
        self.scrub_due_ms = None;
        let total = self.total_duration_ms();
        let target = self
            .scrub_pending_ms
            .take()
            .unwrap_or(self.global_position_ms);
        let clamped = target.clamp(0, total);
        let final_target = if self.looping && total > 0 && clamped >= total {
            0
        } else {
            clamped
        };
        self.seek_global(final_target)
    }

    /// Advances the headless playback clock by `delta_ms` milliseconds, stepping across every
    /// clip boundary in order and emitting [`MultiClipPlayerEvent`]s.
    pub fn advance(&mut self, delta_ms: i64) -> Option<ClipPosition> {
        let total = self.total_duration_ms();
        if total <= 0 {
            self.active_clip = self.timeline.resolve_global_ms(0);
            return self.active_clip;
        }
        if self.is_held() || delta_ms <= 0 {
            return self.active_clip;
        }

        let scaled = ((delta_ms as f64) * self.playback_rate).round() as i64;
        if scaled <= 0 {
            return self.active_clip;
        }

        let spans = self.timeline.spans();
        let mut remaining = scaled;
        let mut cursor = self.global_position_ms.clamp(0, total);
        let mut current_clip = self.active_clip.map(|c| c.clip_index).or_else(|| {
            self.timeline
                .resolve_global_ms(cursor)
                .map(|p| p.clip_index)
        });

        while remaining > 0 {
            // Find the end of the current non-empty clip span containing `cursor`.
            let next_boundary = spans
                .iter()
                .find(|s| {
                    s.duration_ms() > 0 && cursor >= s.global_start_ms && cursor < s.global_end_ms
                })
                .map(|s| s.global_end_ms)
                .unwrap_or(total);

            let dist_to_boundary = (next_boundary - cursor).max(0);
            if dist_to_boundary == 0 || remaining < dist_to_boundary {
                cursor = (cursor + remaining).min(total);
                break;
            }

            // Cross this clip boundary.
            cursor = next_boundary;
            remaining -= dist_to_boundary;

            if cursor >= total {
                if self.looping {
                    self.events.push(MultiClipPlayerEvent::Looped);
                    cursor = 0;
                    if let Some(pos) = self.timeline.resolve_global_ms(0) {
                        if current_clip != Some(pos.clip_index) {
                            self.events.push(MultiClipPlayerEvent::ClipTransition {
                                from_clip: current_clip,
                                to_clip: pos.clip_index,
                                to_clip_id: pos.clip_id,
                                global_ms: pos.global_ms,
                                local_ms: pos.local_ms,
                            });
                        }
                        current_clip = Some(pos.clip_index);
                    }
                    // Guard against enormous delta causing excessive loop iterations.
                    if remaining >= total {
                        remaining %= total;
                    }
                } else {
                    self.paused = true;
                    self.events.push(MultiClipPlayerEvent::Ended);
                    break;
                }
            } else if let Some(pos) = self.timeline.resolve_global_ms(cursor) {
                if current_clip != Some(pos.clip_index) {
                    self.events.push(MultiClipPlayerEvent::ClipTransition {
                        from_clip: current_clip,
                        to_clip: pos.clip_index,
                        to_clip_id: pos.clip_id,
                        global_ms: pos.global_ms,
                        local_ms: pos.local_ms,
                    });
                }
                current_clip = Some(pos.clip_index);
            }
        }

        self.global_position_ms = cursor;
        self.active_clip = self.timeline.resolve_global_ms(cursor);
        self.active_clip
    }

    /// Drains accumulated playback events in emission order.
    pub fn drain_events(&mut self) -> Vec<MultiClipPlayerEvent> {
        std::mem::take(&mut self.events)
    }
}
