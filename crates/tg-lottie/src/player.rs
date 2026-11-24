//! Frame scheduling, frame skipping, ring-buffer state, single/multi players, and frame generator.
//!
//! Port of `lottie/details/lottie_frame_renderer.h`, `lottie/details/lottie_frame_renderer.cpp`,
//! `lottie/lottie_animation.cpp`, `lottie/lottie_single_player.cpp`,
//! `lottie/lottie_multi_player.cpp`, and `lottie/lottie_frame_generator.cpp`.

use std::collections::{HashMap, HashSet};

use crate::common::{
    ColorReplacements, DISPLAYED_INITIAL, FRAME_DISPLAY_TIME_ALREADY_DONE, FrameBuffer,
    FrameRequest, IDEAL_SIZE, Information, MAX_FRAMES_COUNT, MAX_SIZE, Quality, Size, TIME_UNKNOWN,
};
use crate::error::Error;
use crate::provider::{
    FrameProvider, FrameProviderCached, FrameProviderCachedMulti, FrameProviderDirect,
    FrameProviderToken, Instance, MultiCachePutFn,
};

/// Number of frame slots in `SharedState`'s ring buffer (`SharedState::kFramesCount`).
pub const RING_FRAMES_COUNT: usize = 4;

/// A single buffered frame slot inside `SharedState` (`Lottie::Frame`).
#[derive(Debug, Clone)]
pub struct Frame {
    /// Raw rendered frame at provider resolution.
    pub original: FrameBuffer,
    /// Timestamp when this frame was displayed (`DISPLAYED_INITIAL` or `TIME_UNKNOWN` if ready).
    pub displayed: i64,
    /// Scheduled display timestamp in milliseconds.
    pub display: i64,
    /// Monotonic frame index (modulo `frames_count` gives animation frame).
    pub index: u32,
    /// Provider size rounding used when rendering `original`.
    pub size_rounding: u32,
    /// Request used to produce `prepared`.
    pub request: FrameRequest,
    /// Post-processed frame (scaled, mirrored, colored).
    pub prepared: FrameBuffer,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            original: FrameBuffer::default(),
            displayed: DISPLAYED_INITIAL,
            display: TIME_UNKNOWN,
            index: 0,
            size_rounding: 2,
            request: FrameRequest::default(),
            prepared: FrameBuffer::default(),
        }
    }
}

/// Returns `true` if `original` directly satisfies `request` without scaling, mirroring, or tinting
/// (`GoodForRequest` in `lottie_frame_renderer.cpp`).
#[must_use]
pub fn good_for_request(image: &FrameBuffer, request: FrameRequest) -> bool {
    if request.is_empty() {
        return true;
    }
    if request.colored.a != 0 || request.mirror_horizontal {
        return false;
    }
    let size = image.size();
    request.box_size.width == size.width || request.box_size.height == size.height
}

/// Scales, mirrors, and tints `original` according to `request`
/// (`PrepareByRequest` in `lottie_frame_renderer.cpp`).
#[must_use]
pub fn prepare_by_request(
    original: &FrameBuffer,
    request: FrameRequest,
    size_rounding: u32,
) -> FrameBuffer {
    let target_size = if request.is_empty() {
        original.size()
    } else {
        request.size(original.size(), size_rounding)
    };
    let mut out = original.scaled_to(target_size);
    if request.mirror_horizontal {
        out.mirror_horizontal_in_place();
    }
    if request.colored.a != 0 {
        out.apply_colored_in_place(request.colored);
    }
    out
}

/// Returns the frame image matching `frame.request`, caching `frame.prepared` as needed
/// (`PrepareFrameByRequest` in `lottie_frame_renderer.cpp`).
pub fn prepare_frame_by_request(frame: &mut Frame, use_existing_prepared: bool) -> FrameBuffer {
    if good_for_request(&frame.original, frame.request) {
        return frame.original.clone();
    }
    if frame.prepared.is_empty() || !use_existing_prepared {
        frame.prepared = prepare_by_request(&frame.original, frame.request, frame.size_rounding);
    }
    frame.prepared.clone()
}

/// Frame buffer paired with its looped animation frame index (`Animation::FrameInfo`).
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// Premultiplied RGBA frame image.
    pub image: FrameBuffer,
    /// Animation frame index (`0..frames_count`).
    pub index: u32,
}

