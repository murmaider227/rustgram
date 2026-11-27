//! Inline GIF autoplay and round-video playback helper API for chat history bubbles.
//!
//! Exposes a small, self-contained [`InlinePlayback`] manager: given an [`InlineMediaSource`],
//! it decodes frames through `tg-video::ClipReader`, caches and reuses GPU [`egui::TextureHandle`]s,
//! respects global/per-item pause flags and circular masking, and returns the current frame texture.
//!
//! Port origin: `Telegram/SourceFiles/media/clip/media_clip_reader.cpp` and inline clip helpers in
//! `Telegram/SourceFiles/history/view/media/history_view_gif.cpp`.

use std::collections::HashMap;

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use tg_video::{ClipMode, ClipReader, FrameData, Size, StreamedClip, VideoFrame};

use crate::media::backend::MediaItemId;

/// Default maximum number of simultaneously decoded inline clips kept alive in memory.
pub const DEFAULT_MAX_ACTIVE_INLINE_CLIPS: usize = 24;

/// Longest wait before a playing clip is checked again.
const MAX_INLINE_STEP_MS: i64 = 40;

/// Classification of an inline animated media document in chat history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlinePlaybackKind {
    /// Looping silent GIF / H.264 GIFv animation.
    Gif,
    /// Circular round video message (`apply_circle_mask_rgba`).
    RoundVideo,
    /// Looping WebM/VP9 video sticker with alpha channel.
    VideoSticker,
}

pub use tg_app_core::core_types::misc::InlineDocumentData;

/// Descriptor for an inline document requested by the history view.
#[derive(Debug, Clone)]
pub struct InlineMediaSource {
    /// Unique identifier for caching the decoder and GPU texture across frames.
    pub id: MediaItemId,
    /// Animation/video kind (controls circular masking & autoplay policy).
    pub kind: InlinePlaybackKind,
    /// Encoded document bytes or local file path.
    pub data: InlineDocumentData,
    /// Optional target raster size in pixels (`None` uses stream native dimensions).
    pub target_size: Option<(u32, u32)>,
}

/// Current frame texture and playback telemetry returned to history view bubbles.
#[derive(Clone)]
pub struct InlineFrameState {
    /// Current decoded frame texture ready for `ui.painter().image(...)`, if available.
    pub texture: Option<TextureHandle>,
    /// Frame dimensions in pixels (`width`, `height`).
    pub size: Vec2,
    /// Playback progress in `[0.0, 1.0]` within the current loop.
    pub progress: f32,
    /// Total clip duration in milliseconds (`0` if unknown).
    pub duration_ms: i64,
    /// Number of completed loops.
    pub loop_count: u64,
    /// `true` when the clip is actively advancing frames.
    pub is_playing: bool,
    /// Non-empty if opening or decoding failed.
    pub error: Option<String>,
}

/// The decoder of one inline clip: bytes or a local file decode on the UI thread
/// (`Clip::Reader`), a file that is still downloading on its own thread (TGB-3116).
enum ClipSource {
    Local(Box<ClipReader>),
    Streamed(StreamedClip),
}

impl ClipSource {
    fn frame_presentation_time_ms(&self) -> i64 {
        match self {
            Self::Local(reader) => reader.frame_presentation_time_ms(),
            Self::Streamed(clip) => clip.frame_presentation_time_ms(),
        }
    }

    fn frame_real_time_ms(&self) -> i64 {
        match self {
            Self::Local(reader) => reader.frame_real_time_ms(),
            Self::Streamed(clip) => clip.frame_real_time_ms(),
        }
    }

    fn duration_ms(&self) -> i64 {
        match self {
            Self::Local(reader) => reader.duration_ms(),
            Self::Streamed(clip) => clip.duration_ms(),
        }
    }

    fn loop_count(&self) -> u64 {
        match self {
            Self::Local(reader) => reader.loop_count(),
            Self::Streamed(clip) => clip.loop_count(),
        }
    }

    /// The frame due at `target_ms`; `Ok(None)` while a streamed clip is still decoding it.
    fn read_frames_till(&mut self, target_ms: i64) -> Result<Option<VideoFrame>, String> {
        match self {
            Self::Local(reader) => reader
                .read_frames_till(target_ms)
                .map(Some)
                .map_err(|e| e.to_string()),
            Self::Streamed(clip) => {
                clip.request_frames_till(target_ms);
                match clip.error() {
                    Some(err) => Err(err),
                    None => Ok(clip.take_frame()),
                }
            }
        }
    }

