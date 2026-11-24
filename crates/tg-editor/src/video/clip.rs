//! Multi-clip video timeline model: clip metadata, per-clip trim & volume,
//! append/insert/remove/reorder/split operations, and global-to-local timestamp mapping.
//!
//! Port of `Telegram/SourceFiles/editor/video/video_clip.{h,cpp}` extended with
//! multi-clip sequencing for the video editor timeline.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::EditorError;
use crate::geometry::Size;

/// Non-destructive trim range `[from_ms, till_ms]` inside a single video clip
/// (`Editor::VideoTrim` in `photo_editor_inner_common.h`).
///
/// `till_ms == 0` means the end of the source clip (`duration_ms`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoTrim {
    /// Trim start in milliseconds (`>= 0`).
    pub from_ms: i64,
    /// Trim end in milliseconds (`0` means full source duration).
    pub till_ms: i64,
}

impl VideoTrim {
    /// Creates a new trim range.
    #[must_use]
    pub const fn new(from_ms: i64, till_ms: i64) -> Self {
        Self { from_ms, till_ms }
    }

    /// Clamps this trim range against `full_duration_ms`, matching `VideoClip::setTrim`.
    #[must_use]
    pub fn clamped(self, full_duration_ms: i64) -> Self {
        if full_duration_ms <= 0 {
            return Self {
                from_ms: self.from_ms.max(0),
                till_ms: if self.till_ms > 0 { self.till_ms } else { 0 },
            };
        }
        let from_ms = self.from_ms.clamp(0, full_duration_ms);
        let till_ms = if self.till_ms > 0 {
            self.till_ms.clamp(from_ms, full_duration_ms)
        } else {
            0
        };
        Self { from_ms, till_ms }
    }

    /// Returns the effective inclusive end timestamp in milliseconds for a clip of
    /// `full_duration_ms` (`till > from ? till : full`).
    #[must_use]
    pub fn effective_till_ms(self, full_duration_ms: i64) -> i64 {
        let full = full_duration_ms.max(0);
        let from = self.from_ms.clamp(0, full);
        if self.till_ms > from {
            self.till_ms.min(full)
        } else {
            full
        }
    }

    /// Returns the trimmed playback duration in milliseconds (`VideoClip::loopDuration`).
    #[must_use]
    pub fn duration_ms(self, full_duration_ms: i64) -> i64 {
        if full_duration_ms <= 0 {
            return 0;
        }
        let from = self.from_ms.clamp(0, full_duration_ms);
        let till = self.effective_till_ms(full_duration_ms);
        (till - from).max(0)
    }
}

/// Unique identifier for a clip within a [`MultiClipTimeline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClipId(pub u64);

/// Source descriptor for a [`VideoClip`] (`Editor::VideoClipSource`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoClipSource {
    /// Optional local file path for the media clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Optional raw encoded media bytes (for in-memory / synthetic clips).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<u8>,
    /// Display-oriented frame dimensions of the clip.
    #[serde(default)]
    pub dimensions: Size,
    /// Full untrimmed source duration in milliseconds.
    pub duration_ms: i64,
    /// Source frame rate in frames per second.
    #[serde(default = "default_fps")]
    pub fps: f64,
    /// Whether the source clip contains an audio stream.
    #[serde(default)]
    pub has_audio: bool,
    /// Initial trim range applied when constructing the clip.
    #[serde(default)]
    pub initial_trim: VideoTrim,
    /// Initial audio gain in `0.0..=1.0`.
    #[serde(default = "default_volume")]
    pub initial_volume: f64,
    /// Whether this clip is a circular round video message (`round` in `VideoClipSource`).
    #[serde(default)]
    pub round: bool,
}

const fn default_fps() -> f64 {
    30.0
}

const fn default_volume() -> f64 {
    1.0
}

impl Default for VideoClipSource {
    fn default() -> Self {
        Self {
            path: None,
            content: Vec::new(),
            dimensions: Size::new(0, 0),
            duration_ms: 0,
            fps: default_fps(),
            has_audio: false,
            initial_trim: VideoTrim::default(),
            initial_volume: default_volume(),
            round: false,
        }
    }
}

