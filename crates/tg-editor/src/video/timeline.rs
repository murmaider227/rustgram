//! UI-independent trim timeline model, thumbnail frame cache, cover-frame selection,
//! per-overlay item visibility ranges, and timestamp formatting/parsing.
//!
//! Port of `Telegram/SourceFiles/editor/editor_trim_timeline.{h,cpp}`,
//! `Telegram/SourceFiles/editor/video/video_timeline.{h,cpp}`, and
//! `Telegram/SourceFiles/editor/video_item_timeline.{h,cpp}`.

use std::collections::BTreeMap;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::EditorError;
use crate::geometry::Size;
use crate::scene::{ItemId, PaintScene, SceneEntry};
use crate::video::clip::{VideoClip, VideoTrim};
use crate::video::segment_player::{SegmentPlayer, SegmentPlayerOptions, TimelineSeeker};

/// Minimum trim selection length in milliseconds (`kMinSelection = 100`).
pub const MIN_SELECTION_MS: i64 = 100;

/// Minimum visible timeline span when zoomed in (`kMinVisibleSpan = 2000`).
pub const MIN_VISIBLE_SPAN_MS: i64 = 2_000;

/// Maximum thumbnail frames displayed along the timeline strip (`kMaxFrames = 24`).
pub const MAX_TIMELINE_FRAMES: usize = 24;

/// Maximum cached thumbnail frame sets (`kMaxCachedFrameSets = 8`).
pub const MAX_CACHED_FRAME_SETS: usize = 8;

/// Acceptable relative frame-width drift when reusing cached thumbnails (`kFrameWidthTolerance`).
pub const FRAME_WIDTH_TOLERANCE: f64 = 0.25;

/// Decimal precision for editable timestamps (`kStampPrecision = 3`).
pub const STAMP_PRECISION: usize = 3;

/// Descriptor used to initialize [`TrimTimelineModel`] (`Editor::TrimTimelineDescriptor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimTimelineDescriptor {
    /// Total clip duration in milliseconds.
    pub duration_ms: i64,
    /// Optional maximum allowed selection duration (`0` = full clip).
    pub max_duration_ms: i64,
    /// Optional minimum allowed selection duration (`0` = `MIN_SELECTION_MS`).
    pub min_duration_ms: i64,
    /// Initial trim start in milliseconds.
    pub from_ms: i64,
    /// Initial trim end in milliseconds (`0` = full allowed window).
    pub till_ms: i64,
    /// Initial cover-frame timestamp in milliseconds.
    pub cover_ms: i64,
    /// If `true`, hides cover-frame selection (trim-only mode).
    pub trim_only: bool,
}

/// UI-independent trim & cover timeline state machine (`Editor::TrimTimeline`).
#[derive(Debug, Clone, PartialEq)]
pub struct TrimTimelineModel {
    duration_ms: i64,
    max_duration_ms: i64,
    min_duration_ms: i64,
    trim_only: bool,
    from_ms: i64,
    till_ms: i64,
    cover_ms: i64,
    playback_ms: Option<i64>,
    zoom: f64,
    visible_from_ms: f64,
    overlay_visibility: OverlayVisibilityTrack,
}

impl TrimTimelineModel {
    /// Creates and clamps a new timeline model from `descriptor`.
    #[must_use]
    pub fn new(descriptor: TrimTimelineDescriptor) -> Self {
        let duration_ms = descriptor.duration_ms.max(1);
        let max_duration_ms = if descriptor.max_duration_ms > 0 {
            descriptor.max_duration_ms.min(duration_ms)
        } else {
            duration_ms
        };
        let min_duration_ms = descriptor.min_duration_ms.max(0).min(max_duration_ms);
        let from_ms = descriptor
            .from_ms
            .clamp(0, (duration_ms - min_duration_ms).max(0));
        let limit = (from_ms + max_duration_ms).min(duration_ms);
        let till_ms = if descriptor.till_ms > from_ms {
            descriptor.till_ms.min(limit)
        } else {
            limit
        };
        let cover_ms = descriptor.cover_ms.clamp(from_ms, till_ms);
        Self {
            duration_ms,
            max_duration_ms,
            min_duration_ms,
            trim_only: descriptor.trim_only,
            from_ms,
            till_ms,
            cover_ms,
            playback_ms: None,
            zoom: 1.0,
            visible_from_ms: 0.0,
            overlay_visibility: OverlayVisibilityTrack::new(),
        }
    }

