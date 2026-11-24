//! Shared streaming document and per-view instance handles with priority arbitration
//! and adaptive quality switching.
//!
//! Port of `Telegram/SourceFiles/media/streaming/media_streaming_document.cpp` and
//! `Telegram/SourceFiles/media/streaming/media_streaming_instance.cpp`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::streaming::decoder::Decoder;
use crate::streaming::player::StreamingPlayer;
use crate::types::{
    DURATION_UNAVAILABLE, FrameRequest, QualityDescriptor, SpeedEstimate, StreamInformation,
    TIME_UNKNOWN, TrackState, VideoFrame,
};

/// Minimum preloaded headroom (4 seconds) before considering a quality upgrade
/// (`kSwitchQualityUpPreloadedThreshold`).
pub const SWITCH_QUALITY_UP_PRELOADED_THRESHOLD_MS: i64 = 4_000;

/// Bandwidth safety margin multiplier before upgrading quality
/// (`kSwitchQualityUpSpeedMultiplier`).
pub const SWITCH_QUALITY_UP_SPEED_MULTIPLIER: f64 = 1.2;

/// Unique identifier for a registered [`StreamingInstance`].
pub type InstanceId = u64;

#[derive(Debug, Clone)]
struct InstanceEntry {
    priority: i32,
    request: FrameRequest,
}

struct DocumentInner<D: Decoder> {
    player: StreamingPlayer<D>,
    info: StreamInformation,
    instances: BTreeMap<InstanceId, InstanceEntry>,
    next_instance_id: InstanceId,
    other_qualities: Vec<QualityDescriptor>,
    last_speed_estimate: SpeedEstimate,
    waiting: bool,
    cover: Option<VideoFrame>,
}

/// Shared streaming document coordinating one [`StreamingPlayer`] across multiple
/// [`StreamingInstance`] views (mirrors `Media::Streaming::Document`).
pub struct StreamingDocument<D: Decoder> {
    inner: Arc<Mutex<DocumentInner<D>>>,
}

impl<D: Decoder> Clone for StreamingDocument<D> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D: Decoder> StreamingDocument<D> {
    /// Creates a new shared document owning `player` and optional alternative quality variants.
    #[must_use]
    pub fn new(player: StreamingPlayer<D>, other_qualities: Vec<QualityDescriptor>) -> Self {
        let info = player.info().clone();
        let cover = info.video.as_ref().and_then(|v| v.cover.clone());
        Self {
            inner: Arc::new(Mutex::new(DocumentInner {
                player,
                info,
                instances: BTreeMap::new(),
                next_instance_id: 1,
                other_qualities,
                last_speed_estimate: SpeedEstimate::default(),
                waiting: false,
                cover,
            })),
        }
    }

    /// Creates a new [`StreamingInstance`] registered with this document.
    pub fn create_instance(&self) -> StreamingInstance<D> {
        let id = if let Ok(mut guard) = self.inner.lock() {
            let id = guard.next_instance_id;
            guard.next_instance_id += 1;
            guard.instances.insert(
                id,
                InstanceEntry {
                    priority: 1,
                    request: FrameRequest::default(),
                },
            );
            refresh_priority_locked(&mut guard);
            id
        } else {
            0
        };
        StreamingInstance {
            doc: self.clone(),
            id,
            player_locked: false,
        }
    }

    /// Runs a closure with mutable access to the underlying [`StreamingPlayer`].
    pub fn with_player<R>(&self, f: impl FnOnce(&mut StreamingPlayer<D>) -> R) -> Option<R> {
        let mut guard = self.inner.lock().ok()?;
        Some(f(&mut guard.player))
    }

    /// Saves the current frame as the document cover (`Document::saveFrameToCover`).
    pub fn save_frame_to_cover(&self) {
        if let Ok(mut guard) = self.inner.lock()
            && guard.player.is_ready()
            && let Some(frame) = guard.player.current_video_frame().cloned()
        {
            guard.cover = Some(frame);
        }
    }

    /// Returns the stored cover frame, if available.
    #[must_use]
    pub fn cover(&self) -> Option<VideoFrame> {
        self.inner.lock().ok()?.cover.clone()
    }

    /// Returns the number of active [`StreamingInstance`] handles.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.inner.lock().map_or(0, |g| g.instances.len())
    }

    /// Updates the list of alternative quality variants and evaluates whether to switch.
    pub fn set_other_qualities(&self, qualities: Vec<QualityDescriptor>) -> Option<u32> {
        let mut guard = self.inner.lock().ok()?;
        guard.other_qualities = qualities;
        let est = guard.last_speed_estimate;
        check_quality_switch_locked(&guard, est)
    }

    /// Updates buffering state and latest video track progress, then evaluates adaptive
    /// quality switching (`Document::checkForQualitySwitch`).
    ///
    /// Returns `Some(target_height)` if switching to another quality is recommended.
    pub fn update_network_state(
        &self,
        estimate: SpeedEstimate,
        video_state: TrackState,
        waiting: bool,
    ) -> Option<u32> {
        let mut guard = self.inner.lock().ok()?;
        guard.last_speed_estimate = estimate;
        guard.waiting = waiting;
        if let Some(video) = guard.info.video.as_mut() {
            video.state = video_state;
        }
        check_quality_switch_locked(&guard, estimate)
    }
}

fn refresh_priority_locked<D: Decoder>(guard: &mut DocumentInner<D>) {
    if let Some(max_prio) = guard.instances.values().map(|e| e.priority).max() {
        guard.player.set_loader_priority(max_prio);
    }
}