/// 4-frame ring buffer and scheduling state machine (`Lottie::SharedState`).
pub struct SharedState {
    /// Counter cycling `0..8`:
    /// - Even (`0, 2, 4, 6`): waiting for renderer to present next frame.
    /// - Odd (`1, 3, 5, 7`): next frame presented, waiting for display & `mark_frame_shown`.
    counter: i32,
    frames: [Frame; RING_FRAMES_COUNT],
    started: i64,
    delay: i64,
    frame_index: u32,
    frames_count: u32,
    frame_rate: u32,
    skipped_frames: i64,
    provider: Box<dyn FrameProvider>,
    token: Option<FrameProviderToken>,
}

impl SharedState {
    /// Constructs a `SharedState` and renders the cover frame (frame 0).
    pub fn new(mut provider: Box<dyn FrameProvider>, request: FrameRequest) -> Result<Self, Error> {
        if !provider.valid() {
            return Err(Error::ParseFailed("frame provider is not valid".into()));
        }
        let effective_req = if request.is_empty() {
            FrameRequest::new(IDEAL_SIZE)
        } else {
            request
        };
        let mut token = None;
        let cover = provider.construct(&mut token, effective_req);
        if cover.is_empty() {
            return Err(Error::RenderFailed("failed to render cover frame".into()));
        }
        let info = provider.information();
        if info.frame_rate == 0 || info.frames_count == 0 || info.size.is_empty() {
            return Err(Error::NotSupported(
                "invalid animation information in SharedState".into(),
            ));
        }

        let rounding = provider.size_rounding();
        let mut frames: [Frame; RING_FRAMES_COUNT] = Default::default();
        frames[0].request = effective_req;
        frames[0].size_rounding = rounding;
        frames[0].original = cover;

        Ok(Self {
            counter: -1,
            frames,
            started: TIME_UNKNOWN,
            delay: TIME_UNKNOWN,
            frame_index: 0,
            frames_count: info.frames_count,
            frame_rate: info.frame_rate,
            skipped_frames: 0,
            provider,
            token,
        })
    }

    /// Starts the playback timeline at `started_ms` with optional initial `delay_ms` and
    /// `skipped_frames` (`SharedState::start`).
    pub fn start(&mut self, started_ms: i64, delay_ms: i64, skipped_frames: i64) {
        self.started = started_ms;
        self.delay = delay_ms;
        self.skipped_frames = skipped_frames;
        self.counter = 0;
    }

    /// Returns `true` if `start()` has been called.
    #[must_use]
    pub const fn initialized(&self) -> bool {
        self.counter >= 0
    }

    /// Returns the animation metadata.
    #[must_use]
    pub fn information(&self) -> Information {
        if self.frames_count > 0 {
            self.provider.information()
        } else {
            Information::default()
        }
    }

    /// Number of effective frames.
    #[must_use]
    pub const fn frames_count(&self) -> u32 {
        self.frames_count
    }

    /// Effective frame rate in fps.
    #[must_use]
    pub const fn frame_rate(&self) -> u32 {
        self.frame_rate
    }

    /// Returns the current frame slot to paint on screen (`SharedState::frameForPaint`).
    pub fn frame_for_paint(&mut self) -> &mut Frame {
        let idx = if self.counter < 0 {
            0
        } else {
            (self.counter / 2) as usize % RING_FRAMES_COUNT
        };
        &mut self.frames[idx]
    }

    /// Returns a read-only reference to the current frame slot.
    #[must_use]
    pub fn frame_for_paint_ref(&self) -> &Frame {
        let idx = if self.counter < 0 {
            0
        } else {
            (self.counter / 2) as usize % RING_FRAMES_COUNT
        };
        &self.frames[idx]
    }

    /// Computes the scheduled display timestamp for frame `index` (`countFrameDisplayTime`).
    #[must_use]
    pub fn count_frame_display_time(&self, index: u32) -> i64 {
        if self.frame_rate == 0 {
            return self.started.saturating_add(self.delay);
        }
        let total_frames = self.skipped_frames.saturating_add(i64::from(index));
        self.started
            .saturating_add(self.delay)
            .saturating_add((1000 * total_frames) / i64::from(self.frame_rate))
    }

    /// Returns the display timestamp of the next ready frame, or `TIME_UNKNOWN` /
    /// `FRAME_DISPLAY_TIME_ALREADY_DONE` (`SharedState::nextFrameDisplayTime`).
    #[must_use]
    pub fn next_frame_display_time(&self) -> i64 {
        if self.counter < 0 || self.counter % 2 == 0 {
            return TIME_UNKNOWN;
        }
        let next = (self.counter + 1) % (2 * RING_FRAMES_COUNT as i32);
        let idx = (next / 2) as usize;
        let frame = &self.frames[idx];
        if frame.displayed != TIME_UNKNOWN {
            return FRAME_DISPLAY_TIME_ALREADY_DONE;
        }
        frame.display
    }