    /// Borrows the per-overlay scene item visibility track attached to this timeline.
    #[must_use]
    pub const fn overlay_visibility(&self) -> &OverlayVisibilityTrack {
        &self.overlay_visibility
    }

    /// Mutably borrows the per-overlay scene item visibility track attached to this timeline.
    pub fn overlay_visibility_mut(&mut self) -> &mut OverlayVisibilityTrack {
        &mut self.overlay_visibility
    }

    /// Sets a validated `[start_ms, end_ms)` visibility window for `item_id`, rejecting
    /// invalid `start_ms >= end_ms` or negative timestamps.
    pub fn set_item_visibility(
        &mut self,
        item_id: ItemId,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<ItemVisibilityRange, EditorError> {
        self.overlay_visibility.set_range(item_id, start_ms, end_ms)
    }

    /// Returns the IDs of all active scene items visible at `time_ms` in ascending z-order.
    #[must_use]
    pub fn active_overlay_items_at(&self, scene: &PaintScene, time_ms: i64) -> Vec<ItemId> {
        self.overlay_visibility.visible_item_ids_at(scene, time_ms)
    }

    /// Total duration of the clip in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    /// Maximum allowed trim span in milliseconds.
    #[must_use]
    pub const fn max_duration_ms(&self) -> i64 {
        self.max_duration_ms
    }

    /// Effective minimum selection span in milliseconds (`TrimTimeline::minSelection`).
    #[must_use]
    pub fn min_selection_ms(&self) -> i64 {
        let base = if self.min_duration_ms > 0 {
            self.min_duration_ms
        } else {
            MIN_SELECTION_MS
        };
        base.min(self.max_duration_ms).min(self.duration_ms).max(1)
    }

    /// Whether the timeline is in trim-only mode (no cover-frame marker).
    #[must_use]
    pub const fn trim_only(&self) -> bool {
        self.trim_only
    }

    /// Current trim start in milliseconds.
    #[must_use]
    pub const fn from_ms(&self) -> i64 {
        self.from_ms
    }

    /// Current trim end in milliseconds.
    #[must_use]
    pub const fn till_ms(&self) -> i64 {
        self.till_ms
    }

    /// Selected trim duration (`till_ms - from_ms`).
    #[must_use]
    pub const fn selected_duration_ms(&self) -> i64 {
        self.till_ms - self.from_ms
    }

    /// Selected cover-frame timestamp in milliseconds (always within `[from_ms, till_ms]`).
    #[must_use]
    pub const fn cover_ms(&self) -> i64 {
        self.cover_ms
    }

    /// Current preview playback position, if started.
    #[must_use]
    pub const fn playback_ms(&self) -> Option<i64> {
        self.playback_ms
    }

    /// Current zoom factor (`>= 1.0`).
    #[must_use]
    pub const fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Visible timeline span in milliseconds.
    #[must_use]
    pub fn visible_span_ms(&self) -> f64 {
        (self.duration_ms as f64) / self.zoom.max(1.0)
    }

    /// Maximum supported zoom factor (`TrimTimeline::maxZoom`).
    #[must_use]
    pub fn max_zoom(&self) -> f64 {
        let span = MIN_VISIBLE_SPAN_MS.min(self.duration_ms).max(1) as f64;
        ((self.duration_ms as f64) / span).max(1.0)
    }

    /// Start of the visible timeline viewport in milliseconds.
    #[must_use]
    pub fn visible_from_ms(&self) -> i64 {
        self.visible_from_ms.round() as i64
    }

    /// End of the visible timeline viewport in milliseconds.
    #[must_use]
    pub fn visible_till_ms(&self) -> i64 {
        (self.visible_from_ms + self.visible_span_ms())
            .round()
            .min(self.duration_ms as f64) as i64
    }

    /// Updates both trim endpoints, enforcing `min_selection_ms`, `max_duration_ms`,
    /// and re-clamping `cover_ms` into `[from_ms, till_ms]`.
    pub fn set_trim(&mut self, from_ms: i64, till_ms: i64) {
        let min_sel = self.min_selection_ms();
        let mut f = from_ms.clamp(0, (self.duration_ms - min_sel).max(0));
        let mut t = till_ms.clamp(f + min_sel, self.duration_ms);
        if t - f > self.max_duration_ms {
            t = (f + self.max_duration_ms).min(self.duration_ms);
            if t - f < min_sel {
                f = (t - min_sel).max(0);
            }
        }
        self.from_ms = f;
        self.till_ms = t;
        self.cover_ms = self.cover_ms.clamp(self.from_ms, self.till_ms);
    }

    /// Adjusts only the left trim handle (`from_ms`), keeping `[from, till]` valid.
    pub fn set_from(&mut self, from_ms: i64) {
        let min_sel = self.min_selection_ms();
        let min_from = (self.till_ms - self.max_duration_ms).max(0);
        let max_from = (self.till_ms - min_sel).max(0);
        self.from_ms = from_ms.clamp(min_from, max_from);
        self.cover_ms = self.cover_ms.clamp(self.from_ms, self.till_ms);
    }

    /// Adjusts only the right trim handle (`till_ms`), keeping `[from, till]` valid.
    pub fn set_till(&mut self, till_ms: i64) {
        let min_sel = self.min_selection_ms();
        let min_till = (self.from_ms + min_sel).min(self.duration_ms);
        let max_till = (self.from_ms + self.max_duration_ms).min(self.duration_ms);
        self.till_ms = till_ms.clamp(min_till, max_till);
        self.cover_ms = self.cover_ms.clamp(self.from_ms, self.till_ms);
    }

    /// Slides the fixed-width trim window so its midpoint sits at `center_ms`
    /// (`TrimTimeline::moveWindowTo`).
    pub fn move_window_to(&mut self, center_ms: i64) {
        let span = self.till_ms - self.from_ms;
        let half = span / 2;
        let new_from = (center_ms - half).clamp(0, (self.duration_ms - span).max(0));
        self.from_ms = new_from;
        self.till_ms = new_from + span;
        self.cover_ms = self.cover_ms.clamp(self.from_ms, self.till_ms);
    }

    /// Selects a cover frame timestamp, clamped into `[from_ms, till_ms]`.
    pub fn set_cover(&mut self, cover_ms: i64) {
        self.cover_ms = cover_ms.clamp(self.from_ms, self.till_ms);
    }

    /// Sets the current playback position, clamped into `[from_ms, till_ms]`.
    pub fn set_playback_position(&mut self, position_ms: i64) {
        self.playback_ms = Some(position_ms.clamp(self.from_ms, self.till_ms));
    }

    /// Sets the visible zoom and viewport start (`TrimTimeline::setVisibleRange`).
    pub fn set_visible_range(&mut self, zoom: f64, from_ms: f64) -> bool {
        let max_z = self.max_zoom();
        let new_zoom = if zoom.is_finite() {
            zoom.clamp(1.0, max_z)
        } else {
            1.0
        };
        let span = (self.duration_ms as f64) / new_zoom;
        let max_from = ((self.duration_ms as f64) - span).max(0.0);
        let new_from = if from_ms.is_finite() {
            from_ms.clamp(0.0, max_from)
        } else {
            0.0
        };
        let changed =
            (self.zoom - new_zoom).abs() > 1e-6 || (self.visible_from_ms - new_from).abs() > 1e-3;
        self.zoom = new_zoom;
        self.visible_from_ms = new_from;
        changed
    }

    /// Maps a normalized strip fraction `0.0..=1.0` to a timestamp in milliseconds.
    #[must_use]
    pub fn time_at_fraction(&self, frac: f64) -> i64 {
        let clamped = frac.clamp(0.0, 1.0);
        (self.visible_from_ms + clamped * self.visible_span_ms())
            .round()
            .clamp(0.0, self.duration_ms as f64) as i64
    }

    /// Maps a timestamp in milliseconds to a normalized fraction in the visible strip.
    #[must_use]
    pub fn fraction_at_time(&self, time_ms: i64) -> f64 {
        let span = self.visible_span_ms().max(1.0);
        ((time_ms as f64) - self.visible_from_ms) / span
    }

    /// Computes thumbnail sample timestamps along the visible strip (`VideoTimeline::reloadFrames`).
    #[must_use]
    pub fn thumbnail_sample_positions(
        &self,
        strip_width: i32,
        strip_height: i32,
        video_size: Size,
    ) -> (Vec<i64>, Size) {
        if strip_width <= 0 || strip_height <= 0 || video_size.is_empty() {
            return (Vec::new(), Size::new(0, 0));
        }
        let aspect_w = ((f64::from(strip_height) * f64::from(video_size.width))
            / f64::from(video_size.height))
        .round()
        .max(1.0) as i32;
        let raw_count =
            ((strip_width + aspect_w - 1) / aspect_w).clamp(1, MAX_TIMELINE_FRAMES as i32);
        let count = raw_count as usize;
        let frame_w = ((strip_width + raw_count - 1) / raw_count).max(1);
        let from = self.visible_from_ms();
        let span = (self.visible_till_ms() - from).max(1);

        let mut positions = Vec::with_capacity(count);
        for i in 0..count {
            let offset = (((i as f64) + 0.5) * (span as f64) / (count as f64)).round() as i64;
            positions.push((from + offset).clamp(0, self.duration_ms));
        }
        (positions, Size::new(frame_w, strip_height))
    }

    /// Formats the duration or trim-range label (`TrimTimeline::durationLabel`).
    #[must_use]
    pub fn duration_label_text(&self) -> String {
        if self.till_ms - self.from_ms >= self.duration_ms {
            format_stamp(self.till_ms - self.from_ms, 1)
        } else {
            format!(
                "{} – {}",
                format_stamp(self.from_ms, 1),
                format_stamp(self.till_ms, 1)
            )
        }
    }

    /// Parses user-edited range text (e.g. `"0:01.5 - 0:04.0"` or `"0:03.2"`) and applies trim
    /// (`TrimTimeline::applyDurationText`). Returns `true` if parsed and applied.
    pub fn apply_duration_text(&mut self, text: &str) -> bool {
        let parts = split_stamps(text);
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        let Some(till) = parse_stamp(parts[parts.len() - 1]) else {
            return false;
        };
        let Some(from) = (if parts.len() > 1 {
            parse_stamp(parts[0])
        } else {
            Some(self.from_ms)
        }) else {
            return false;
        };
        self.set_trim(from.min(till), from.max(till));
        true
    }
}

/// Formats milliseconds as `M:SS.d` (or `H:MM:SS.d`), matching `Stamp()` in `editor_trim_timeline.cpp`.
#[must_use]
pub fn format_stamp(value_ms: i64, precision: usize) -> String {
    let clamped = value_ms.max(0);
    let total_secs = clamped / 1000;
    let ms = clamped % 1000;
    let prec = precision.clamp(1, STAMP_PRECISION);
    let frac_full = format!("{ms:03}");
    let frac = &frac_full[..prec];
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}.{frac}")
    } else {
        format!("{mins}:{secs:02}.{frac}")
    }
}