fn check_quality_switch_locked<D: Decoder>(
    guard: &DocumentInner<D>,
    estimate: SpeedEstimate,
) -> Option<u32> {
    if let Some(higher) = check_switch_to_higher_quality_locked(guard, estimate) {
        return Some(higher);
    }
    check_switch_to_lower_quality_locked(guard, estimate)
}

fn check_switch_to_higher_quality_locked<D: Decoder>(
    guard: &DocumentInner<D>,
    estimate: SpeedEstimate,
) -> Option<u32> {
    let video = guard.info.video.as_ref()?;
    let state = video.state;
    if guard.other_qualities.is_empty()
        || state.duration_ms == TIME_UNKNOWN
        || state.duration_ms == DURATION_UNAVAILABLE
        || state.duration_ms <= 0
        || state.position_ms == TIME_UNKNOWN
        || state.received_till_ms == TIME_UNKNOWN
        || estimate.bytes_per_second == 0
        || estimate.unreliable
    {
        return None;
    }
    let required_preloaded = state
        .duration_ms
        .min(state.position_ms + SWITCH_QUALITY_UP_PRELOADED_THRESHOLD_MS);
    if state.received_till_ms < required_preloaded {
        return None;
    }
    let current_size = guard.player.file_size();
    let duration_sec = (state.duration_ms as f64) / 1000.0;
    let multiplier = guard.player.speed() * SWITCH_QUALITY_UP_SPEED_MULTIPLIER;

    let mut best = QualityDescriptor {
        height: 0,
        size_in_bytes: current_size,
    };
    for &desc in &guard.other_qualities {
        let per_second = (desc.size_in_bytes as f64) / duration_sec;
        if desc.size_in_bytes > best.size_in_bytes
            && f64::from(estimate.bytes_per_second) >= per_second * multiplier
        {
            best = desc;
        }
    }
    (best.height > 0).then_some(best.height)
}

fn check_switch_to_lower_quality_locked<D: Decoder>(
    guard: &DocumentInner<D>,
    estimate: SpeedEstimate,
) -> Option<u32> {
    if guard.other_qualities.is_empty() || !guard.waiting || estimate.bytes_per_second == 0 {
        return None;
    }
    let current_size = guard.player.file_size();
    let mut best = QualityDescriptor::default();
    for &desc in &guard.other_qualities {
        if desc.size_in_bytes < current_size && desc.size_in_bytes > best.size_in_bytes {
            best = desc;
        }
    }
    (best.height > 0).then_some(best.height)
}

/// Per-view handle to a [`StreamingDocument`] (mirrors `Media::Streaming::Instance`).
///
/// Automatically unregisters itself and releases any held player lock on drop.
pub struct StreamingInstance<D: Decoder> {
    doc: StreamingDocument<D>,
    id: InstanceId,
    player_locked: bool,
}

impl<D: Decoder> StreamingInstance<D> {
    /// Returns the instance identifier.
    #[must_use]
    pub const fn id(&self) -> InstanceId {
        self.id
    }

    /// Sets this instance's download priority and recomputes the document's max priority.
    pub fn set_priority(&self, priority: i32) {
        if let Ok(mut guard) = self.doc.inner.lock()
            && let Some(entry) = guard.instances.get_mut(&self.id)
        {
            if entry.priority == priority {
                return;
            }
            entry.priority = priority;
            refresh_priority_locked(&mut guard);
        }
    }

    /// Returns this instance's current priority.
    #[must_use]
    pub fn priority(&self) -> i32 {
        self.doc
            .inner
            .lock()
            .ok()
            .and_then(|g| g.instances.get(&self.id).map(|e| e.priority))
            .unwrap_or(1)
    }

    /// Updates this instance's preferred [`FrameRequest`].
    pub fn set_frame_request(&self, request: FrameRequest) {
        if let Ok(mut guard) = self.doc.inner.lock()
            && let Some(entry) = guard.instances.get_mut(&self.id)
        {
            entry.request = request;
        }
    }

    /// Acquires a player lock so shared playback stays alive while this view is visible.
    pub fn lock_player(&mut self) {
        if !self.player_locked {
            self.player_locked = true;
            if let Ok(mut guard) = self.doc.inner.lock() {
                guard.player.lock();
            }
        }
    }

    /// Releases the player lock held by this instance.
    pub fn unlock_player(&mut self) {
        if self.player_locked {
            self.player_locked = false;
            if let Ok(mut guard) = self.doc.inner.lock() {
                guard.player.unlock();
            }
        }
    }

    /// Returns `true` if this instance holds a lock on the player.
    #[must_use]
    pub const fn is_player_locked(&self) -> bool {
        self.player_locked
    }

    /// Delegates a seek to the shared player.
    pub fn seek(&self, position_ms: i64, world_now_ms: Option<i64>) -> Result<()> {
        if let Ok(mut guard) = self.doc.inner.lock() {
            guard.player.seek(position_ms, world_now_ms)?;
        }
        Ok(())
    }

    /// Returns a clone of the shared [`StreamingDocument`].
    #[must_use]
    pub fn document(&self) -> StreamingDocument<D> {
        self.doc.clone()
    }
}

impl<D: Decoder> Drop for StreamingInstance<D> {
    fn drop(&mut self) {
        self.unlock_player();
        if let Ok(mut guard) = self.doc.inner.lock() {
            guard.instances.remove(&self.id);
            refresh_priority_locked(&mut guard);
        }
    }
}