    /// Adds `delayed_ms` and/or `skipped_frames` to the timeline and recalculates the next
    /// frame's display timestamp (`SharedState::addTimelineDelay`).
    pub fn add_timeline_delay(&mut self, delayed_ms: i64, skipped_frames: i64) {
        if delayed_ms == 0 && skipped_frames == 0 {
            return;
        }
        if self.counter < 0 || self.counter % 2 == 0 {
            return;
        }
        self.delay = self.delay.saturating_add(delayed_ms);
        self.skipped_frames = self.skipped_frames.saturating_add(skipped_frames);

        let next = (self.counter + 1) % (2 * RING_FRAMES_COUNT as i32);
        let idx = (next / 2) as usize;
        if self.frames[idx].displayed == TIME_UNKNOWN {
            let f_idx = self.frames[idx].index;
            self.frames[idx].display = self.count_frame_display_time(f_idx);
        }
    }

    /// Marks the pending next frame as displayed at `now_ms` (`SharedState::markFrameDisplayed`).
    pub fn mark_frame_displayed(&mut self, now_ms: i64) {
        if self.counter < 0 || self.counter % 2 == 0 {
            return;
        }
        let next = (self.counter + 1) % (2 * RING_FRAMES_COUNT as i32);
        let idx = (next / 2) as usize;
        if self.frames[idx].displayed == TIME_UNKNOWN {
            self.frames[idx].displayed = now_ms;
        }
    }

    /// Advances the paint slot to the displayed frame (`SharedState::markFrameShown`).
    /// Returns `true` if the counter advanced and a new slot is available for prerendering.
    pub fn mark_frame_shown(&mut self) -> bool {
        if self.counter < 0 || self.counter % 2 == 0 {
            return false;
        }
        let next = (self.counter + 1) % (2 * RING_FRAMES_COUNT as i32);
        let idx = (next / 2) as usize;
        if self.frames[idx].displayed == TIME_UNKNOWN {
            return false;
        }
        self.counter = next;
        true
    }

    /// Renders or prerenders the next frame in the ring buffer (`SharedState::renderNextFrame`).
    /// Returns `(rendered_any, presented_to_display)`.
    pub fn render_next_frame(&mut self, request: FrameRequest) -> (bool, bool) {
        if self.counter < 0 || self.frames_count == 0 {
            return (false, false);
        }
        let c = self.counter;
        if c % 2 == 0 {
            // Present slot `(c / 2 + 1) % 4`
            let idx = ((c / 2) as usize + 1) % RING_FRAMES_COUNT;
            if self.frames[idx].displayed != TIME_UNKNOWN {
                self.render_into_slot(idx, request);
                if self.frames[idx].displayed != TIME_UNKNOWN {
                    return (false, false);
                }
            }
            let f_idx = self.frames[idx].index;
            self.frames[idx].display = self.count_frame_display_time(f_idx);
            self.counter = (c + 1) % (2 * RING_FRAMES_COUNT as i32);
            (true, true)
        } else {
            // Prerender slot `((c - 1) / 2 + 2) % 4` or the one after it.
            let idx = (((c - 1) / 2) as usize + 2) % RING_FRAMES_COUNT;
            let next_idx = (idx + 1) % RING_FRAMES_COUNT;
            if self.frames[idx].displayed != TIME_UNKNOWN {
                self.render_into_slot(idx, request);
                return (self.frames[idx].displayed == TIME_UNKNOWN, false);
            } else if self.frames[next_idx].displayed != TIME_UNKNOWN {
                self.render_into_slot(next_idx, request);
                return (self.frames[next_idx].displayed == TIME_UNKNOWN, false);
            }
            (false, false)
        }
    }

    fn render_into_slot(&mut self, slot: usize, request: FrameRequest) {
        if self.frames_count == 0 {
            return;
        }
        self.frame_index = self.frame_index.wrapping_add(1);
        let target_idx = self.frame_index % self.frames_count;
        let ok = self.provider.render(
            &mut self.token,
            &mut self.frames[slot].original,
            request,
            target_idx,
        );
        if !ok {
            return;
        }
        self.frames[slot].request = request;
        self.frames[slot].size_rounding = self.provider.size_rounding();
        prepare_frame_by_request(&mut self.frames[slot], false);
        self.frames[slot].index = self.frame_index;
        self.frames[slot].displayed = TIME_UNKNOWN;
    }

    /// Computes the looped animation frame index for an elapsed duration in milliseconds.
    #[must_use]
    pub fn frame_index_for_time(&self, elapsed_ms: i64) -> u32 {
        if self.frames_count == 0 || self.frame_rate == 0 {
            return 0;
        }
        let elapsed = elapsed_ms.max(0) as u64;
        let frame = (elapsed * u64::from(self.frame_rate)) / 1000;
        (frame % u64::from(self.frames_count)) as u32
    }
}