/// Parses a `[[HH:]MM:]SS[.mmm]` timestamp string into milliseconds (`ParseStamp`).
#[must_use]
pub fn parse_stamp(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut seconds: i64 = 0;
    for part in &parts[..parts.len() - 1] {
        let val: i64 = part.parse().ok()?;
        if val < 0 {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(val)?;
    }
    let last = parts[parts.len() - 1];
    let sep_idx = last.find(['.', ',']);
    let (whole_str, frac_str) = match sep_idx {
        Some(idx) => (&last[..idx], &last[idx + 1..]),
        None => (last, ""),
    };
    if whole_str.is_empty() && frac_str.is_empty() {
        return None;
    }
    let whole: i64 = if whole_str.is_empty() {
        0
    } else {
        whole_str.parse().ok()?
    };
    if whole < 0 {
        return None;
    }
    let digits_len = frac_str.len().min(STAMP_PRECISION);
    // `get`: the field is free text, so the cut can fall inside a multibyte character.
    let digits = frac_str.get(..digits_len)?;
    let mut ms: i64 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    if ms < 0 {
        return None;
    }
    for _ in digits_len..STAMP_PRECISION {
        ms *= 10;
    }
    let total_secs = seconds.checked_mul(60)?.checked_add(whole)?;
    total_secs.checked_mul(1000)?.checked_add(ms)
}

fn is_range_dash(ch: char) -> bool {
    matches!(ch, '-' | '\u{2013}' | '\u{2014}')
}

fn split_stamps(text: &str) -> Vec<&str> {
    text.split(|ch: char| ch.is_whitespace() || is_range_dash(ch))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Cached strip of extracted video thumbnail frames (`Editor::VideoTimelineFrames`).
#[derive(Debug, Clone)]
pub struct VideoTimelineFrames {
    /// Decoded thumbnail images ordered from left to right.
    pub frames: Vec<RgbaImage>,
    /// Start timestamp of the strip in milliseconds.
    pub from_ms: i64,
    /// Span covered by the strip in milliseconds.
    pub span_ms: i64,
    /// Natural box dimensions of each thumbnail slot.
    pub box_size: Size,
}

impl VideoTimelineFrames {
    /// Returns `true` if this cached strip matches the requested viewport and slot geometry
    /// within [`FRAME_WIDTH_TOLERANCE`].
    #[must_use]
    pub fn matches(&self, count: usize, from_ms: i64, span_ms: i64, box_size: Size) -> bool {
        self.frames.len() == count
            && self.from_ms == from_ms
            && self.span_ms == span_ms
            && self.box_size.height == box_size.height
            && (f64::from((box_size.width - self.box_size.width).abs())
                <= f64::from(self.box_size.width) * FRAME_WIDTH_TOLERANCE)
    }
}

/// Bounded LRU-style cache of extracted thumbnail strips (`Editor::VideoTimelineFramesCache`).
#[derive(Debug, Default, Clone)]
pub struct VideoTimelineFramesCache {
    sets: Vec<VideoTimelineFrames>,
}

impl VideoTimelineFramesCache {
    /// Creates an empty thumbnail cache.
    #[must_use]
    pub const fn new() -> Self {
        Self { sets: Vec::new() }
    }

    /// Finds the most recently added strip matching `(count, from_ms, span_ms, box_size)`.
    #[must_use]
    pub fn find(
        &self,
        count: usize,
        from_ms: i64,
        span_ms: i64,
        box_size: Size,
    ) -> Option<&VideoTimelineFrames> {
        self.sets
            .iter()
            .rev()
            .find(|s| s.matches(count, from_ms, span_ms, box_size))
    }

    /// Inserts a strip into the cache, evicting the oldest if exceeding [`MAX_CACHED_FRAME_SETS`].
    pub fn add(&mut self, set: VideoTimelineFrames) {
        self.sets.push(set);
        if self.sets.len() > MAX_CACHED_FRAME_SETS {
            self.sets.remove(0);
        }
    }

    /// Number of currently cached thumbnail strips.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Returns `true` when no strips are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Per-overlay scene item visibility ranges & VideoItemTimeline model
// ---------------------------------------------------------------------------

/// Half-open `[start_ms, end_ms)` visibility window for a scene overlay item on a video timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemVisibilityRange {
    /// Inclusive appearance timestamp in milliseconds (`>= 0`).
    pub start_ms: i64,
    /// Exclusive disappearance timestamp in milliseconds (`> start_ms`).
    pub end_ms: i64,
}

impl ItemVisibilityRange {
    /// Creates a validated visibility range, rejecting negative timestamps or `start_ms >= end_ms`.
    pub fn new(start_ms: i64, end_ms: i64) -> Result<Self, EditorError> {
        if start_ms < 0 || start_ms >= end_ms {
            return Err(EditorError::InvalidVisibilityRange { start_ms, end_ms });
        }
        Ok(Self { start_ms, end_ms })
    }

    /// Duration of this visibility window in milliseconds (`end_ms - start_ms`).
    #[must_use]
    pub const fn duration_ms(self) -> i64 {
        self.end_ms - self.start_ms
    }

    /// Returns `true` when `time_ms` falls within `[start_ms, end_ms)`.
    #[must_use]
    pub const fn contains(self, time_ms: i64) -> bool {
        time_ms >= self.start_ms && time_ms < self.end_ms
    }

    /// Clamps this window into `[0, max_duration_ms]`, returning `None` if the clamped window
    /// collapses to empty (`start_ms >= end_ms`).
    #[must_use]
    pub fn clamped_to(self, max_duration_ms: i64) -> Option<Self> {
        if max_duration_ms <= 0 {
            return None;
        }
        let start_ms = self.start_ms.clamp(0, max_duration_ms);
        let end_ms = self.end_ms.clamp(0, max_duration_ms);
        if start_ms < end_ms {
            Some(Self { start_ms, end_ms })
        } else {
            None
        }
    }
}

/// Per-overlay visibility schedule associating scene [`ItemId`]s with [`ItemVisibilityRange`]s.
///
/// Scene items without an explicit entry are treated as visible across the full video timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayVisibilityTrack {
    ranges: BTreeMap<ItemId, ItemVisibilityRange>,
}

impl OverlayVisibilityTrack {
    /// Creates an empty visibility track (all items visible for the full timeline).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ranges: BTreeMap::new(),
        }
    }

    /// Registers or replaces the visibility window `[start_ms, end_ms)` for `item_id`.
    ///
    /// Returns [`EditorError::InvalidVisibilityRange`] if `start_ms < 0` or `start_ms >= end_ms`.
    pub fn set_range(
        &mut self,
        item_id: ItemId,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<ItemVisibilityRange, EditorError> {
        let range = ItemVisibilityRange::new(start_ms, end_ms)?;
        self.ranges.insert(item_id, range);
        Ok(range)
    }

    /// Registers a visibility window clamped to `[0, timeline_duration_ms]`.
    ///
    /// Rejects invalid `start_ms < 0`, `start_ms >= end_ms`, or windows that collapse outside
    /// `timeline_duration_ms`.
    pub fn set_range_clamped(
        &mut self,
        item_id: ItemId,
        start_ms: i64,
        end_ms: i64,
        timeline_duration_ms: i64,
    ) -> Result<ItemVisibilityRange, EditorError> {
        let raw = ItemVisibilityRange::new(start_ms, end_ms)?;
        let clamped = raw
            .clamped_to(timeline_duration_ms)
            .ok_or(EditorError::InvalidVisibilityRange { start_ms, end_ms })?;
        self.ranges.insert(item_id, clamped);
        Ok(clamped)
    }

    /// Clears the explicit visibility window for `item_id` (making it visible for the full video).
    pub fn remove_range(&mut self, item_id: ItemId) -> Option<ItemVisibilityRange> {
        self.ranges.remove(&item_id)
    }

    /// Looks up the explicit visibility window for `item_id`, if any.
    #[must_use]
    pub fn range_for(&self, item_id: ItemId) -> Option<ItemVisibilityRange> {
        self.ranges.get(&item_id).copied()
    }

    /// Returns all registered visibility windows keyed by [`ItemId`].
    #[must_use]
    pub const fn entries(&self) -> &BTreeMap<ItemId, ItemVisibilityRange> {
        &self.ranges
    }

    /// Number of items with explicit visibility windows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Returns `true` if no explicit visibility windows are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Returns `true` if `item_id` is visible at `time_ms`.
    ///
    /// Items without a registered window are always visible (`true`).
    #[must_use]
    pub fn is_item_visible_at(&self, item_id: ItemId, time_ms: i64) -> bool {
        match self.ranges.get(&item_id) {
            Some(range) => range.contains(time_ms),
            None => true,
        }
    }

    /// Returns active drawable [`SceneEntry`] references from `scene` visible at `time_ms`
    /// in ascending z-order.
    #[must_use]
    pub fn visible_entries_at<'a>(
        &self,
        scene: &'a PaintScene,
        time_ms: i64,
    ) -> Vec<&'a SceneEntry> {
        scene
            .active_items_by_z()
            .into_iter()
            .filter(|entry| self.is_item_visible_at(entry.id, time_ms))
            .collect()
    }

    /// Returns the [`ItemId`]s of all active drawable items in `scene` visible at `time_ms`
    /// in ascending z-order.
    #[must_use]
    pub fn visible_item_ids_at(&self, scene: &PaintScene, time_ms: i64) -> Vec<ItemId> {
        self.visible_entries_at(scene, time_ms)
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    }

    /// Drops visibility windows whose `ItemId` is no longer an active item in `scene`.
    pub fn prune_missing(&mut self, scene: &PaintScene) {
        let active: Vec<ItemId> = scene
            .active_items_by_z()
            .into_iter()
            .map(|e| e.id)
            .collect();
        self.ranges.retain(|id, _| active.contains(id));
    }

    /// Validates that every stored window satisfies `0 <= start_ms < end_ms`.
    pub fn validate(&self) -> Result<(), EditorError> {
        for range in self.ranges.values() {
            let _ = ItemVisibilityRange::new(range.start_ms, range.end_ms)?;
        }
        Ok(())
    }
}