impl VideoClipSource {
    /// Creates a synthetic/headless clip source with the given duration, size, and audio flag.
    #[must_use]
    pub fn synthetic(duration_ms: i64, dimensions: Size, has_audio: bool) -> Self {
        Self {
            duration_ms: duration_ms.max(0),
            dimensions,
            has_audio,
            ..Self::default()
        }
    }

    /// Creates a clip source backed by a file path on disk.
    #[must_use]
    pub fn from_path(
        path: impl Into<PathBuf>,
        duration_ms: i64,
        dimensions: Size,
        has_audio: bool,
    ) -> Self {
        Self {
            path: Some(path.into()),
            duration_ms: duration_ms.max(0),
            dimensions,
            has_audio,
            ..Self::default()
        }
    }
}

/// Snapshot of a [`VideoClip`]'s editable trim and audio state (`Editor::VideoClip::State`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoClipState {
    /// Active trim range.
    pub trim: VideoTrim,
    /// Audio volume multiplier in `0.0..=1.0`.
    pub volume: f64,
    /// Explicit mute flag.
    #[serde(default)]
    pub muted: bool,
}

impl Default for VideoClipState {
    fn default() -> Self {
        Self {
            trim: VideoTrim::default(),
            volume: 1.0,
            muted: false,
        }
    }
}

/// Single editable clip on a multi-clip timeline (`Editor::VideoClip`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoClip {
    /// Unique identifier within its parent [`MultiClipTimeline`].
    pub id: ClipId,
    /// Underlying media source metadata.
    pub source: VideoClipSource,
    /// Active non-destructive trim range.
    pub trim: VideoTrim,
    /// Audio gain in `0.0..=1.0`.
    pub volume: f64,
    /// Explicit mute toggle (preserves `volume` when unmuted).
    #[serde(default)]
    pub muted: bool,
    /// Whether sound output is currently enabled (`setSoundEnabled`).
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    /// Whether the clip's playback was explicitly stopped (`_stopped`).
    #[serde(default)]
    stopped: bool,
    /// Whether the clip's playback resources were released at least once (`_released`).
    #[serde(default)]
    released: bool,
}

const fn default_true() -> bool {
    true
}

impl VideoClip {
    /// Constructs a new [`VideoClip`], clamping its initial trim and volume from `source`.
    #[must_use]
    pub fn new(id: ClipId, source: VideoClipSource) -> Self {
        let trim = source.initial_trim.clamped(source.duration_ms);
        let volume = clamp_volume(source.initial_volume);
        Self {
            id,
            source,
            trim,
            volume,
            muted: false,
            sound_enabled: true,
            stopped: false,
            released: false,
        }
    }

    /// Unique clip identifier.
    #[must_use]
    pub const fn id(&self) -> ClipId {
        self.id
    }

    /// Underlying clip source metadata.
    #[must_use]
    pub const fn source(&self) -> &VideoClipSource {
        &self.source
    }

    /// Full untrimmed source duration in milliseconds (`VideoClip::duration`).
    #[must_use]
    pub const fn duration_ms(&self) -> i64 {
        self.source.duration_ms
    }

    /// Current trim range (`VideoClip::trim`).
    #[must_use]
    pub const fn trim(&self) -> VideoTrim {
        self.trim
    }

    /// Updates the trim range, clamping it into `[0, duration_ms]` (`VideoClip::setTrim`).
    pub fn set_trim(&mut self, trim: VideoTrim) {
        self.trim = trim.clamped(self.source.duration_ms);
    }

    /// Effective start timestamp inside the source clip in milliseconds.
    #[must_use]
    pub fn effective_from_ms(&self) -> i64 {
        self.trim.from_ms.clamp(0, self.source.duration_ms.max(0))
    }

    /// Effective end timestamp inside the source clip in milliseconds.
    #[must_use]
    pub fn effective_till_ms(&self) -> i64 {
        self.trim.effective_till_ms(self.source.duration_ms)
    }