/// Single-animation player with frame scheduling and caching (`Lottie::SinglePlayer`).
pub struct SinglePlayer {
    state: SharedState,
    request: FrameRequest,
    next_frame_time: i64,
}

impl SinglePlayer {
    /// Creates a `SinglePlayer` directly from plain JSON or `.tgs` bytes.
    pub fn from_bytes(
        content: &[u8],
        request: FrameRequest,
        quality: Quality,
        replacements: Option<&ColorReplacements>,
    ) -> Result<Self, Error> {
        let mut direct = FrameProviderDirect::new(quality);
        direct.load(content, replacements)?;
        Self::from_provider(Box::new(direct), request)
    }

    /// Creates a `SinglePlayer` backed by a single compressed frame cache.
    pub fn from_cached(
        content: Vec<u8>,
        cached: Vec<u8>,
        put: Option<Box<dyn FnMut(Vec<u8>) + Send>>,
        request: FrameRequest,
        quality: Quality,
        replacements: Option<ColorReplacements>,
    ) -> Result<Self, Error> {
        let effective_req = if request.is_empty() {
            FrameRequest::new(IDEAL_SIZE)
        } else {
            request
        };
        let provider =
            FrameProviderCached::new(content, put, cached, effective_req, quality, replacements);
        Self::from_provider(Box::new(provider), effective_req)
    }

    /// Creates a `SinglePlayer` backed by `N` multi-cache blobs.
    pub fn from_cached_multi(
        content: Vec<u8>,
        caches: Vec<Vec<u8>>,
        put: Option<MultiCachePutFn>,
        request: FrameRequest,
        quality: Quality,
        replacements: Option<ColorReplacements>,
    ) -> Result<Self, Error> {
        let effective_req = if request.is_empty() {
            FrameRequest::new(IDEAL_SIZE)
        } else {
            request
        };
        let provider = FrameProviderCachedMulti::new(
            content,
            put,
            caches,
            effective_req,
            quality,
            replacements,
        );
        Self::from_provider(Box::new(provider), effective_req)
    }

    /// Creates a `SinglePlayer` from any `FrameProvider`.
    pub fn from_provider(
        provider: Box<dyn FrameProvider>,
        request: FrameRequest,
    ) -> Result<Self, Error> {
        let effective_req = if request.is_empty() {
            FrameRequest::new(IDEAL_SIZE)
        } else {
            request
        };
        let state = SharedState::new(provider, effective_req)?;
        Ok(Self {
            state,
            request: effective_req,
            next_frame_time: TIME_UNKNOWN,
        })
    }

    /// Starts playback at timestamp `now_ms` and renders the next frame into the ring buffer.
    pub fn start(&mut self, now_ms: i64) {
        self.state.start(now_ms, 0, 0);
        self.next_frame_time = TIME_UNKNOWN;
        self.pump_renderer();
    }

    /// Returns animation metadata.
    #[must_use]
    pub fn information(&self) -> Information {
        self.state.information()
    }

    /// Returns total effective frame count.
    #[must_use]
    pub const fn frames_count(&self) -> u32 {
        self.state.frames_count()
    }

    /// Returns the currently displayed animation frame index (`0..frames_count`).
    #[must_use]
    pub fn frame_index(&self) -> u32 {
        let count = self.state.frames_count();
        if count == 0 {
            return 0;
        }
        self.state.frame_for_paint_ref().index % count
    }

    /// Computes the looped frame index for an elapsed time in milliseconds.
    #[must_use]
    pub fn frame_index_for_time(&self, elapsed_ms: i64) -> u32 {
        self.state.frame_index_for_time(elapsed_ms)
    }

    /// Returns the timestamp (ms) when the next frame should be displayed, or `TIME_UNKNOWN`.
    #[must_use]
    pub fn next_frame_display_time(&self) -> i64 {
        if self.next_frame_time != TIME_UNKNOWN {
            self.next_frame_time
        } else {
            self.state.next_frame_display_time()
        }
    }

    /// Returns the current frame image prepared for the player's current request.
    pub fn frame(&mut self) -> FrameBuffer {
        prepare_frame_by_request(self.state.frame_for_paint(), true)
    }

    /// Returns the current frame image prepared for `request`, updating the active request if changed.
    pub fn frame_with_request(&mut self, request: FrameRequest) -> FrameBuffer {
        let frame = self.state.frame_for_paint();
        let changed = frame.request != request;
        if changed {
            frame.request = request;
            self.request = request;
        }
        prepare_frame_by_request(frame, !changed)
    }

