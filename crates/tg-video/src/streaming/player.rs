//! High-level streaming video/audio player with A/V sync, buffering, and seeking.
//!
//! Port of `Telegram/SourceFiles/media/streaming/media_streaming_player.cpp`.

use std::collections::VecDeque;

use crate::error::{Result, StreamingError, VideoError};
use crate::streaming::clock::{
    DEFAULT_FRAME_QUEUE_CAPACITY, FramePopOutcome, FrameQueue, SyncClock, TimePoint,
};
use crate::streaming::decoder::{DecodedPacket, Decoder};
use crate::types::{
    AudioFrame, DURATION_UNAVAILABLE, PlaybackOptions, SPEED_MAX, SPEED_MIN, StreamInformation,
    StreamingUpdate, TIME_UNKNOWN, VideoFrame,
};

/// Minimum buffered media duration required to resume from waiting-for-data (`kBufferFor` = 3 s).
pub const BUFFER_FOR_MS: i64 = 3_000;

/// Read-ahead target for remote streaming loaders (`kLoadInAdvanceForRemote` = 32 s).
pub const LOAD_IN_ADVANCE_REMOTE_MS: i64 = 32_000;

/// Read-ahead target for local file loaders (`kLoadInAdvanceForLocal` = 5 s).
pub const LOAD_IN_ADVANCE_LOCAL_MS: i64 = 5_000;

/// How far the clock may run past the end of the last shown frame while the decoder waits for
/// the network, before playback pauses for data (`Player::_pausedByWaitingForData`).
pub const STARVE_GRACE_MS: i64 = 40;

/// Lifecycle stage of [`StreamingPlayer`] (mirrors `Player::Stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerStage {
    /// Player has not been started or was stopped.
    Uninitialized,
    /// Stream metadata/first frame ready, awaiting start/resume.
    Ready,
    /// Active playback (may be paused by user or buffering).
    Started,
    /// Playback reached end of stream.
    Finished,
    /// Playback stopped due to an error.
    Failed,
}

/// High-level streaming player generic over any [`Decoder`].
pub struct StreamingPlayer<D: Decoder> {
    decoder: D,
    options: PlaybackOptions,
    stage: PlayerStage,
    clock: SyncClock,
    video_queue: FrameQueue,
    audio_queue: VecDeque<AudioFrame>,
    current_video_frame: Option<VideoFrame>,
    paused_by_user: bool,
    paused_by_waiting: bool,
    eof_reached: bool,
    last_error: Option<StreamingError>,
    updates: VecDeque<StreamingUpdate>,
    lock_count: usize,
    video_position_ms: i64,
    video_received_till_ms: i64,
    audio_position_ms: i64,
    audio_received_till_ms: i64,
    /// The last [`Decoder::next_frame`] call returned [`DecodedPacket::Pending`].
    decoder_pending: bool,
    /// `paused_by_waiting` was set because the decoder ran dry (see [`STARVE_GRACE_MS`]).
    waiting_starved: bool,
    last_video_duration_ms: i64,
}