    /// Active trimmed playback duration in milliseconds (`VideoClip::loopDuration`).
    #[must_use]
    pub fn loop_duration_ms(&self) -> i64 {
        self.trim.duration_ms(self.source.duration_ms)
    }

    /// Whether the source clip has an audio track (`VideoClip::hasAudio`).
    #[must_use]
    pub const fn has_audio(&self) -> bool {
        self.source.has_audio
    }

    /// Configured audio volume in `0.0..=1.0` (`VideoClip::volume`).
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.volume
    }

    /// Clamps and sets the audio volume in `0.0..=1.0` (`VideoClip::setVolume`).
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = clamp_volume(volume);
    }

    /// Returns `true` if this clip is explicitly muted.
    #[must_use]
    pub const fn is_muted(&self) -> bool {
        self.muted
    }

    /// Sets or clears explicit mute without losing the configured volume level.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Returns `true` when this clip produces audible sound (`VideoClip::sounding`).
    #[must_use]
    pub fn sounding(&self) -> bool {
        self.has_audio() && !self.muted && self.sound_enabled && self.volume > 0.0
    }

    /// Effective audio gain (`0.0` when muted, silent, or without audio).
    #[must_use]
    pub fn effective_volume(&self) -> f64 {
        if self.sounding() { self.volume } else { 0.0 }
    }

    /// Enables or disables sound playback for this clip (`VideoClip::setSoundEnabled`).
    pub fn set_sound_enabled(&mut self, enabled: bool) {
        self.sound_enabled = enabled;
    }

    /// Captures the editable clip state (`VideoClip::state`).
    #[must_use]
    pub const fn state(&self) -> VideoClipState {
        VideoClipState {
            trim: self.trim,
            volume: self.volume,
            muted: self.muted,
        }
    }

    /// Restores a previously captured [`VideoClipState`] (`VideoClip::restore`).
    pub fn restore(&mut self, state: VideoClipState) {
        self.set_trim(state.trim);
        self.set_volume(state.volume);
        self.muted = state.muted;
    }

    /// Returns `true` when the clip has a file path or in-memory content (`VideoClip::hasContent`).
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.source
            .path
            .as_ref()
            .is_some_and(|p| !p.as_os_str().is_empty())
            || !self.source.content.is_empty()
    }

    /// Returns `true` if the clip is animated or previously had active playback (`VideoClip::animated`).
    #[must_use]
    pub fn animated(&self) -> bool {
        self.has_content() || self.released
    }

    /// Stops playback for this clip (`VideoClip::stop`).
    pub fn stop(&mut self) {
        if !self.animated() {
            return;
        }
        self.released = true;
        self.stopped = true;
    }

    /// Resumes playback if previously stopped (`VideoClip::resume`).
    pub fn resume(&mut self) -> bool {
        if self.stopped {
            self.stopped = false;
            true
        } else {
            false
        }
    }

    /// Returns `true` if playback is currently stopped.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stopped
    }
}