    /// Returns both the prepared frame image and its animation frame index.
    pub fn frame_info(&mut self, request: FrameRequest) -> FrameInfo {
        let count = self.state.frames_count().max(1);
        let image = self.frame_with_request(request);
        let index = self.state.frame_for_paint_ref().index % count;
        FrameInfo { image, index }
    }

    /// Checks playback at `now_ms`. If the next frame is due, marks it displayed, adds any
    /// overrun delay (`now_ms - scheduled_time`), and returns `true` (`SinglePlayer::checkStep`).
    pub fn check_step(&mut self, now_ms: i64) -> bool {
        if !self.state.initialized() {
            self.start(now_ms);
        }
        if self.next_frame_time == FRAME_DISPLAY_TIME_ALREADY_DONE {
            return false;
        }
        if self.next_frame_time == TIME_UNKNOWN {
            self.pump_renderer();
            let next = self.state.next_frame_display_time();
            if next == TIME_UNKNOWN || next == FRAME_DISPLAY_TIME_ALREADY_DONE {
                return false;
            }
            self.next_frame_time = next;
        }
        if now_ms < self.next_frame_time {
            return false;
        }
        let scheduled = self.next_frame_time;
        self.state.mark_frame_displayed(now_ms);
        self.state.add_timeline_delay(now_ms - scheduled, 0);
        self.next_frame_time = FRAME_DISPLAY_TIME_ALREADY_DONE;
        true
    }

    /// Marks the displayed frame as painted on screen, advancing the ring buffer and rendering
    /// the subsequent frame (`SinglePlayer::markFrameShown`).
    pub fn mark_frame_shown(&mut self) -> bool {
        if self.next_frame_time == FRAME_DISPLAY_TIME_ALREADY_DONE {
            self.next_frame_time = TIME_UNKNOWN;
        }
        let advanced = self.state.mark_frame_shown();
        if advanced {
            self.pump_renderer();
        }
        advanced
    }

    /// Convenience helper combining `check_step(now_ms)` and `mark_frame_shown()`: advances the
    /// player if the next frame is due at `now_ms`, returning `Some(new_frame_index)` on advance.
    pub fn advance(&mut self, now_ms: i64) -> Option<u32> {
        if self.check_step(now_ms) && self.mark_frame_shown() {
            Some(self.frame_index())
        } else {
            None
        }
    }

    /// Renders up to one presentation + prerender step in the ring buffer.
    fn pump_renderer(&mut self) {
        let req = self.request;
        for _ in 0..RING_FRAMES_COUNT {
            let (rendered, _) = self.state.render_next_frame(req);
            if !rendered {
                break;
            }
        }
    }
}

/// Identifier of an animation registered inside a `MultiPlayer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnimationId(pub u64);

struct PausedInfo {
    state: SharedState,
    request: FrameRequest,
    pause_time: i64,
    pause_delay: i64,
}

struct ActiveEntry {
    state: SharedState,
    request: FrameRequest,
}

/// Synchronized multi-animation player (`Lottie::MultiPlayer`).
///
/// Shares a single timeline (`started`, `last_sync_time`, `delay`) across all active
/// animations and computes skipped catch-up frames when unpausing or joining mid-playback.
pub struct MultiPlayer {
    quality: Quality,
    next_id: u64,
    active: HashMap<AnimationId, ActiveEntry>,
    paused: HashMap<AnimationId, PausedInfo>,
    pending_pause: HashSet<AnimationId>,
    pending_unpause: HashSet<AnimationId>,
    pending_remove: HashSet<AnimationId>,
    started: i64,
    last_sync_time: i64,
    delay: i64,
    next_frame_time: i64,
}

impl MultiPlayer {
    /// Creates a new `MultiPlayer` with the specified `quality`.
    #[must_use]
    pub fn new(quality: Quality) -> Self {
        Self {
            quality,
            next_id: 1,
            active: HashMap::new(),
            paused: HashMap::new(),
            pending_pause: HashSet::new(),
            pending_unpause: HashSet::new(),
            pending_remove: HashSet::new(),
            started: TIME_UNKNOWN,
            last_sync_time: TIME_UNKNOWN,
            delay: 0,
            next_frame_time: TIME_UNKNOWN,
        }
    }

    /// Appends an animation from JSON / `.tgs` bytes and starts it synchronized with existing ones.
    pub fn append(
        &mut self,
        content: &[u8],
        request: FrameRequest,
        replacements: Option<&ColorReplacements>,
        now_ms: i64,
    ) -> Result<AnimationId, Error> {
        let mut direct = FrameProviderDirect::new(self.quality);
        direct.load(content, replacements)?;
        self.append_provider(Box::new(direct), request, now_ms)
    }