    fn inspect_at(&mut self, position_ms: i64) -> Result<(), String> {
        match self {
            Self::Local(reader) => reader
                .inspect_at(position_ms)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Self::Streamed(clip) => {
                clip.request_seek(position_ms);
                Ok(())
            }
        }
    }
}

struct InlineClipEntry {
    kind: InlinePlaybackKind,
    reader: Option<ClipSource>,
    texture: Option<TextureHandle>,
    size: Vec2,
    start_ms: i64,
    paused: bool,
    paused_at_ms: i64,
    last_touched_ms: i64,
    last_error: Option<String>,
    /// Set by [`InlinePlayback::seek_item`] so a paused clip uploads the frame at the new spot.
    texture_stale: bool,
}

impl InlineClipEntry {
    const fn new(kind: InlinePlaybackKind, reader: Option<ClipSource>, now_ms: i64) -> Self {
        Self {
            kind,
            reader,
            texture: None,
            size: Vec2::ZERO,
            start_ms: now_ms,
            paused: false,
            paused_at_ms: 0,
            last_touched_ms: now_ms,
            last_error: None,
            texture_stale: false,
        }
    }
}

/// Manager for inline GIF autoplay and round-video previews in the chat history list.
pub struct InlinePlayback {
    entries: HashMap<MediaItemId, InlineClipEntry>,
    max_active: usize,
    paused_all: bool,
    autoplay_gifs: bool,
    autoplay_round_videos: bool,
}