/// Headless controller for trimming an embedded video overlay item and syncing its
/// [`SegmentPlayer`] + [`TimelineSeeker`] (`Editor::VideoItemTimeline`).
#[derive(Debug, Clone)]
pub struct VideoItemTimelineModel {
    clip: Option<VideoClip>,
    timeline: Option<TrimTimelineModel>,
    player: Option<SegmentPlayer>,
    seeker: TimelineSeeker,
    frames_cache: VideoTimelineFramesCache,
    length_changes: Vec<i64>,
}

impl Default for VideoItemTimelineModel {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoItemTimelineModel {
    /// Creates an empty video item timeline controller.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            clip: None,
            timeline: None,
            player: None,
            seeker: TimelineSeeker::new(),
            frames_cache: VideoTimelineFramesCache::new(),
            length_changes: Vec::new(),
        }
    }

    /// Binds a [`VideoClip`] to this item timeline, constructing a trim-only
    /// [`TrimTimelineModel`] and [`SegmentPlayer`] (`VideoItemTimeline::setClip`).
    pub fn set_clip(&mut self, clip: Option<VideoClip>) {
        self.seeker = TimelineSeeker::new();
        self.timeline = None;
        self.player = None;
        self.clip = clip;
        let Some(ref mut c) = self.clip else {
            return;
        };
        let trim = c.trim();
        let tl = TrimTimelineModel::new(TrimTimelineDescriptor {
            duration_ms: c.duration_ms().max(1),
            max_duration_ms: 0,
            min_duration_ms: 0,
            from_ms: trim.from_ms,
            till_ms: trim.till_ms,
            cover_ms: trim.from_ms,
            trim_only: true,
        });
        let clamped_trim = VideoTrim::new(tl.from_ms(), tl.till_ms());
        c.set_trim(clamped_trim);

        let mut player = SegmentPlayer::new(SegmentPlayerOptions {
            keep_alpha: false,
            audio: false,
            volume: c.volume(),
        });
        player.set_sound(c.sounding());
        player.start();
        player.set_segment(tl.from_ms(), tl.till_ms());

        self.timeline = Some(tl);
        self.player = Some(player);
    }

    /// Returns the currently bound [`VideoClip`], if any.
    #[must_use]
    pub const fn clip(&self) -> Option<&VideoClip> {
        self.clip.as_ref()
    }

    /// Returns the active [`TrimTimelineModel`], if a clip is bound.
    #[must_use]
    pub const fn timeline(&self) -> Option<&TrimTimelineModel> {
        self.timeline.as_ref()
    }

    /// Returns the active [`SegmentPlayer`], if a clip is bound.
    #[must_use]
    pub const fn player(&self) -> Option<&SegmentPlayer> {
        self.player.as_ref()
    }

    /// Returns the thumbnail frame cache for this video item.
    #[must_use]
    pub const fn frames_cache(&self) -> &VideoTimelineFramesCache {
        &self.frames_cache
    }

    /// Mutably borrows the thumbnail frame cache for this video item.
    pub fn frames_cache_mut(&mut self) -> &mut VideoTimelineFramesCache {
        &mut self.frames_cache
    }

    /// Applies a trim edit `[from_ms, till_ms]`, propagating to the clip, segment player,
    /// throttled seeker, and recording the new loop duration in `length_changes`.
    pub fn apply_trim(&mut self, from_ms: i64, till_ms: i64, now_ms: i64) -> Option<i64> {
        let tl = self.timeline.as_mut()?;
        let clip = self.clip.as_mut()?;
        tl.set_trim(from_ms, till_ms);
        let new_from = tl.from_ms();
        let new_till = tl.till_ms();
        clip.set_trim(VideoTrim::new(new_from, new_till));
        if let Some(ref mut player) = self.player {
            player.set_segment(new_from, new_till);
            self.seeker.request_seek(new_from, now_ms);
        }
        let loop_dur = clip.loop_duration_ms();
        self.length_changes.push(loop_dur);
        Some(loop_dur)
    }

    /// Synchronizes the timeline handles back from the bound clip's current trim
    /// (`VideoItemTimeline::refreshTrim`).
    pub fn refresh_trim(&mut self) {
        if let (Some(clip), Some(tl)) = (self.clip.as_ref(), self.timeline.as_mut()) {
            let trim = clip.trim();
            tl.set_trim(trim.from_ms, trim.till_ms);
        }
    }

    /// Updates clip audio volume and propagates to the player (`AddVolumeAction`).
    pub fn set_volume(&mut self, volume: f64) {
        if let Some(ref mut clip) = self.clip {
            clip.set_volume(volume);
            if let Some(ref mut player) = self.player {
                player.set_volume(clip.volume());
                player.set_sound(clip.sounding());
            }
        }
    }

    /// Advances the internal seeker throttle clock.
    pub fn tick_seeker(&mut self, now_ms: i64) -> Option<i64> {
        let player = self.player.as_mut()?;
        self.seeker.tick(now_ms, player)
    }

    /// Begins a timeline handle drag on this item.
    pub fn begin_drag(&mut self) {
        if let Some(ref mut player) = self.player {
            self.seeker.begin_dragging(player);
        }
    }

    /// Ends a timeline handle drag and resumes playback inside the trimmed segment.
    pub fn end_drag(&mut self) -> Option<i64> {
        let tl = self.timeline.as_ref()?;
        let player = self.player.as_mut()?;
        Some(
            self.seeker
                .finish_dragging(tl.from_ms(), tl.till_ms(), player),
        )
    }

    /// Drains recorded loop-duration updates emitted after trim changes (`lengthChanges`).
    pub fn drain_length_changes(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.length_changes)
    }
}