    /// Appends an animation from any `FrameProvider`.
    pub fn append_provider(
        &mut self,
        provider: Box<dyn FrameProvider>,
        request: FrameRequest,
        now_ms: i64,
    ) -> Result<AnimationId, Error> {
        let effective_req = if request.is_empty() {
            FrameRequest::new(IDEAL_SIZE)
        } else {
            request
        };
        let mut state = SharedState::new(provider, effective_req)?;
        if self.started == TIME_UNKNOWN {
            self.started = now_ms;
            self.last_sync_time = TIME_UNKNOWN;
            self.delay = 0;
        }
        let sync_time = if self.last_sync_time != TIME_UNKNOWN {
            self.last_sync_time
        } else {
            self.started
        };
        let skipped = self.count_frame_index(&state, sync_time, self.delay);
        state.start(self.started, self.delay, skipped);
        for _ in 0..RING_FRAMES_COUNT {
            if !state.render_next_frame(effective_req).0 {
                break;
            }
        }
        let id = AnimationId(self.next_id);
        self.next_id += 1;
        self.active.insert(
            id,
            ActiveEntry {
                state,
                request: effective_req,
            },
        );
        Ok(id)
    }

    /// Port of `MultiPlayer::countFrameIndex` (`lottie_multi_player.cpp:85-96`).
    #[must_use]
    pub fn count_frame_index(&self, state: &SharedState, time: i64, delay: i64) -> i64 {
        let rate = i64::from(state.frame_rate());
        if rate == 0 || self.started == TIME_UNKNOWN {
            return 0;
        }
        let frames_time = time - self.started - delay;
        ((frames_time + 1) * rate - 1) / 1000
    }

    /// Schedules an animation to be paused at the next sync point (or immediately if idle).
    pub fn pause(&mut self, id: AnimationId) {
        if self.active.contains_key(&id) {
            if self.last_sync_time != TIME_UNKNOWN && self.next_frame_time == TIME_UNKNOWN {
                self.pause_and_save_state(id);
            } else {
                self.pending_pause.insert(id);
            }
        } else {
            self.pending_unpause.remove(&id);
        }
    }

    /// Schedules a paused animation to resume and catch up (`MultiPlayer::unpause`).
    pub fn unpause(&mut self, id: AnimationId, now_ms: i64) {
        if self.paused.contains_key(&id) {
            if self.active.is_empty() {
                if let Some(info) = self.paused.remove(&id) {
                    let sync = if self.last_sync_time != TIME_UNKNOWN {
                        self.last_sync_time
                    } else {
                        now_ms
                    };
                    self.active.insert(
                        id,
                        ActiveEntry {
                            state: info.state,
                            request: info.request,
                        },
                    );
                    self.add_timeline_delay(now_ms - sync);
                    self.last_sync_time = now_ms;
                    self.mark_frame_shown();
                }
            } else {
                self.pending_unpause.insert(id);
            }
        } else {
            self.pending_pause.remove(&id);
        }
    }

    /// Removes an animation from the player.
    pub fn remove(&mut self, id: AnimationId) {
        if !self.active.is_empty() && self.next_frame_time != TIME_UNKNOWN {
            self.pending_remove.insert(id);
        } else {
            self.remove_now(id);
        }
    }

    /// Returns the number of active animations.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Returns the number of paused animations.
    #[must_use]
    pub fn paused_count(&self) -> usize {
        self.paused.len()
    }

    /// Returns the current prepared frame for `id`.
    pub fn frame(&mut self, id: AnimationId) -> Option<FrameBuffer> {
        if let Some(entry) = self.active.get_mut(&id) {
            return Some(prepare_frame_by_request(
                entry.state.frame_for_paint(),
                true,
            ));
        }
        if let Some(info) = self.paused.get_mut(&id) {
            return Some(prepare_frame_by_request(info.state.frame_for_paint(), true));
        }
        None
    }

    /// Returns the current animation frame index for `id`.
    #[must_use]
    pub fn frame_index(&self, id: AnimationId) -> Option<u32> {
        let state = self
            .active
            .get(&id)
            .map(|e| &e.state)
            .or_else(|| self.paused.get(&id).map(|p| &p.state))?;
        let count = state.frames_count();
        if count == 0 {
            return Some(0);
        }
        Some(state.frame_for_paint_ref().index % count)
    }