impl<D: Decoder> StreamingPlayer<D> {
    /// Creates a new player wrapping `decoder` with `options`.
    pub fn new(decoder: D, options: PlaybackOptions) -> Result<Self> {
        if !(SPEED_MIN..=SPEED_MAX).contains(&options.speed) {
            return Err(VideoError::InvalidArgument(format!(
                "speed {} outside [{SPEED_MIN}, {SPEED_MAX}]",
                options.speed
            )));
        }
        if !(0.0..=1.0).contains(&options.volume) {
            return Err(VideoError::InvalidArgument(format!(
                "volume {} outside [0.0, 1.0]",
                options.volume
            )));
        }

        let info = decoder.info().clone();
        let initial_pos = info
            .video
            .as_ref()
            .map(|v| v.state.position_ms)
            .or_else(|| info.audio.as_ref().map(|a| a.state.position_ms))
            .unwrap_or(options.position_ms);
        let initial_video_recv = info
            .video
            .as_ref()
            .map_or(TIME_UNKNOWN, |v| v.state.received_till_ms);
        let initial_audio_recv = info
            .audio
            .as_ref()
            .map_or(TIME_UNKNOWN, |a| a.state.received_till_ms);

        let mut updates = VecDeque::new();
        updates.push_back(StreamingUpdate::Information(Box::new(info)));

        let clock = SyncClock::new(
            initial_pos.max(0),
            options.speed,
            options.sync_video_by_audio,
        );
        let video_queue =
            FrameQueue::new(DEFAULT_FRAME_QUEUE_CAPACITY, options.wait_for_mark_as_shown);

        Ok(Self {
            decoder,
            options,
            stage: PlayerStage::Ready,
            clock,
            video_queue,
            audio_queue: VecDeque::new(),
            current_video_frame: None,
            paused_by_user: false,
            paused_by_waiting: false,
            eof_reached: false,
            last_error: None,
            updates,
            lock_count: 0,
            video_position_ms: initial_pos.max(0),
            video_received_till_ms: initial_video_recv,
            audio_position_ms: initial_pos.max(0),
            audio_received_till_ms: initial_audio_recv,
            decoder_pending: false,
            waiting_starved: false,
            last_video_duration_ms: 0,
        })
    }

    /// Returns stream metadata.
    #[must_use]
    pub fn info(&self) -> &StreamInformation {
        self.decoder.info()
    }

    /// Returns a shared reference to the underlying decoder.
    #[must_use]
    pub const fn decoder(&self) -> &D {
        &self.decoder
    }

    /// Returns a mutable reference to the underlying decoder.
    pub fn decoder_mut(&mut self) -> &mut D {
        &mut self.decoder
    }

    /// Returns current lifecycle stage.
    #[must_use]
    pub const fn stage(&self) -> PlayerStage {
        self.stage
    }

