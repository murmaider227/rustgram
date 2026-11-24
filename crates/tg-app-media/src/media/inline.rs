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
use tg_video::{ClipMode, ClipReader, FrameData, Size};

use crate::media::backend::MediaItemId;

/// Default maximum number of simultaneously decoded inline clips kept alive in memory.
pub const DEFAULT_MAX_ACTIVE_INLINE_CLIPS: usize = 24;

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

struct InlineClipEntry {
    kind: InlinePlaybackKind,
    reader: Option<ClipReader>,
    texture: Option<TextureHandle>,
    size: Vec2,
    start_ms: i64,
    paused: bool,
    paused_at_ms: i64,
    last_touched_ms: i64,
    last_error: Option<String>,
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
            let entry = self.create_entry(source, now_ms);
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
            let need_first_frame = entry.texture.is_none();
            if is_playing || need_first_frame {
                let target_ms = if is_playing {
                    (now_ms - entry.start_ms).max(0)
                } else {
                    entry.paused_at_ms
                };
                match reader.read_frames_till(target_ms) {
                    Ok(frame) => {
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
                    }
                    Err(err) => {
                        entry.last_error = Some(err.to_string());
                    }
                }
            }
            if is_playing {
                ctx.request_repaint_after(std::time::Duration::from_millis(25));
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

    fn create_entry(&self, source: &InlineMediaSource, now_ms: i64) -> InlineClipEntry {
        let open_res = match &source.data {
            InlineDocumentData::Bytes(bytes) => {
                ClipReader::from_bytes(bytes.clone(), ClipMode::Normal)
            }
            InlineDocumentData::File(path) => ClipReader::from_file(path, ClipMode::Normal),
        };
        match open_res {
            Ok(mut reader) => {
                if let Some((w, h)) = source.target_size {
                    reader.set_resize(Some(Size::new(w.max(1), h.max(1))));
                }
                if source.kind == InlinePlaybackKind::RoundVideo {
                    reader.set_round_circle(true);
                }
                let info = reader.info();
                let size = Vec2::new(info.video_width as f32, info.video_height as f32);
                InlineClipEntry {
                    kind: source.kind,
                    reader: Some(reader),
                    texture: None,
                    size,
                    start_ms: now_ms,
                    paused: false,
                    paused_at_ms: 0,
                    last_touched_ms: now_ms,
                    last_error: None,
                }
            }
            Err(err) => InlineClipEntry {
                kind: source.kind,
                reader: None,
                texture: None,
                size: Vec2::ZERO,
                start_ms: now_ms,
                paused: false,
                paused_at_ms: 0,
                last_touched_ms: now_ms,
                last_error: Some(err.to_string()),
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