    /// Steps all active animations at `now_ms`. Returns `true` if a new display tick fired.
    pub fn check_step(&mut self, now_ms: i64) -> bool {
        if self.active.is_empty() || self.next_frame_time == FRAME_DISPLAY_TIME_ALREADY_DONE {
            return false;
        }
        if self.next_frame_time == TIME_UNKNOWN {
            let mut earliest = TIME_UNKNOWN;
            for entry in self.active.values() {
                let t = entry.state.next_frame_display_time();
                if t == TIME_UNKNOWN {
                    return false;
                }
                if t == FRAME_DISPLAY_TIME_ALREADY_DONE {
                    continue;
                }
                if earliest == TIME_UNKNOWN || t < earliest {
                    earliest = t;
                }
            }
            if earliest == TIME_UNKNOWN {
                return false;
            }
            self.next_frame_time = earliest;
        }
        if now_ms < self.next_frame_time {
            return false;
        }
        let scheduled = self.next_frame_time;
        for entry in self.active.values_mut() {
            let t = entry.state.next_frame_display_time();
            if t != FRAME_DISPLAY_TIME_ALREADY_DONE && t != TIME_UNKNOWN && now_ms >= t {
                entry.state.mark_frame_displayed(now_ms);
            }
        }
        self.add_timeline_delay(now_ms - scheduled);
        self.last_sync_time = now_ms;
        self.next_frame_time = FRAME_DISPLAY_TIME_ALREADY_DONE;
        self.process_pending();
        true
    }

    /// Marks frames shown for all active animations and prerenders subsequent frames.
    pub fn mark_frame_shown(&mut self) -> bool {
        if self.next_frame_time == FRAME_DISPLAY_TIME_ALREADY_DONE {
            self.next_frame_time = TIME_UNKNOWN;
        }
        let mut count = 0usize;
        for entry in self.active.values_mut() {
            if entry.state.mark_frame_shown() {
                count += 1;
                for _ in 0..RING_FRAMES_COUNT {
                    if !entry.state.render_next_frame(entry.request).0 {
                        break;
                    }
                }
            }
        }
        count > 0
    }

    fn add_timeline_delay(&mut self, delayed: i64) {
        if delayed == 0 {
            return;
        }
        for entry in self.active.values_mut() {
            entry.state.add_timeline_delay(delayed, 0);
        }
        self.delay = self.delay.saturating_add(delayed);
    }

    fn process_pending(&mut self) {
        let to_pause: Vec<_> = self.pending_pause.drain().collect();
        for id in to_pause {
            self.pause_and_save_state(id);
        }
        let to_unpause: Vec<_> = self.pending_unpause.drain().collect();
        for id in to_unpause {
            self.unpause_and_keep_up(id);
        }
        let to_remove: Vec<_> = self.pending_remove.drain().collect();
        for id in to_remove {
            self.remove_now(id);
        }
    }

    fn pause_and_save_state(&mut self, id: AnimationId) {
        if let Some(entry) = self.active.remove(&id) {
            self.paused.insert(
                id,
                PausedInfo {
                    state: entry.state,
                    request: entry.request,
                    pause_time: self.last_sync_time,
                    pause_delay: self.delay,
                },
            );
        }
    }

    fn unpause_and_keep_up(&mut self, id: AnimationId) {
        if let Some(mut info) = self.paused.remove(&id) {
            let idx_paused = self.count_frame_index(&info.state, info.pause_time, info.pause_delay);
            let idx_now = self.count_frame_index(&info.state, self.last_sync_time, self.delay);
            info.state
                .add_timeline_delay(self.delay - info.pause_delay, idx_now - idx_paused);
            self.active.insert(
                id,
                ActiveEntry {
                    state: info.state,
                    request: info.request,
                },
            );
        }
    }

    fn remove_now(&mut self, id: AnimationId) {
        self.active.remove(&id);
        self.paused.remove(&id);
        self.pending_remove.remove(&id);
        self.pending_pause.remove(&id);
        self.pending_unpause.remove(&id);
        if self.active.is_empty() {
            self.next_frame_time = TIME_UNKNOWN;
            if self.paused.is_empty() {
                self.started = TIME_UNKNOWN;
                self.last_sync_time = TIME_UNKNOWN;
                self.delay = 0;
            }
        }
    }
}

/// Aspect ratio scaling mode for `FrameGenerator`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AspectRatioMode {
    /// Fit inside target size preserving aspect ratio (letterbox margins transparent).
    #[default]
    KeepAspectRatio,
    /// Expand to cover target size preserving aspect ratio (cropped to center-bottom).
    KeepAspectRatioByExpanding,
}

/// Output produced by `FrameGenerator::render_next`.
#[derive(Debug, Clone)]
pub struct GeneratedFrame {
    /// Frame display duration in milliseconds.
    pub duration_ms: i64,
    /// Rendered premultiplied RGBA frame.
    pub image: FrameBuffer,
    /// `true` if this was the final frame of the animation.
    pub last: bool,
}