    /// Returns `true` when playback is active (`Ready` or `Started`).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self.stage, PlayerStage::Ready | PlayerStage::Started)
    }

    /// Returns `true` when stream metadata is initialized and frames can be read.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        !matches!(self.stage, PlayerStage::Uninitialized | PlayerStage::Failed)
    }

    /// Returns `true` when paused by the user or buffering.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused_by_user || self.paused_by_waiting || self.clock.is_paused()
    }

    /// Returns the last fatal streaming error, if any.
    #[must_use]
    pub const fn failed(&self) -> Option<StreamingError> {
        self.last_error
    }

    /// Returns total file size in bytes.
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.decoder.file_size()
    }

    /// Returns the read-ahead target duration in milliseconds.
    #[must_use]
    pub fn load_in_advance_ms(&self) -> i64 {
        if self.decoder.is_remote() {
            LOAD_IN_ADVANCE_REMOTE_MS
        } else {
            LOAD_IN_ADVANCE_LOCAL_MS
        }
    }

    /// Starts or resumes playback at `world_now_ms`.
    pub fn start(&mut self, world_now_ms: i64) {
        if self.stage == PlayerStage::Ready {
            self.stage = PlayerStage::Started;
        }
        self.paused_by_user = false;
        if !self.paused_by_waiting {
            self.clock.resume(world_now_ms);
        }
    }

    /// Pauses playback at `world_now_ms`.
    pub fn pause(&mut self, world_now_ms: i64) {
        self.paused_by_user = true;
        self.clock.pause(world_now_ms);
    }

    /// Resumes playback at `world_now_ms`.
    pub fn resume(&mut self, world_now_ms: i64) {
        self.start(world_now_ms);
    }

    /// Stops playback and clears buffered queues.
    pub fn stop(&mut self) {
        self.stage = PlayerStage::Uninitialized;
        self.video_queue.clear();
        self.audio_queue.clear();
        self.current_video_frame = None;
        self.eof_reached = false;
    }

    /// Seeks to `position_ms` and resets frame queues and A/V clock.
    pub fn seek(&mut self, position_ms: i64, world_now_ms: Option<i64>) -> Result<()> {
        self.decoder.seek(position_ms)?;
        self.video_queue.clear();
        self.audio_queue.clear();
        self.eof_reached = false;
        self.last_error = None;
        if self.stage == PlayerStage::Finished {
            self.stage = PlayerStage::Started;
        }
        let target = position_ms.max(0);
        self.video_position_ms = target;
        self.audio_position_ms = target;
        self.last_video_duration_ms = 0;
        self.clock.seek(target, world_now_ms);
        // Like tdesktop after a seek, hold the clock until the first frames are decoded, so a
        // background decoder's first frames are not dropped as stale.
        if let Some(now) = world_now_ms
            && self.stage == PlayerStage::Started
            && self.decoder.info().video.is_some()
        {
            self.waiting_starved = true;
            self.set_waiting_for_data(true, now);
        }
        Ok(())
    }

    /// Returns current playback speed multiplier.
    #[must_use]
    pub const fn speed(&self) -> f64 {
        self.options.speed
    }

    /// Sets playback speed (`SPEED_MIN..=SPEED_MAX`) at `world_now_ms`.
    pub fn set_speed(&mut self, speed: f64, world_now_ms: i64) -> Result<()> {
        if !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
            return Err(VideoError::InvalidArgument(format!(
                "speed {speed} outside [{SPEED_MIN}, {SPEED_MAX}]"
            )));
        }
        self.options.speed = speed;
        self.clock.set_speed(speed, world_now_ms);
        Ok(())
    }

    /// Returns current audio volume (`0.0..=1.0`).
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.options.volume
    }

    /// Sets audio volume (`0.0..=1.0`).
    pub fn set_volume(&mut self, volume: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(VideoError::InvalidArgument(format!(
                "volume {volume} outside [0.0, 1.0]"
            )));
        }
        self.options.volume = volume;
        Ok(())
    }

    /// Enables or disables seamless looping.
    pub fn set_loop(&mut self, loop_playback: bool) {
        self.options.loop_playback = loop_playback;
        self.decoder.set_loop(loop_playback);
    }

    /// Updates the underlying loader's scheduling priority.
    pub fn set_loader_priority(&mut self, priority: i32) {
        self.decoder.set_loader_priority(priority);
    }

    /// Feeds an external audio mixer synchronization point into the video clock.
    pub fn sync_to_audio(&mut self, point: TimePoint) {
        self.clock.sync_to_audio(point);
    }

    /// Marks the currently displayed frame as shown (required when `wait_for_mark_as_shown` is set).
    pub fn mark_frame_shown(&mut self) -> bool {
        self.video_queue.mark_frame_shown()
    }

    /// Increments the player lock count (mirrors `Player::lock`).
    pub fn lock(&mut self) {
        self.lock_count = self.lock_count.saturating_add(1);
    }

    /// Decrements the player lock count (mirrors `Player::unlock`).
    pub fn unlock(&mut self) {
        self.lock_count = self.lock_count.saturating_sub(1);
    }

    /// Returns `true` when at least one instance holds a lock on the player.
    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.lock_count > 0
    }

    /// Returns total late video frames dropped by the frame queue.
    #[must_use]
    pub const fn dropped_frames(&self) -> u64 {
        self.video_queue.dropped_total()
    }

    /// Returns the most recently displayed video frame.
    #[must_use]
    pub const fn current_video_frame(&self) -> Option<&VideoFrame> {
        self.current_video_frame.as_ref()
    }

    /// Returns the current playback position on the media timeline at `world_now_ms`.
    #[must_use]
    pub fn position_ms(&self, world_now_ms: i64) -> i64 {
        let raw = self.clock.track_time(world_now_ms).track_time_ms;
        if self.options.loop_playback {
            let dur = self.total_duration_ms();
            if dur > 0 && dur != DURATION_UNAVAILABLE {
                return raw.rem_euclid(dur);
            }
        }
        raw
    }

    /// Returns total duration across video/audio tracks (`DURATION_UNAVAILABLE` if unknown).
    #[must_use]
    pub fn total_duration_ms(&self) -> i64 {
        let v = self
            .decoder
            .info()
            .video
            .as_ref()
            .map_or(TIME_UNKNOWN, |x| x.state.duration_ms);
        let a = self
            .decoder
            .info()
            .audio
            .as_ref()
            .map_or(TIME_UNKNOWN, |x| x.state.duration_ms);
        let best = v.max(a);
        if best == TIME_UNKNOWN {
            DURATION_UNAVAILABLE
        } else {
            best
        }
    }

    /// Drains pending [`StreamingUpdate`] notifications.
    pub fn drain_updates(&mut self) -> Vec<StreamingUpdate> {
        self.updates.drain(..).collect()
    }

    /// Pops the next buffered audio frame, if any.
    pub fn pop_audio_frame(&mut self) -> Option<AudioFrame> {
        let frame = self.audio_queue.pop_front()?;
        self.audio_position_ms = frame.pts_ms;
        self.updates
            .push_back(StreamingUpdate::UpdateAudio(frame.pts_ms));
        Some(frame)
    }

    /// Decodes frames from the underlying [`Decoder`] until the video frame queue is full
    /// or EOF is reached.
    pub fn fill_buffers(&mut self) -> Result<()> {
        self.decoder_pending = false;
        while !self.eof_reached && self.video_queue.needs_frame() {
            match self.decoder.next_frame() {
                Ok(DecodedPacket::Video(frame)) => {
                    if frame.pts_ms > self.video_received_till_ms {
                        self.video_received_till_ms = frame.pts_ms;
                        self.updates
                            .push_back(StreamingUpdate::PreloadedVideo(frame.pts_ms));
                    }
                    self.video_queue.push(frame);
                }
                Ok(DecodedPacket::Audio(frame)) => {
                    if frame.pts_ms > self.audio_received_till_ms {
                        self.audio_received_till_ms = frame.pts_ms;
                        self.updates
                            .push_back(StreamingUpdate::PreloadedAudio(frame.pts_ms));
                    }
                    self.audio_queue.push_back(frame);
                }
                Ok(DecodedPacket::EndOfStream) => {
                    self.eof_reached = true;
                    break;
                }
                Ok(DecodedPacket::Pending) => {
                    self.decoder_pending = true;
                    break;
                }
                Err(err) => {
                    let se = match &err {
                        VideoError::Streaming(s) => *s,
                        _ => StreamingError::InvalidData,
                    };
                    self.last_error = Some(se);
                    self.stage = PlayerStage::Failed;
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// Advances playback to `world_now_ms`, refilling buffers, dropping late frames if
    /// needed, and returning the frame due for presentation (or a wait delay / EOF).
    pub fn step_video(&mut self, world_now_ms: i64) -> Result<FramePopOutcome> {
        if self.stage == PlayerStage::Ready && !self.paused_by_user {
            self.start(world_now_ms);
        }
        self.fill_buffers()?;
        self.update_starvation(world_now_ms);
        let track_now = self.clock.track_time(world_now_ms).track_time_ms;
        let outcome = self.video_queue.pop_ready(track_now, self.options.speed);
        match &outcome {
            FramePopOutcome::Ready { frame, .. } => {
                self.current_video_frame = Some(frame.clone());
                self.video_position_ms = frame.pts_ms;
                self.last_video_duration_ms = frame.duration_ms;
                let reported_pos = if self.options.loop_playback {
                    let dur = self.total_duration_ms();
                    if dur > 0 && dur != DURATION_UNAVAILABLE {
                        frame.pts_ms.rem_euclid(dur)
                    } else {
                        frame.pts_ms
                    }
                } else {
                    frame.pts_ms
                };
                self.updates
                    .push_back(StreamingUpdate::UpdateVideo(reported_pos));
            }
            FramePopOutcome::Empty if self.eof_reached && self.stage != PlayerStage::Finished => {
                self.stage = PlayerStage::Finished;
                self.updates.push_back(StreamingUpdate::Finished);
            }
            _ => {}
        }
        Ok(outcome)
    }

    /// Decodes and returns the next sequential video frame directly (ignoring wall-clock pacing).
    /// Useful for offline decoding, thumbnails, and the `play` CLI example.
    pub fn next_video_frame(&mut self) -> Result<Option<VideoFrame>> {
        loop {
            match self.decoder.next_frame()? {
                DecodedPacket::Video(frame) => {
                    self.video_position_ms = frame.pts_ms;
                    self.current_video_frame = Some(frame.clone());
                    return Ok(Some(frame));
                }
                DecodedPacket::Audio(frame) => {
                    self.audio_queue.push_back(frame);
                }
                DecodedPacket::EndOfStream => {
                    self.eof_reached = true;
                    self.stage = PlayerStage::Finished;
                    return Ok(None);
                }
                DecodedPacket::Pending => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }

    /// Waits for data when a background decoder runs dry because the loader has not delivered
    /// the bytes yet, instead of letting the clock run on and then dropping every frame that
    /// arrives late; resumes once the frame queue is full again, restarting the clock at the
    /// first queued frame so nothing is skipped. A decoder that is merely slow (busy CPU) keeps
    /// the clock running and late frames are dropped, as tdesktop's `VideoTrack` does.
    fn update_starvation(&mut self, world_now_ms: i64) {
        if self.waiting_starved {
            if !self.video_queue.needs_frame() || self.eof_reached {
                if let Some(pts) = self.video_queue.front_pts() {
                    self.clock.seek(pts, Some(world_now_ms));
                }
                self.waiting_starved = false;
                self.set_waiting_for_data(false, world_now_ms);
            }
            return;
        }
        if !self.decoder_pending
            || self.paused_by_user
            || self.paused_by_waiting
            || self.stage != PlayerStage::Started
            || self.eof_reached
            || !self.video_queue.is_empty()
            || self.decoder.info().video.is_none()
            || !self.decoder.waiting_for_data()
        {
            return;
        }
        let shown_till = self.video_position_ms + self.last_video_duration_ms.max(1);
        let track_now = self.clock.track_time(world_now_ms).track_time_ms;
        if track_now > shown_till + STARVE_GRACE_MS {
            self.waiting_starved = true;
            self.set_waiting_for_data(true, world_now_ms);
        }
    }

    /// `true` while playback waits for the background decoder to refill the frame queue.
    #[must_use]
    pub const fn is_waiting_for_data(&self) -> bool {
        self.paused_by_waiting
    }

    /// Sets waiting-for-data buffering pause state and emits [`StreamingUpdate::WaitingForData`].
    pub fn set_waiting_for_data(&mut self, waiting: bool, world_now_ms: i64) {
        if self.paused_by_waiting == waiting {
            return;
        }
        self.paused_by_waiting = waiting;
        if waiting {
            self.clock.pause(world_now_ms);
        } else if !self.paused_by_user {
            self.clock.resume(world_now_ms);
        }
        self.updates
            .push_back(StreamingUpdate::WaitingForData(waiting));
    }

    /// Returns `true` when buffered video/audio exceeds `BUFFER_FOR_MS` (or reached EOF).
    #[must_use]
    pub fn has_buffered_enough(&self) -> bool {
        if self.eof_reached {
            return true;
        }
        let v_ok = self.decoder.info().video.is_none()
            || self.video_received_till_ms >= self.video_position_ms + BUFFER_FOR_MS;
        let a_ok = self.decoder.info().audio.is_none()
            || self.audio_received_till_ms >= self.audio_position_ms + BUFFER_FOR_MS;
        v_ok && a_ok
    }
}