impl Default for InlinePlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl InlinePlayback {
    /// Creates a new [`InlinePlayback`] manager with autoplay enabled for GIFs and round videos.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            max_active: DEFAULT_MAX_ACTIVE_INLINE_CLIPS,
            paused_all: false,
            autoplay_gifs: true,
            autoplay_round_videos: true,
        }
    }

    /// Configures the maximum number of concurrent decoders kept in cache.
    pub fn set_max_active(&mut self, max_active: usize) {
        self.max_active = max_active.max(1);
    }

    /// Pauses or resumes all inline animations (e.g. while fullscreen viewer is open).
    pub fn set_paused_all(&mut self, paused: bool) {
        self.paused_all = paused;
    }

    /// Returns `true` when global inline pause is active.
    #[must_use]
    pub const fn is_paused_all(&self) -> bool {
        self.paused_all
    }

    /// Enables or disables automatic GIF playback.
    pub fn set_autoplay_gifs(&mut self, enabled: bool) {
        self.autoplay_gifs = enabled;
    }

    /// Enables or disables automatic silent round-video preview playback.
    pub fn set_autoplay_round_videos(&mut self, enabled: bool) {
        self.autoplay_round_videos = enabled;
    }

    /// Toggles pause state for a single inline document.
    pub fn toggle_item_paused(&mut self, id: MediaItemId, now_ms: i64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            let next = !entry.paused;
            self.set_item_paused(id, next, now_ms);
        }
    }

    /// Idempotently sets the pause state for a single inline document.
    ///
    /// Calling `set_item_paused(id, true, t)` when already paused or `set_item_paused(id, false, t)`
    /// when already playing is a no-op so timeline offsets (`start_ms` / `paused_at_ms`) remain
    /// intact across repeated visibility updates.
    pub fn set_item_paused(&mut self, id: MediaItemId, paused: bool, now_ms: i64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            if entry.paused == paused {
                return;
            }
            if paused {
                entry.paused = true;
                entry.paused_at_ms = (now_ms - entry.start_ms).max(0);
            } else {
                entry.paused = false;
                entry.start_ms = now_ms - entry.paused_at_ms;
            }
        }
    }

    /// Seeks `id` to `position_ms` within the stream and restarts its clock there, so a round
    /// video switched to sound plays from the start in step with its audio
    /// (`Gif::playAnimation` -> `Media::Streaming` round instance restarting at 0).
    pub fn seek_item(&mut self, id: MediaItemId, position_ms: i64, now_ms: i64) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        let position_ms = position_ms.max(0);
        if let Some(reader) = &mut entry.reader {
            match reader.inspect_at(position_ms) {
                Ok(_) => entry.last_error = None,
                Err(err) => entry.last_error = Some(err),
            }
        }
        entry.start_ms = now_ms - position_ms;
        entry.paused_at_ms = position_ms;
        entry.texture_stale = true;
    }

    /// Returns the stream-relative position (`frameRealTime`) of the frame shown for `id`.
    #[must_use]
    pub fn item_position_ms(&self, id: MediaItemId) -> Option<i64> {
        self.entries
            .get(&id)
            .and_then(|entry| entry.reader.as_ref())
            .map(ClipSource::frame_real_time_ms)
    }

    /// Returns `true` if `id` is currently tracked and paused.
    #[must_use]
    pub fn is_item_paused(&self, id: MediaItemId) -> bool {
        self.entries.get(&id).is_some_and(|entry| entry.paused)
    }

    /// Returns the number of cached inline decoders currently held.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.entries.len()
    }

    /// Drops cached decoders that have not been rendered for longer than `max_idle_ms`.
    pub fn evict_stale(&mut self, now_ms: i64, max_idle_ms: i64) {
        self.entries
            .retain(|_, entry| now_ms.saturating_sub(entry.last_touched_ms) <= max_idle_ms);
    }

    /// Releases all active inline decoders and GPU textures.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns the current frame texture and playback state for `source` at `now_ms`.
    ///
    /// Lazily opens a `tg_video::ClipReader` on first call, applies circular masking for
    /// [`InlinePlaybackKind::RoundVideo`], advances frames up to `now_ms`, and reuses the
    /// uploaded `egui::TextureHandle` across frames.
    pub fn frame_for(
        &mut self,
        ctx: &Context,
        source: &InlineMediaSource,
        now_ms: i64,
    ) -> InlineFrameState {
        if !self.entries.contains_key(&source.id) {
            self.enforce_capacity();
            let entry = Self::create_entry(ctx, source, now_ms);
            self.entries.insert(source.id, entry);
        }

        let autoplay_allowed = match source.kind {
            InlinePlaybackKind::Gif | InlinePlaybackKind::VideoSticker => self.autoplay_gifs,
            InlinePlaybackKind::RoundVideo => self.autoplay_round_videos,
        };
        let global_paused = self.paused_all;

        let Some(entry) = self.entries.get_mut(&source.id) else {
            return InlineFrameState {
                texture: None,
                size: Vec2::ZERO,
                progress: 0.0,
                duration_ms: 0,
                loop_count: 0,
                is_playing: false,
                error: Some("inline entry missing".to_owned()),
            };
        };

        entry.last_touched_ms = now_ms;
        let is_playing =
            autoplay_allowed && !global_paused && !entry.paused && entry.reader.is_some();

        if let Some(reader) = &mut entry.reader {
            let need_first_frame = entry.texture.is_none() || entry.texture_stale;
            let target_ms = if is_playing {
                (now_ms - entry.start_ms).max(0)
            } else {
                entry.paused_at_ms
            };
            // The shown frame stays until the clock reaches the next frame's time; decoding,
            // copying and uploading it again on every repaint only costs UI time.
            let frame_due = reader.frame_presentation_time_ms() <= target_ms;
            if need_first_frame || (is_playing && frame_due) {
                match reader.read_frames_till(target_ms) {
                    Ok(None) => {}
                    Ok(Some(frame)) => {
                        entry.size = Vec2::new(frame.width as f32, frame.height as f32);
                        if let FrameData::Rgba(bytes) = &frame.data {
                            let img = ColorImage::from_rgba_unmultiplied(
                                [frame.width as usize, frame.height as usize],
                                bytes,
                            );
                            if let Some(tex) = &mut entry.texture {
                                tex.set(img, TextureOptions::LINEAR);
                            } else {
                                entry.texture = Some(ctx.load_texture(
                                    format!("tg_inline_{:?}_{}", entry.kind, source.id.0),
                                    img,
                                    TextureOptions::LINEAR,
                                ));
                            }
                        }
                        entry.last_error = None;
                        entry.texture_stale = false;
                    }
                    Err(err) => {
                        entry.last_error = Some(err);
                    }
                }
            }
            if is_playing {
                // Wake up when the next frame is due (`Clip::Reader` frame timer), not on a
                // fixed 40 Hz tick that shows 30 fps clips at uneven 25 / 50 ms steps.
                let due_in = entry.start_ms + reader.frame_presentation_time_ms() - now_ms;
                let delay_ms = due_in.clamp(1, MAX_INLINE_STEP_MS).unsigned_abs();
                ctx.request_repaint_after(std::time::Duration::from_millis(delay_ms));
            }

            let duration_ms = reader.duration_ms();
            let progress = if duration_ms > 0 {
                (reader.frame_real_time_ms() as f32 / duration_ms as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };

            return InlineFrameState {
                texture: entry.texture.clone(),
                size: entry.size,
                progress,
                duration_ms,
                loop_count: reader.loop_count(),
                is_playing,
                error: entry.last_error.clone(),
            };
        }

        InlineFrameState {
            texture: entry.texture.clone(),
            size: entry.size,
            progress: 0.0,
            duration_ms: 0,
            loop_count: 0,
            is_playing: false,
            error: entry.last_error.clone(),
        }
    }

    fn create_entry(ctx: &Context, source: &InlineMediaSource, now_ms: i64) -> InlineClipEntry {
        let resize = source
            .target_size
            .map(|(w, h)| Size::new(w.max(1), h.max(1)));
        let round = source.kind == InlinePlaybackKind::RoundVideo;
        let open_res = match &source.data {
            InlineDocumentData::Bytes(bytes) => {
                ClipReader::from_bytes(bytes.clone(), ClipMode::Normal)
            }
            InlineDocumentData::File(path) => ClipReader::from_file(path, ClipMode::Normal),
            InlineDocumentData::Stream(doc) => {
                // The frame size is known once the first frame is decoded.
                let ctx = ctx.clone();
                let clip = StreamedClip::spawn(
                    std::sync::Arc::clone(&doc.0),
                    ClipMode::Normal,
                    resize,
                    round,
                    move || ctx.request_repaint(),
                );
                return InlineClipEntry::new(source.kind, Some(ClipSource::Streamed(clip)), now_ms);
            }
        };
        match open_res {
            Ok(mut reader) => {
                if resize.is_some() {
                    reader.set_resize(resize);
                }
                if round {
                    reader.set_round_circle(true);
                }
                let info = reader.info();
                let size = Vec2::new(info.video_width as f32, info.video_height as f32);
                InlineClipEntry {
                    size,
                    ..InlineClipEntry::new(
                        source.kind,
                        Some(ClipSource::Local(Box::new(reader))),
                        now_ms,
                    )
                }
            }
            Err(err) => InlineClipEntry {
                last_error: Some(err.to_string()),
                ..InlineClipEntry::new(source.kind, None, now_ms)
            },
        }
    }

    fn enforce_capacity(&mut self) {
        while self.entries.len() >= self.max_active {
            if let Some(&oldest_id) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_touched_ms)
                .map(|(id, _)| id)
            {
                self.entries.remove(&oldest_id);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_dummy_entry(playback: &mut InlinePlayback, id: MediaItemId, start_ms: i64) {
        playback.entries.insert(
            id,
            InlineClipEntry {
                kind: InlinePlaybackKind::Gif,
                reader: None,
                texture: None,
                size: Vec2::new(64.0, 64.0),
                start_ms,
                paused: false,
                paused_at_ms: 0,
                last_touched_ms: start_ms,
                last_error: None,
                texture_stale: false,
            },
        );
    }

    #[test]
    fn set_item_paused_is_idempotent_and_preserves_timeline() {
        let mut playback = InlinePlayback::new();
        let id = MediaItemId(42);
        insert_dummy_entry(&mut playback, id, 1_000);
        assert!(!playback.is_item_paused(id));

        // Pausing at t = 1_500 captures 500 ms elapsed.
        playback.set_item_paused(id, true, 1_500);
        assert!(playback.is_item_paused(id));
        assert_eq!(playback.entries[&id].paused_at_ms, 500);

        // Repeatedly setting paused = true later must NOT overwrite paused_at_ms or resume.
        playback.set_item_paused(id, true, 9_999);
        assert!(playback.is_item_paused(id));
        assert_eq!(playback.entries[&id].paused_at_ms, 500);

        // Resuming at t = 3_000 shifts start_ms so elapsed resumes smoothly from 500 ms.
        playback.set_item_paused(id, false, 3_000);
        assert!(!playback.is_item_paused(id));
        assert_eq!(playback.entries[&id].start_ms, 2_500);

        // Repeatedly setting paused = false must NOT alter start_ms.
        playback.set_item_paused(id, false, 7_777);
        assert!(!playback.is_item_paused(id));
        assert_eq!(playback.entries[&id].start_ms, 2_500);

        // Unknown IDs remain no-ops.
        playback.set_item_paused(MediaItemId(999), true, 4_000);
        assert!(!playback.is_item_paused(MediaItemId(999)));
    }
}