/// Sequential frame generator (`Lottie::FrameGenerator` in `lottie_frame_generator.cpp`).
pub struct FrameGenerator {
    instance: Option<Instance>,
    size: Size,
    frames_count: u32,
    frame_index: u32,
    frame_duration_ms: i64,
    multiplier: u32,
}

impl FrameGenerator {
    /// Creates a `FrameGenerator` from plain Lottie JSON or `.tgs` bytes, applying tdesktop's
    /// 60->30 fps halving and bounds validation.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let instance = Instance::from_bytes(bytes, None)?;
        let rate = instance.frame_rate();
        let multiplier = if rate.round() as u32 == 60 { 2 } else { 1 };
        let size = instance.size();
        let frames_count = instance.frames_count().div_ceil(multiplier);
        let frame_duration_ms = if rate > 0.0 {
            ((1000.0 * f64::from(multiplier)) / rate).round() as i64
        } else {
            0
        };
        if frames_count == 0
            || frames_count > MAX_FRAMES_COUNT
            || frame_duration_ms <= 0
            || size.is_empty()
            || size.width > MAX_SIZE
            || size.height > MAX_SIZE
        {
            return Err(Error::NotSupported(
                "animation violates FrameGenerator bounds".into(),
            ));
        }
        Ok(Self {
            instance: Some(instance),
            size,
            frames_count,
            frame_index: 0,
            frame_duration_ms,
            multiplier,
        })
    }

    /// Total effective frame count.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.frames_count
    }

    /// Effective frame rate in frames per second.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.instance
            .as_ref()
            .map_or(0.0, |i| i.frame_rate() / f64::from(self.multiplier))
    }

    /// Duration of each generated frame in milliseconds.
    #[must_use]
    pub const fn frame_duration_ms(&self) -> i64 {
        self.frame_duration_ms
    }

    /// Resets the sequential cursor back to frame 0 (`FrameGenerator::jumpToStart`).
    pub fn jump_to_start(&mut self) {
        self.frame_index = 0;
    }

    /// Renders the next frame in sequence into a `FrameBuffer` of `size` (`FrameGenerator::renderNext`).
    pub fn render_next(&mut self, size: Size, mode: AspectRatioMode) -> Option<GeneratedFrame> {
        let inst = self.instance.as_ref()?;
        if self.frames_count == 0 || self.frame_index >= self.frames_count || size.is_empty() {
            return None;
        }
        let idx = self.frame_index;
        self.frame_index += 1;

        let render_size = match mode {
            AspectRatioMode::KeepAspectRatio => self.size.scaled_keep_aspect(size),
            AspectRatioMode::KeepAspectRatioByExpanding => {
                self.size.scaled_keep_aspect_expanding(size)
            }
        };

        let mut storage = FrameBuffer::new(size);
        if render_size.width <= size.width && render_size.height <= size.height {
            inst.render_to_prepared(&mut storage, idx * self.multiplier);
        } else {
            let mut expanded = FrameBuffer::new(render_size);
            inst.render_to_prepared(&mut expanded, idx * self.multiplier);
            // Crop matching lottie_frame_generator.cpp: horizontally centered, bottom-aligned.
            let crop_x = (render_size.width.saturating_sub(size.width)) / 2;
            let crop_y = render_size.height.saturating_sub(size.height);
            for y in 0..size.height {
                let sy = crop_y + y;
                if sy >= render_size.height {
                    continue;
                }
                let src_row = (sy as usize * render_size.width as usize + crop_x as usize) * 4;
                let dst_row = (y as usize * size.width as usize) * 4;
                let copy_len = (size.width as usize).min((render_size.width - crop_x) as usize) * 4;
                storage.data[dst_row..(dst_row + copy_len)]
                    .copy_from_slice(&expanded.data[src_row..(src_row + copy_len)]);
            }
        }

        Some(GeneratedFrame {
            duration_ms: self.frame_duration_ms,
            image: storage,
            last: self.frame_index == self.frames_count,
        })
    }
}

/// Renders frame 0 of a Lottie / `.tgs` payload at 512x512 (`Lottie::ReadThumbnail`).
pub fn read_thumbnail(content: &[u8]) -> Result<FrameBuffer, Error> {
    let mut direct = FrameProviderDirect::new(Quality::High);
    direct.load(content, None)?;
    let state = SharedState::new(Box::new(direct), FrameRequest::new(IDEAL_SIZE))?;
    Ok(state.frame_for_paint_ref().original.clone())
}