fn clamp_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Global timeline span occupied by one clip inside a [`MultiClipTimeline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipGlobalSpan {
    /// Zero-based index of the clip in the sequence.
    pub clip_index: usize,
    /// Unique identifier of the clip.
    pub clip_id: ClipId,
    /// Inclusive global start timestamp in milliseconds.
    pub global_start_ms: i64,
    /// Exclusive global end timestamp in milliseconds (inclusive on the final clip).
    pub global_end_ms: i64,
    /// Effective trim start inside the clip's source media (ms).
    pub local_from_ms: i64,
    /// Effective trim end inside the clip's source media (ms).
    pub local_till_ms: i64,
}

impl ClipGlobalSpan {
    /// Duration of this span in milliseconds.
    #[must_use]
    pub const fn duration_ms(self) -> i64 {
        self.global_end_ms - self.global_start_ms
    }
}

/// Resolved playback coordinate mapping a global timeline timestamp to a specific clip and local offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipPosition {
    /// Zero-based index of the active clip.
    pub clip_index: usize,
    /// Unique identifier of the active clip.
    pub clip_id: ClipId,
    /// Local timestamp inside that clip's source media (ms).
    pub local_ms: i64,
    /// Clamped global timestamp on the multi-clip timeline (ms).
    pub global_ms: i64,
}

/// Ordered multi-clip video sequence supporting append, insert, remove, reorder,
/// split at timestamp, per-clip trim & volume/mute, and exact global/local mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiClipTimeline {
    clips: Vec<VideoClip>,
    next_clip_id: u64,
}

impl Default for MultiClipTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiClipTimeline {
    /// Creates an empty multi-clip timeline.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            clips: Vec::new(),
            next_clip_id: 1,
        }
    }

    /// Creates a timeline initialized with `sources` in order.
    #[must_use]
    pub fn from_sources(sources: impl IntoIterator<Item = VideoClipSource>) -> Self {
        let mut timeline = Self::new();
        for source in sources {
            timeline.append(source);
        }
        timeline
    }

    /// Returns all clips in playback order.
    #[must_use]
    pub fn clips(&self) -> &[VideoClip] {
        &self.clips
    }

    /// Number of clips in the sequence.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    /// Returns `true` if there are no clips in the sequence.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// Returns a reference to the clip at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&VideoClip> {
        self.clips.get(index)
    }

    /// Returns a mutable reference to the clip at `index`.
    #[must_use]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut VideoClip> {
        self.clips.get_mut(index)
    }

    /// Looks up a clip and its current index by [`ClipId`].
    #[must_use]
    pub fn find_by_id(&self, id: ClipId) -> Option<(usize, &VideoClip)> {
        self.clips.iter().enumerate().find(|(_, c)| c.id == id)
    }

    fn alloc_id(&mut self) -> ClipId {
        let id = ClipId(self.next_clip_id);
        self.next_clip_id = self.next_clip_id.saturating_add(1);
        id
    }

    /// Appends a new clip to the end of the timeline and returns its [`ClipId`].
    pub fn append(&mut self, source: VideoClipSource) -> ClipId {
        let id = self.alloc_id();
        self.clips.push(VideoClip::new(id, source));
        id
    }

    /// Inserts a new clip at `index` (`0..=len()`), returning its [`ClipId`].
    pub fn insert(&mut self, index: usize, source: VideoClipSource) -> Result<ClipId, EditorError> {
        if index > self.clips.len() {
            return Err(EditorError::InvalidTimeline(format!(
                "insert index {index} out of bounds for {} clips",
                self.clips.len()
            )));
        }
        let id = self.alloc_id();
        self.clips.insert(index, VideoClip::new(id, source));
        Ok(id)
    }

    /// Removes and returns the clip at `index`.
    pub fn remove(&mut self, index: usize) -> Result<VideoClip, EditorError> {
        if index >= self.clips.len() {
            return Err(EditorError::InvalidTimeline(format!(
                "remove index {index} out of bounds for {} clips",
                self.clips.len()
            )));
        }
        Ok(self.clips.remove(index))
    }

    /// Moves the clip at `from_index` to `to_index`, preserving the relative order of other clips.
    pub fn reorder(&mut self, from_index: usize, to_index: usize) -> Result<(), EditorError> {
        let len = self.clips.len();
        if from_index >= len || to_index >= len {
            return Err(EditorError::InvalidTimeline(format!(
                "reorder indices ({from_index} -> {to_index}) out of bounds for {len} clips"
            )));
        }
        if from_index == to_index {
            return Ok(());
        }
        let clip = self.clips.remove(from_index);
        self.clips.insert(to_index, clip);
        Ok(())
    }

    /// Updates the trim range of the clip at `index`.
    ///
    /// Rejects out-of-bounds indices, negative `from_ms`, or inverted/zero-length ranges
    /// where `till_ms > 0 && till_ms <= from_ms` or the clamped duration would be `<= 0`.
    pub fn set_clip_trim(&mut self, index: usize, trim: VideoTrim) -> Result<(), EditorError> {
        let len = self.clips.len();
        let clip = self.clips.get_mut(index).ok_or_else(|| {
            EditorError::InvalidTimeline(format!(
                "clip index {index} out of bounds for {len} clips"
            ))
        })?;
        if trim.from_ms < 0 {
            return Err(EditorError::InvalidTimeline(format!(
                "negative trim start {}ms",
                trim.from_ms
            )));
        }
        if trim.till_ms < 0 {
            return Err(EditorError::InvalidTimeline(format!(
                "negative trim end {}ms",
                trim.till_ms
            )));
        }
        if trim.till_ms > 0 && trim.till_ms <= trim.from_ms {
            return Err(EditorError::InvalidTimeline(format!(
                "invalid trim range [{}, {}]: start must be less than end",
                trim.from_ms, trim.till_ms
            )));
        }
        let clamped = trim.clamped(clip.duration_ms());
        if clip.duration_ms() > 0 && clamped.duration_ms(clip.duration_ms()) <= 0 {
            return Err(EditorError::InvalidTimeline(format!(
                "trim range [{}, {}] yields zero duration for {}ms clip",
                trim.from_ms,
                trim.till_ms,
                clip.duration_ms()
            )));
        }
        clip.trim = clamped;
        Ok(())
    }

    /// Sets the volume (`0.0..=1.0`) of the clip at `index`.
    pub fn set_clip_volume(&mut self, index: usize, volume: f64) -> Result<(), EditorError> {
        let len = self.clips.len();
        let clip = self.clips.get_mut(index).ok_or_else(|| {
            EditorError::InvalidTimeline(format!(
                "clip index {index} out of bounds for {len} clips"
            ))
        })?;
        clip.set_volume(volume);
        Ok(())
    }

    /// Sets or clears explicit mute on the clip at `index`.
    pub fn set_clip_muted(&mut self, index: usize, muted: bool) -> Result<(), EditorError> {
        let len = self.clips.len();
        let clip = self.clips.get_mut(index).ok_or_else(|| {
            EditorError::InvalidTimeline(format!(
                "clip index {index} out of bounds for {len} clips"
            ))
        })?;
        clip.set_muted(muted);
        Ok(())
    }

    /// Splits the clip covering `global_ms` into two contiguous clips at that exact timestamp.
    ///
    /// Preserves source metadata, volume, mute, and sound-enabled flags on both halves while
    /// keeping the total timeline duration unchanged. Returns `(left_id, right_id)`.
    ///
    /// Returns an error if the timeline is empty, `global_ms` is outside the timeline, or
    /// `global_ms` lands directly on a clip boundary (which would create a zero-length clip).
    pub fn split_at_global_ms(&mut self, global_ms: i64) -> Result<(ClipId, ClipId), EditorError> {
        let total = self.total_duration_ms();
        if self.clips.is_empty() || total <= 0 {
            return Err(EditorError::InvalidTimeline(
                "cannot split an empty timeline".to_owned(),
            ));
        }
        if global_ms <= 0 || global_ms >= total {
            return Err(EditorError::InvalidTimeline(format!(
                "split timestamp {global_ms}ms must be strictly inside (0, {total})"
            )));
        }
        let spans = self.spans();
        let Some(span) = spans
            .iter()
            .find(|s| global_ms > s.global_start_ms && global_ms < s.global_end_ms)
        else {
            return Err(EditorError::InvalidTimeline(format!(
                "split timestamp {global_ms}ms falls on an existing clip boundary"
            )));
        };

        let idx = span.clip_index;
        let offset_in_clip = global_ms - span.global_start_ms;
        let split_local_ms = span.local_from_ms + offset_in_clip;
        let orig_from = span.local_from_ms;
        let orig_till = span.local_till_ms;

        let right_id = self.alloc_id();
        let left_clip = &mut self.clips[idx];
        let left_id = left_clip.id;

        let mut right_clip = left_clip.clone();
        right_clip.id = right_id;

        left_clip.set_trim(VideoTrim::new(orig_from, split_local_ms));
        right_clip.set_trim(VideoTrim::new(split_local_ms, orig_till));

        self.clips.insert(idx + 1, right_clip);
        Ok((left_id, right_id))
    }

    /// Total active duration of all clips combined in milliseconds.
    #[must_use]
    pub fn total_duration_ms(&self) -> i64 {
        self.clips
            .iter()
            .fold(0i64, |acc, c| acc.saturating_add(c.loop_duration_ms()))
    }

    /// Computes monotonic contiguous global spans for every clip in playback order.
    #[must_use]
    pub fn spans(&self) -> Vec<ClipGlobalSpan> {
        let mut out = Vec::with_capacity(self.clips.len());
        let mut cursor = 0i64;
        for (clip_index, clip) in self.clips.iter().enumerate() {
            let local_from_ms = clip.effective_from_ms();
            let local_till_ms = clip.effective_till_ms();
            let dur = (local_till_ms - local_from_ms).max(0);
            let global_end_ms = cursor.saturating_add(dur);
            out.push(ClipGlobalSpan {
                clip_index,
                clip_id: clip.id,
                global_start_ms: cursor,
                global_end_ms,
                local_from_ms,
                local_till_ms,
            });
            cursor = global_end_ms;
        }
        out
    }

    /// Resolves a global timeline timestamp to `(clip_index, local_ms)`, clamping out-of-range
    /// values into `[0, total_duration_ms()]`.
    ///
    /// Boundary convention:
    /// - Interior boundary `spans[i].global_end_ms == spans[i + 1].global_start_ms` resolves to
    ///   the start of clip `i + 1` (`local_from_ms`).
    /// - `global_ms >= total_duration_ms()` resolves to the end of the last non-empty clip
    ///   (`local_till_ms`).
    /// - Returns `None` only when the timeline has no clips.
    #[must_use]
    pub fn resolve_global_ms(&self, global_ms: i64) -> Option<ClipPosition> {
        if self.clips.is_empty() {
            return None;
        }
        let total = self.total_duration_ms();
        let clamped = global_ms.clamp(0, total);
        let spans = self.spans();

        // First look for a non-empty span strictly containing `clamped` in `[start, end)`.
        for span in &spans {
            if span.duration_ms() > 0
                && clamped >= span.global_start_ms
                && clamped < span.global_end_ms
            {
                let local_ms = span.local_from_ms + (clamped - span.global_start_ms);
                return Some(ClipPosition {
                    clip_index: span.clip_index,
                    clip_id: span.clip_id,
                    local_ms,
                    global_ms: clamped,
                });
            }
        }

        // At exact end of timeline (`clamped == total`): map to the end of the last non-empty clip,
        // or the first clip if all clips have 0 duration.
        let last = spans
            .iter()
            .rev()
            .find(|s| s.duration_ms() > 0)
            .or_else(|| spans.first())?;
        Some(ClipPosition {
            clip_index: last.clip_index,
            clip_id: last.clip_id,
            local_ms: last.local_till_ms,
            global_ms: clamped,
        })
    }

    /// Maps `(clip_index, local_ms)` back to a global timeline timestamp in milliseconds.
    ///
    /// `local_ms` is clamped to the clip's active `[effective_from_ms, effective_till_ms]` window.
    /// Returns `None` if `clip_index` is out of bounds.
    #[must_use]
    pub fn global_ms_for(&self, clip_index: usize, local_ms: i64) -> Option<i64> {
        let span = self.spans().into_iter().nth(clip_index)?;
        let clamped_local = local_ms.clamp(span.local_from_ms, span.local_till_ms);
        Some(span.global_start_ms + (clamped_local - span.local_from_ms))
    }

    /// Validates that all clip invariants hold (non-negative durations, valid trims).
    pub fn validate(&self) -> Result<(), EditorError> {
        for (idx, clip) in self.clips.iter().enumerate() {
            if clip.duration_ms() < 0 {
                return Err(EditorError::InvalidTimeline(format!(
                    "clip {idx} has negative duration {}ms",
                    clip.duration_ms()
                )));
            }
            if clip.trim.from_ms < 0
                || clip.trim.till_ms < 0
                || (clip.trim.till_ms > 0 && clip.trim.till_ms < clip.trim.from_ms)
            {
                return Err(EditorError::InvalidTimeline(format!(
                    "clip {idx} has invalid trim [{}, {}]",
                    clip.trim.from_ms, clip.trim.till_ms
                )));
            }
        }
        Ok(())
    }
}
