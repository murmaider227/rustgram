//! Interactive egui Video Editor modal backed by `tg-editor`'s UI-independent
//! [`VideoEditorModel`] and `tg-video` / FFmpeg export pipeline.
//!
//! Port origin: `Telegram/SourceFiles/editor/video/video_editor.cpp`,
//! `video_timeline.cpp`, `video_quality_slider.cpp`, and `editor_trim_timeline.cpp`.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Role,
    ScrollArea, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo,
};
use tg_editor::video::{
    ClipId, ClipPosition, ItemVisibilityRange, MultiClipExportOptions, MultiClipExportPlan,
    MultiClipSegmentPlayer, MultiClipTimeline, VideoAudioTrack, VideoClipSource, VideoEditorData,
    VideoEditorModel, VideoModifications, VideoSourceInfo, VideoTrim, build_multi_clip_export_plan,
    format_stamp, video_quality_label,
};
use tg_editor::{
    AudioTrackClip, AudioTrackTimelineModel, Color as EditorColor, CropEnhanceParams, EditorError,
    ItemId, LocationData, LocationItem, PaintScene, PointF, SceneDraft, ShapeItem, ShapeType,
    Size as EditorSize, TextAlignment, TextItem, TextStyle, TextTypeface, VideoTimelineDraft,
    WeatherData, WeatherItem, collect_story_areas,
};
use tg_ui::tg_style::st;

use crate::history::media::format_file_size;
use crate::history::theme::HistoryColors;

/// Outcome returned when the user closes [`VideoEditorModalState`].
#[derive(Debug, Clone)]
pub struct VideoEditorOutcome {
    /// Target candidate index inside [`crate::history::compose_boxes::AttachFilesBoxState`].
    pub file_index: usize,
    /// Collected non-destructive video edits (trim, quality, mute/GIF, cover, transforms).
    pub modifications: VideoModifications,
    /// Optional exported MP4 file path if an export was run to disk.
    pub exported_path: Option<PathBuf>,
    /// Optional cover-frame JPEG thumbnail bytes.
    pub cover_jpeg: Option<Vec<u8>>,
    /// Versioned [`SceneDraft`] capturing overlays, crop/enhance, and multi-clip timeline state.
    pub scene_draft: Option<SceneDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragTarget {
    None,
    LeftHandle,
    RightHandle,
    CoverHead,
}

static ACTIVE_VIDEO_EDITOR_TEMP_DIR: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);
static NEXT_VIDEO_EDIT_EXPORT_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Configures the active account's temporary directory for video editor FFmpeg exports
/// (e.g. `<tdata>/temp_<dataName>/video_edit`) so exported temp MP4s never land in `%TEMP%`.
pub fn set_active_video_editor_temp_dir(dir: Option<PathBuf>) {
    if let Ok(mut guard) = ACTIVE_VIDEO_EDITOR_TEMP_DIR.lock() {
        *guard = dir;
    }
}

/// Returns the currently configured account video-editor temp directory, if any.
#[must_use]
pub fn active_video_editor_temp_dir() -> Option<PathBuf> {
    ACTIVE_VIDEO_EDITOR_TEMP_DIR
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

/// Deletes stale temporary video editor exports inside `dir` (skipping any file held open).
pub fn cleanup_video_editor_temp_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
        } else if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Interactive Video Editor modal state hosted alongside `PhotoEditorModalState` in the
/// Send Files box (`crates/tg-app/src/history/compose_boxes.rs`).
pub struct VideoEditorModalState {
    /// Target file index in `AttachFilesBoxState`.
    pub file_index: usize,
    /// Display name of the video being edited.
    pub file_name: String,
    /// UI-independent video editor model from `tg-editor`.
    pub model: VideoEditorModel,
    /// Multi-clip timeline and headless segment playback controller (`tg-editor::video`).
    pub clip_player: MultiClipSegmentPlayer,
    /// Currently selected clip index inside `clip_player.timeline()`.
    pub selected_clip_index: usize,
    /// Optional overlay audio track timeline (`editor/audio_track_timeline.cpp`).
    pub audio_timeline: AudioTrackTimelineModel,
    /// Scene items overlay layer (text, shapes, location, weather, story areas).
    pub overlay_scene: PaintScene,
    /// Currently selected overlay [`ItemId`] for visibility-window editing.
    pub selected_overlay_id: Option<ItemId>,
    /// Most recently saved in-memory [`SceneDraft`] checkpoint.
    pub saved_draft: Option<SceneDraft>,
    /// Optional disk path for reading/writing versioned binary [`SceneDraft`] files.
    pub draft_path: Option<PathBuf>,
    /// Optional explicit temp export directory under the active account folder.
    export_temp_dir: Option<PathBuf>,
    /// Editable trim range text (synced when editing timestamps directly).
    pub range_input: String,
    /// Active drag handle on the trim timeline.
    drag_target: DragTarget,
    /// Last frame timestamp (seconds) for advancing `clip_player` while playing.
    last_frame_time: Option<f64>,
    /// Measured outer height from the previous frame for vertical centering without clipping.
    last_modal_height: Option<f32>,
    /// Measured unclipped inner content height to engage vertical scrolling only when needed.
    last_content_height: Option<f32>,
    /// Cached thumbnail textures for the timeline strip.
    thumb_textures: Vec<TextureHandle>,
    /// Cached cover-frame preview texture.
    cover_tex: Option<TextureHandle>,
    /// Cover timestamp currently represented by `cover_tex`.
    cover_tex_ms: i64,
    /// Seed for procedural preview rendering when editing synthetic sample videos.
    preview_seed: u8,
    /// Optional status/error message from FFmpeg export, draft I/O, or timeline ops.
    pub status_message: Option<String>,
}

impl std::fmt::Debug for VideoEditorModalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoEditorModalState")
            .field("file_index", &self.file_index)
            .field("file_name", &self.file_name)
            .field("from_ms", &self.model.timeline.from_ms())
            .field("till_ms", &self.model.timeline.till_ms())
            .field("cover_ms", &self.model.timeline.cover_ms())
            .field("quality", &self.model.quality.selected_shorter_side())
            .field("muted", &self.model.audio.muted)
            .field("gif", &self.model.gif)
            .finish()
    }
}

impl VideoEditorModalState {
    /// Opens the video editor for a file entry in `AttachFilesBoxState`.
    ///
    /// If `path` exists on disk and FFmpeg can probe it, real metadata, timeline thumbnails,
    /// and cover-frame extraction are used. Otherwise falls back cleanly to synthetic metadata
    /// and procedural preview frames (for offline demos and unit tests).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn open_for_file(
        file_index: usize,
        file_name: impl Into<String>,
        width: i32,
        height: i32,
        duration_ms: i64,
        file_size: u64,
        initial: Option<VideoModifications>,
        seed: u8,
    ) -> Self {
        let name = file_name.into();
        let initial_mods = initial.unwrap_or_default();
        let data = VideoEditorData::default();
        let path_ref = Path::new(&name);

        let model = if path_ref.is_file()
            && tg_editor::video::is_ffmpeg_available()
            && let Ok(probed) =
                VideoEditorModel::open_file(path_ref, data.clone(), initial_mods.clone())
        {
            probed
        } else {
            let info = VideoSourceInfo {
                dimensions: tg_editor::Size::new(width.max(160), height.max(120)),
                duration_ms: duration_ms.max(1_000),
                fps: 30.0,
                file_size: file_size.max(120_000),
                video_bitrate: 1_800_000,
                audio: VideoAudioTrack {
                    has_audio: true,
                    channels: 2,
                    sample_rate: 48_000,
                    bitrate: 128_000,
                    codec_name: "aac".to_owned(),
                    muted: false,
                    volume: 1.0,
                },
                video_codec: "h264".to_owned(),
                rotation: 0,
            };
            VideoEditorModel::new(None, info, data, initial_mods)
        };

        let range_input = model.timeline.duration_label_text();
        let dim = model.source_info.dimensions;
        let max_audio_ms = (model.timeline.till_ms() - model.timeline.from_ms()).max(1_000);
        let mut primary_source = match &model.source_path {
            Some(path) => VideoClipSource::from_path(
                path.clone(),
                model.source_info.duration_ms,
                dim,
                model.audio.has_audio,
            ),
            None => VideoClipSource::synthetic(
                model.source_info.duration_ms,
                dim,
                model.audio.has_audio,
            ),
        };
        primary_source.fps = model.source_info.fps;
        primary_source.initial_trim =
            VideoTrim::new(model.timeline.from_ms(), model.timeline.till_ms());
        primary_source.initial_volume = f64::from(model.audio.volume);
        let mut multi_timeline = MultiClipTimeline::from_sources([primary_source]);
        let _ = multi_timeline.set_clip_muted(0, model.audio.muted);
        let clip_player = MultiClipSegmentPlayer::new(multi_timeline);

        Self {
            file_index,
            file_name: name,
            model,
            clip_player,
            selected_clip_index: 0,
            audio_timeline: AudioTrackTimelineModel::new(Some(max_audio_ms)),
            overlay_scene: PaintScene::from_size(dim),
            selected_overlay_id: None,
            saved_draft: None,
            draft_path: None,
            export_temp_dir: None,
            range_input,
            drag_target: DragTarget::None,
            last_frame_time: None,
            last_modal_height: None,
            last_content_height: None,
            thumb_textures: Vec::new(),
            cover_tex: None,
            cover_tex_ms: -1,
            preview_seed: seed,
            status_message: None,
        }
    }

    /// Appends an additional clip to the multi-clip timeline and selects it.
    pub fn append_clip(&mut self, source: VideoClipSource) -> ClipId {
        let id = self.clip_player.mutate_timeline(|tl| tl.append(source));
        self.selected_clip_index = self.clip_player.timeline().len().saturating_sub(1);
        self.status_message = None;
        id
    }

    /// Appends a synthetic sample clip matching the primary video's dimensions (for demo/UI).
    pub fn append_sample_clip(&mut self, duration_ms: i64) -> ClipId {
        let dim = self.model.source_info.dimensions;
        let src = VideoClipSource::synthetic(duration_ms.max(1_000), dim, true);
        self.append_clip(src)
    }

    /// Splits the clip covering the current global playhead timestamp into two clips.
    pub fn split_at_playhead(&mut self) -> Result<(ClipId, ClipId), EditorError> {
        let pos = self.clip_player.global_position_ms();
        let res = self
            .clip_player
            .mutate_timeline(|tl| tl.split_at_global_ms(pos));
        match res {
            Ok((left, right)) => {
                if let Some((idx, _)) = self.clip_player.timeline().find_by_id(right) {
                    self.selected_clip_index = idx;
                }
                self.status_message = None;
                Ok((left, right))
            }
            Err(err) => {
                self.status_message = Some(format!("Split failed: {err}"));
                Err(err)
            }
        }
    }

    /// Moves the currently selected clip left (`-1`) or right (`+1`) in playback order.
    pub fn move_selected_clip(&mut self, delta: isize) -> Result<(), EditorError> {
        let len = self.clip_player.timeline().len();
        if len <= 1 || self.selected_clip_index >= len {
            return Ok(());
        }
        let target =
            (self.selected_clip_index as isize + delta).clamp(0, (len - 1) as isize) as usize;
        if target == self.selected_clip_index {
            return Ok(());
        }
        let from = self.selected_clip_index;
        self.clip_player
            .mutate_timeline(|tl| tl.reorder(from, target))?;
        self.selected_clip_index = target;
        self.status_message = None;
        Ok(())
    }

    /// Removes the currently selected clip if more than one clip exists on the timeline.
    pub fn remove_selected_clip(&mut self) -> Result<(), EditorError> {
        let len = self.clip_player.timeline().len();
        if len <= 1 {
            return Err(EditorError::InvalidTimeline(
                "cannot remove the last remaining clip".to_owned(),
            ));
        }
        let idx = self.selected_clip_index.min(len - 1);
        self.clip_player.mutate_timeline(|tl| tl.remove(idx))?;
        self.selected_clip_index = idx.min(self.clip_player.timeline().len().saturating_sub(1));
        self.status_message = None;
        Ok(())
    }

    /// Sets mute state on the currently selected clip.
    pub fn set_selected_clip_muted(&mut self, muted: bool) -> Result<(), EditorError> {
        let idx = self.selected_clip_index;
        self.clip_player
            .mutate_timeline(|tl| tl.set_clip_muted(idx, muted))
    }

    /// Updates audio gain (`0.0..=1.0`) on the currently selected clip.
    pub fn set_selected_clip_volume(&mut self, volume: f64) -> Result<(), EditorError> {
        let idx = self.selected_clip_index;
        self.clip_player
            .mutate_timeline(|tl| tl.set_clip_volume(idx, volume))
    }

    /// Updates non-destructive trim bounds on the currently selected clip.
    pub fn set_selected_clip_trim(&mut self, trim: VideoTrim) -> Result<(), EditorError> {
        let idx = self.selected_clip_index;
        self.clip_player
            .mutate_timeline(|tl| tl.set_clip_trim(idx, trim))
    }

    /// Toggles multi-clip playback between playing and paused.
    pub fn toggle_playback(&mut self) {
        if self.clip_player.is_playing() {
            self.clip_player.pause();
        } else {
            self.clip_player.play();
        }
    }

    /// Seeks the multi-clip playhead to `global_ms` (clamped into `[0, total_duration_ms]`).
    pub fn seek_global_ms(&mut self, global_ms: i64) -> Option<ClipPosition> {
        self.clip_player.seek_global(global_ms)
    }

    /// Registers a validated `[start_ms, end_ms)` visibility range for an overlay scene item.
    pub fn set_overlay_visibility(
        &mut self,
        item_id: ItemId,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<ItemVisibilityRange, EditorError> {
        let res = self
            .model
            .timeline
            .set_item_visibility(item_id, start_ms, end_ms);
        if let Err(ref err) = res {
            self.status_message = Some(format!("Overlay timing: {err}"));
        } else {
            self.status_message = None;
        }
        res
    }

    /// Clears any explicit visibility window for `item_id` (making it visible for the full video).
    pub fn clear_overlay_visibility(&mut self, item_id: ItemId) -> Option<ItemVisibilityRange> {
        self.model
            .timeline
            .overlay_visibility_mut()
            .remove_range(item_id)
    }

    /// Returns active overlay item IDs visible at the current multi-clip playhead timestamp.
    #[must_use]
    pub fn visible_overlays_at_playhead(&self) -> Vec<ItemId> {
        let now_ms = self.clip_player.global_position_ms();
        self.model
            .timeline
            .active_overlay_items_at(&self.overlay_scene, now_ms)
    }

    /// Builds a deterministic FFmpeg multi-clip concat/filtergraph export plan via `tg-editor`.
    pub fn build_multi_clip_export_plan(
        &self,
        output_path: &Path,
    ) -> Result<MultiClipExportPlan, EditorError> {
        let opts = MultiClipExportOptions {
            quality: self.model.quality.selected_shorter_side(),
            gif: self.model.gif,
            geometry: self.model.geometry.clone(),
            target_dimensions: Some(self.model.source_info.dimensions),
            ..MultiClipExportOptions::default()
        };
        build_multi_clip_export_plan(
            self.clip_player.timeline(),
            Some((
                &self.overlay_scene,
                self.model.timeline.overlay_visibility(),
            )),
            &opts,
            output_path,
        )
    }

    /// Snapshots all crop/enhance, overlay scene items, multi-clip timeline clips, visibility
    /// windows, quality/GIF flags, and audio track into a versioned [`SceneDraft`].
    #[must_use]
    pub fn build_scene_draft(&self) -> SceneDraft {
        let video_timeline = VideoTimelineDraft {
            timeline: self.clip_player.timeline().clone(),
            overlay_visibility: self.model.timeline.overlay_visibility().clone(),
            quality: self.model.quality.selected_shorter_side(),
            gif: self.model.gif,
            cover_ms: self.model.timeline.cover_ms(),
            audio_track: self.audio_timeline.track.clone(),
        };
        SceneDraft::new(
            CropEnhanceParams::from(&self.model.geometry),
            self.overlay_scene.clone(),
            Some(video_timeline),
        )
    }

    /// Restores crop/enhance, overlay scene items, and multi-clip timeline state from `draft`.
    pub fn apply_scene_draft(&mut self, draft: &SceneDraft) -> Result<(), EditorError> {
        draft.validate()?;
        draft.crop_enhance.apply_to(&mut self.model.geometry);
        self.overlay_scene = draft.scene.clone();
        self.selected_overlay_id = self.overlay_scene.active_items_by_z().last().map(|e| e.id);
        if let Some(ref vt) = draft.video_timeline {
            if !vt.timeline.is_empty() {
                self.clip_player.replace_timeline(vt.timeline.clone());
                self.selected_clip_index = self
                    .selected_clip_index
                    .min(vt.timeline.len().saturating_sub(1));
                if let Some(first) = vt.timeline.get(0) {
                    self.model
                        .timeline
                        .set_trim(first.effective_from_ms(), first.effective_till_ms());
                }
            }
            *self.model.timeline.overlay_visibility_mut() = vt.overlay_visibility.clone();
            self.model.quality.set_shorter_side(vt.quality);
            self.model.set_gif(vt.gif);
            self.model.timeline.set_cover(vt.cover_ms);
            if let Some(ref audio) = vt.audio_track {
                self.audio_timeline.set_track(audio.clone());
            } else {
                self.audio_timeline.clear_track();
            }
        }
        self.range_input = self.model.timeline.duration_label_text();
        self.cover_tex = None;
        self.status_message = None;
        Ok(())
    }

    /// Builds a deterministic per-file draft path under `drafts_dir` (e.g.
    /// `<profile_data_dir>/video_drafts/<stem>_<hash>.tged`) so drafts never collide or escape.
    #[must_use]
    pub fn default_draft_path(drafts_dir: &Path, file_name: &str) -> PathBuf {
        let stem = Path::new(file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("video");
        let sanitized: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
        let prefix = if sanitized.is_empty() {
            "video".to_owned()
        } else {
            sanitized
        };
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in file_name.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        drafts_dir.join(format!("{prefix}_{hash:016x}.tged"))
    }

    /// Persists the current [`SceneDraft`] as a checksummed binary envelope at `path` and returns
    /// its deterministic 64-bit fingerprint.
    pub fn save_draft_to_path(&mut self, path: &Path) -> Result<u64, EditorError> {
        let draft = self.build_scene_draft();
        let bytes = draft.to_bytes()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| EditorError::DraftSerialization(e.to_string()))?;
        }
        std::fs::write(path, bytes).map_err(|e| EditorError::DraftSerialization(e.to_string()))?;
        let fp = draft.fingerprint();
        self.saved_draft = Some(draft);
        self.draft_path = Some(path.to_path_buf());
        self.status_message = Some(format!("Draft saved ({fp:08x})"));
        Ok(fp)
    }

    /// Loads and restores a checksummed binary [`SceneDraft`] from `path`, returning its
    /// deterministic 64-bit fingerprint.
    pub fn load_draft_from_path(&mut self, path: &Path) -> Result<u64, EditorError> {
        let bytes =
            std::fs::read(path).map_err(|e| EditorError::DraftDeserialization(e.to_string()))?;
        let draft = SceneDraft::from_bytes(&bytes)?;
        let fp = draft.fingerprint();
        self.apply_scene_draft(&draft)?;
        self.saved_draft = Some(draft);
        self.draft_path = Some(path.to_path_buf());
        self.status_message = Some(format!("Draft restored ({fp:08x})"));
        Ok(fp)
    }

    fn ensure_thumbnails(&mut self, ctx: &egui::Context, strip_w: i32, strip_h: i32) {
        if !self.thumb_textures.is_empty() {
            return;
        }
        if let Ok(strip) = self.model.load_timeline_thumbnails(strip_w, strip_h)
            && !strip.frames.is_empty()
        {
            self.thumb_textures = strip
                .frames
                .iter()
                .enumerate()
                .map(|(i, img)| {
                    let color = ColorImage::from_rgba_unmultiplied(
                        [img.width() as usize, img.height() as usize],
                        img.as_raw(),
                    );
                    ctx.load_texture(
                        format!("tg_video_tl_thumb_{}_{i}", self.file_index),
                        color,
                        TextureOptions::LINEAR,
                    )
                })
                .collect();
            return;
        }

        // Procedural thumbnail strip fallback for sample/offline videos.
        let count = 8usize;
        let tw = 48usize;
        let th = 36usize;
        self.thumb_textures = (0..count)
            .map(|i| {
                let frac = i as f32 / (count.saturating_sub(1).max(1)) as f32;
                let img = procedural_video_frame(tw as u32, th as u32, frac, self.preview_seed);
                let color = ColorImage::from_rgba_unmultiplied([tw, th], img.as_raw());
                ctx.load_texture(
                    format!("tg_video_tl_proc_{}_{i}", self.file_index),
                    color,
                    TextureOptions::LINEAR,
                )
            })
            .collect();
    }

    fn ensure_cover_preview(&mut self, ctx: &egui::Context) {
        let cover_ms = self.model.timeline.cover_ms();
        if self.cover_tex.is_some() && self.cover_tex_ms == cover_ms {
            return;
        }
        self.cover_tex_ms = cover_ms;
        if let Ok(img) = self.model.extract_cover_preview(360) {
            let color = ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            self.cover_tex = Some(ctx.load_texture(
                format!("tg_video_cover_preview_{}", self.file_index),
                color,
                TextureOptions::LINEAR,
            ));
            return;
        }
        let dur = self.model.timeline.duration_ms().max(1) as f32;
        let frac = (cover_ms as f32 / dur).clamp(0.0, 1.0);
        let raw = procedural_video_frame(240, 144, frac, self.preview_seed);
        let transformed = tg_editor::apply(&raw, &self.model.geometry);
        let color = ColorImage::from_rgba_unmultiplied(
            [transformed.width() as usize, transformed.height() as usize],
            transformed.as_raw(),
        );
        self.cover_tex = Some(ctx.load_texture(
            format!("tg_video_cover_proc_{}", self.file_index),
            color,
            TextureOptions::LINEAR,
        ));
    }

    /// Configures an explicit per-account temp directory for exported video files.
    pub fn set_export_temp_dir(&mut self, dir: PathBuf) {
        self.export_temp_dir = Some(dir);
    }

    /// Resolves the temporary output directory for exported video files: explicit override,
    /// then the active account's temp folder, then the draft directory's `tmp_exports`, and only
    /// as a last resort a PID-scoped fallback.
    #[must_use]
    pub fn resolve_export_temp_dir(&self) -> PathBuf {
        if let Some(ref dir) = self.export_temp_dir {
            return dir.clone();
        }
        if let Some(dir) = active_video_editor_temp_dir() {
            return dir;
        }
        if let Some(parent) = self.draft_path.as_ref().and_then(|p| p.parent()) {
            return parent.join("tmp_exports");
        }
        std::env::temp_dir().join(format!("rustgram_video_edit_pid_{}", std::process::id()))
    }

    /// Runs an FFmpeg export to a temporary MP4 file under the account temp directory if a real
    /// source file is attached; otherwise returns the collected [`VideoModifications`] immediately.
    pub fn finalize_outcome(&mut self) -> VideoEditorOutcome {
        let mods = self.model.collect();
        let mut exported_path = None;
        let mut cover_jpeg = None;
        if self.model.source_path.is_some() && tg_editor::video::is_ffmpeg_available() {
            let out_dir = self.resolve_export_temp_dir();
            let _ = std::fs::create_dir_all(&out_dir);
            let seq = NEXT_VIDEO_EDIT_EXPORT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let out_path = out_dir.join(format!(
                "video_edit_{}_{}_{}.mp4",
                self.file_index,
                mods.from_ms.max(0),
                seq
            ));
            match self.model.export_to(&out_path, true) {
                Ok(res) => {
                    exported_path = Some(res.output_path);
                    cover_jpeg = res.cover_jpeg;
                    self.status_message = None;
                }
                Err(err) => {
                    self.status_message = Some(format!("Export warning: {err}"));
                }
            }
        }
        let scene_draft = Some(self.build_scene_draft());
        VideoEditorOutcome {
            file_index: self.file_index,
            modifications: mods,
            exported_path,
            cover_jpeg,
            scene_draft,
        }
    }

    /// Renders the video editor modal panel. Returns `Some(Some(outcome))` on Apply,
    /// `Some(None)` on Cancel, or `None` while open.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
    ) -> Option<Option<VideoEditorOutcome>> {
        let mut outcome = None;
        let now_secs = ui.ctx().input(|i| i.time);
        if let Some(prev) = self.last_frame_time {
            let dt_ms = ((now_secs - prev).clamp(0.0, 0.25) * 1000.0).round() as i64;
            if self.clip_player.is_playing() && dt_ms > 0 {
                let _ = self.clip_player.advance(dt_ms);
                ui.ctx().request_repaint();
            }
        }
        self.last_frame_time = Some(now_secs);

        self.ensure_cover_preview(ui.ctx());
        self.ensure_thumbnails(ui.ctx(), 420, st::video_timeline_strip_height.0);

        let screen = ui.ctx().viewport_rect();
        let modal_w = (screen.width() - 24.0).clamp(320.0, 520.0);
        let max_modal_h = (screen.height() - 24.0).max(260.0);
        let max_body_h = (max_modal_h - 28.0).max(220.0);
        let est_modal_h = self.last_modal_height.unwrap_or(680.0).min(max_modal_h);
        let modal_pos = Pos2::new(
            screen.left() + ((screen.width() - modal_w) * 0.5).max(8.0),
            screen.top() + ((screen.height() - est_modal_h) * 0.5).max(12.0),
        );

        egui::Area::new(Id::unique(("tg_video_editor_modal", "backdrop")))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                );
            });

        let area_resp = egui::Area::new(Id::unique("tg_video_editor_modal"))
            .order(egui::Order::Foreground)
            .fixed_pos(modal_pos)
            .show(ui.ctx(), |ui| {
                // Ensure checkboxes and subtle controls meet non-text contrast in light & dark themes.
                let control_border = Color32::from_rgba_unmultiplied(
                    colors.sub_text_fg.r(),
                    colors.sub_text_fg.g(),
                    colors.sub_text_fg.b(),
                    135,
                );
                ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(1.0, control_border);
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(modal_w - 28.0);
                        let needs_scroll = self
                            .last_content_height
                            .is_some_and(|h| h > max_body_h + 1.0);
                        if needs_scroll {
                            let scroll_out = ScrollArea::vertical()
                                .id_salt("tg_video_editor_scroll")
                                .max_height(max_body_h)
                                .min_scrolled_height(max_body_h)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    self.show_inner(ui, colors, &mut outcome);
                                });
                            self.last_content_height = Some(scroll_out.content_size.y);
                        } else {
                            let inner_resp = ui.vertical(|ui| {
                                self.show_inner(ui, colors, &mut outcome);
                            });
                            self.last_content_height = Some(inner_resp.response.rect.height());
                        }
                    });
            });
        let measured_h = area_resp.response.rect.height();
        if measured_h > 40.0
            && self
                .last_modal_height
                .is_none_or(|prev| (prev - measured_h).abs() > 1.0)
        {
            self.last_modal_height = Some(measured_h);
            ui.ctx().request_repaint();
        }

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = Some(None);
        }
        outcome
    }

    fn show_inner(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
        outcome: &mut Option<Option<VideoEditorOutcome>>,
    ) {
        let out_dim = self.model.output_dimensions();
        let est_bytes = self.model.estimated_output_size();
        ui.horizontal(|ui| {
            let meta_text = format!(
                "{}×{} · ~{}",
                out_dim.width,
                out_dim.height,
                format_file_size(est_bytes)
            );
            let title_max_w = (ui.available_width() - 120.0).max(120.0);
            ui.allocate_ui_with_layout(
                Vec2::new(title_max_w, 20.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("Video Editor — {}", self.file_name))
                                .strong()
                                .size(15.0)
                                .color(colors.bold_fg),
                        )
                        .truncate(),
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(meta_text)
                        .size(11.5)
                        .color(colors.sub_text_fg),
                );
            });
        });
        ui.add_space(6.0);

        // 1. Cover / video preview canvas
        let canvas_w = ui.available_width();
        let canvas_h = (canvas_w * 0.36).clamp(132.0, 184.0);
        let (canvas, _) = ui.allocate_exact_size(Vec2::new(canvas_w, canvas_h), Sense::hover());
        ui.painter().rect_filled(
            canvas,
            CornerRadius::same(8),
            Color32::from_rgb(0x12, 0x16, 0x1C),
        );
        if let Some(ref tex) = self.cover_tex {
            let sz = tex.size_vec2();
            let scale = (canvas.width() / sz.x.max(1.0)).min(canvas.height() / sz.y.max(1.0));
            let draw_rect = Rect::from_center_size(canvas.center(), sz * scale);
            ui.painter().image(
                tex.id(),
                draw_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // Cover timestamp & mute/GIF badge overlay
        let badge_text = format!(
            "Cover: {} · Trim: {}{}",
            format_stamp(self.model.timeline.cover_ms(), 1),
            self.model.timeline.duration_label_text(),
            if self.model.gif {
                " · GIF"
            } else if self.model.audio.muted {
                " · Muted"
            } else {
                ""
            }
        );
        let badge_rect = Rect::from_min_size(
            Pos2::new(canvas.left() + 8.0, canvas.top() + 8.0),
            Vec2::new((canvas.width() - 16.0).min(280.0), 22.0),
        );
        ui.painter().rect_filled(
            badge_rect,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(0, 0, 0, 155),
        );
        ui.painter().text(
            Pos2::new(badge_rect.left() + 8.0, badge_rect.center().y),
            Align2::LEFT_CENTER,
            badge_text,
            FontId::proportional(11.0),
            Color32::WHITE,
        );

        ui.add_space(8.0);

        // 2. Interactive Trim Timeline with frame thumbnails + Left/Right handles + Cover marker
        self.paint_trim_timeline(ui, colors);

        ui.add_space(6.0);

        // 2b. Multi-Clip Sequence Timeline (`tg-editor::video::MultiClipTimeline` + `MultiClipSegmentPlayer`)
        self.paint_multi_clip_timeline(ui, colors);

        ui.add_space(6.0);

        // 3. Quality / Resolution choice (`video_quality_slider.cpp`)
        if self.model.data.allow_quality {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Quality:")
                        .size(12.5)
                        .color(colors.text_fg),
                );
                let levels = self.model.quality.levels().to_vec();
                let selected_idx = self.model.quality.selected_index();
                for (idx, level) in levels.iter().enumerate() {
                    let lbl = video_quality_label(level);
                    if ui.selectable_label(idx == selected_idx, lbl).clicked() {
                        self.model.quality.set_index(idx);
                    }
                }
            });
        }

        ui.add_space(4.0);

        // 4. Mute, GIF toggle, and Audio Track volume
        let narrow_layout = ui.available_width() < 400.0;
        ui.horizontal_wrapped(|ui| {
            let mut muted = self.model.audio.muted;
            if ui.checkbox(&mut muted, "Mute audio").changed() {
                self.model.set_muted(muted);
            }
            if self.model.gif_allowed {
                let mut gif = self.model.gif;
                if ui.checkbox(&mut gif, "Send as GIF").changed() {
                    self.model.set_gif(gif);
                }
            }
            if !narrow_layout
                && self.model.audio.has_audio
                && !self.model.audio.muted
                && !self.model.gif
            {
                self.paint_volume_controls(ui, colors);
            }
        });
        if narrow_layout && self.model.audio.has_audio && !self.model.audio.muted && !self.model.gif
        {
            ui.horizontal(|ui| {
                self.paint_volume_controls(ui, colors);
            });
        }

        ui.add_space(4.0);

        // 5. Transform, Cover & Scene Draft controls
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Rotate 90°").clicked() {
                self.model.rotate_cw();
                self.cover_tex = None;
            }
            if ui.small_button("Flip").clicked() {
                self.model.toggle_flip();
                self.cover_tex = None;
            }
            if ui.small_button("Cover = Start").clicked() {
                let start = self.model.timeline.from_ms();
                self.model.timeline.set_cover(start);
            }
            if ui.small_button("Cover = Mid").clicked() {
                let mid = (self.model.timeline.from_ms() + self.model.timeline.till_ms()) / 2;
                self.model.timeline.set_cover(mid);
            }
            if ui.small_button("Save Draft").clicked() {
                if let Some(path) = self.draft_path.clone() {
                    if let Err(err) = self.save_draft_to_path(&path) {
                        self.status_message = Some(format!("Draft save error: {err}"));
                    }
                } else {
                    let draft = self.build_scene_draft();
                    let fp = draft.fingerprint();
                    self.saved_draft = Some(draft);
                    self.status_message = Some(format!("Draft saved ({fp:08x})"));
                }
            }
            let can_restore =
                self.saved_draft.is_some() || self.draft_path.as_ref().is_some_and(|p| p.is_file());
            if ui
                .add_enabled(can_restore, egui::Button::new("Restore Draft").small())
                .clicked()
            {
                if let Some(draft) = self.saved_draft.clone() {
                    if let Err(err) = self.apply_scene_draft(&draft) {
                        self.status_message = Some(format!("Draft restore error: {err}"));
                    } else {
                        self.status_message =
                            Some(format!("Draft restored ({:08x})", draft.fingerprint()));
                    }
                } else if let Some(path) = self.draft_path.clone()
                    && let Err(err) = self.load_draft_from_path(&path)
                {
                    self.status_message = Some(format!("Draft restore error: {err}"));
                }
            }
            if ui.small_button("Reset").clicked() {
                self.model.reset();
                self.audio_timeline.clear_track();
                self.overlay_scene = PaintScene::from_size(self.model.source_info.dimensions);
                self.selected_overlay_id = None;
                let reset_src = VideoClipSource::synthetic(
                    self.model.source_info.duration_ms,
                    self.model.source_info.dimensions,
                    self.model.audio.has_audio,
                );
                self.clip_player
                    .replace_timeline(MultiClipTimeline::from_sources([reset_src]));
                self.selected_clip_index = 0;
                self.range_input = self.model.timeline.duration_label_text();
                self.cover_tex = None;
                self.status_message = None;
            }
        });

        ui.add_space(6.0);

        // 6. Audio Track Timeline (`audio_track_timeline.cpp`) & Scene Overlays (`scene_items.rs`)
        self.paint_audio_track_timeline(ui, colors);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let dim = self.model.source_info.dimensions;
            let center = PointF::new(f64::from(dim.width) * 0.5, f64::from(dim.height) * 0.5);
            if ui.small_button("+ Text").clicked() {
                let id = self.overlay_scene.add_text(TextItem::new(
                    "Caption",
                    EditorColor::WHITE,
                    28.0,
                    TextStyle::SemiTransparent,
                    TextTypeface::Default,
                    TextAlignment::Center,
                    dim,
                    center,
                ));
                self.selected_overlay_id = Some(id);
            }
            if ui.small_button("+ Shape").clicked() {
                let id = self.overlay_scene.add_shape(ShapeItem::new(
                    ShapeType::Star,
                    EditorColor::rgb(255, 210, 64),
                    4.0,
                    false,
                    center,
                    120.0,
                ));
                self.selected_overlay_id = Some(id);
            }
            if ui.small_button("+ Location").clicked() {
                let id = self.overlay_scene.add_location(LocationItem::new(
                    LocationData {
                        lat: 47.3769,
                        lon: 8.5417,
                        title: "Location".to_owned(),
                        ..LocationData::default()
                    },
                    dim,
                    center,
                ));
                self.selected_overlay_id = Some(id);
            }
            if ui.small_button("+ Weather").clicked() {
                let id = self.overlay_scene.add_weather(WeatherItem::new(
                    WeatherData::default(),
                    dim,
                    center,
                ));
                self.selected_overlay_id = Some(id);
            }
            let areas =
                collect_story_areas(&self.overlay_scene, EditorSize::new(dim.width, dim.height));
            let layers = self.overlay_scene.active_items_by_z().len();
            if layers > 0 {
                let visible_now = self.visible_overlays_at_playhead().len();
                ui.label(
                    egui::RichText::new(format!(
                        "Overlays: {layers} ({visible_now} active · Areas: {})",
                        areas.total_count()
                    ))
                    .size(11.0)
                    .color(colors.sub_text_fg),
                );
            }
        });
        self.paint_overlay_visibility_controls(ui, colors);

        if let Some(ref msg) = self.status_message {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(msg)
                    .size(11.5)
                    .color(colors.attention_fg),
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let apply_btn_w = 96.0;
                let (r, resp) =
                    ui.allocate_exact_size(Vec2::new(apply_btn_w, 30.0), Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Apply Edit"));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter()
                    .rect_filled(r, CornerRadius::same(6), colors.accent);
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "Apply Edit",
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
                if resp.clicked() {
                    *outcome = Some(Some(self.finalize_outcome()));
                }
                ui.add_space(8.0);
                if ui.button("Cancel").clicked() {
                    *outcome = Some(None);
                }
            });
        });
    }

    fn paint_volume_controls(&mut self, ui: &mut Ui, colors: &HistoryColors) {
        ui.label(
            egui::RichText::new("Volume:")
                .size(12.0)
                .color(colors.sub_text_fg),
        );
        ui.spacing_mut().slider_width = 96.0;
        let mut vol = self.model.audio.volume;
        if ui
            .add(
                egui::Slider::new(&mut vol, 0.0..=1.0)
                    .fixed_decimals(2)
                    .show_value(false),
            )
            .changed()
        {
            self.model.set_audio_volume(vol);
        }
        ui.label(
            egui::RichText::new(format!("{}%", (self.model.audio.volume * 100.0).round()))
                .size(11.5)
                .color(colors.sub_text_fg),
        );
    }

    fn paint_multi_clip_timeline(&mut self, ui: &mut Ui, colors: &HistoryColors) {
        let total_ms = self.clip_player.total_duration_ms().max(1);
        let playhead_ms = self.clip_player.global_position_ms().clamp(0, total_ms);
        let spans = self.clip_player.timeline().spans();
        let clip_count = spans.len();
        self.selected_clip_index = self.selected_clip_index.min(clip_count.saturating_sub(1));
        let active_clip_idx = self
            .clip_player
            .current_position()
            .map_or(0, |p| p.clip_index);

        // Header + transport readout row
        ui.horizontal_wrapped(|ui| {
            let play_label = if self.clip_player.is_playing() {
                "Pause"
            } else {
                "Play"
            };
            if ui.small_button(play_label).clicked() {
                self.toggle_playback();
            }
            ui.label(
                egui::RichText::new(format!(
                    "{} / {} · Clip {}/{}",
                    format_stamp(playhead_ms, 1),
                    format_stamp(total_ms, 1),
                    active_clip_idx + 1,
                    clip_count.max(1),
                ))
                .size(11.5)
                .color(colors.text_fg),
            );
        });

        // Multi-clip proportional strip (`st::videoTimelineTrimStripHeight`, `st::videoTimelineRadius`)
        let strip_h = st::video_timeline_trim_strip_height.0 as f32;
        let radius = CornerRadius::same(st::video_timeline_radius.0 as u8);
        let bar_w = ui.available_width().max(180.0);
        let (strip, resp) =
            ui.allocate_exact_size(Vec2::new(bar_w, strip_h), Sense::click_and_drag());
        resp.widget_info(|| WidgetInfo::labeled(Role::Slider, true, "Multi-clip video timeline"));
        ui.painter()
            .rect_filled(strip, radius, Color32::from_rgb(0x16, 0x1C, 0x24));

        // Scrubbing & clicking on the multi-clip track seeks `MultiClipSegmentPlayer` and selects the hit clip.
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let frac = f64::from(((pos.x - strip.left()) / strip.width().max(1.0)).clamp(0.0, 1.0));
            let target_ms = (frac * total_ms as f64).round() as i64;
            if let Some(resolved) = self.clip_player.seek_global(target_ms) {
                self.selected_clip_index = resolved.clip_index;
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        // Paint each clip segment proportionally across `strip`.
        let palette_tints = [
            Color32::from_rgb(0x2B, 0x5B, 0x84),
            Color32::from_rgb(0x3B, 0x6E, 0x62),
            Color32::from_rgb(0x6C, 0x4E, 0x8A),
            Color32::from_rgb(0x88, 0x5B, 0x3A),
        ];
        for span in &spans {
            let x0 = strip.left() + (span.global_start_ms as f32 / total_ms as f32) * strip.width();
            let x1 = strip.left() + (span.global_end_ms as f32 / total_ms as f32) * strip.width();
            let seg = Rect::from_min_max(
                Pos2::new(x0 + 1.0, strip.top() + 2.0),
                Pos2::new((x1 - 1.0).max(x0 + 4.0), strip.bottom() - 2.0),
            );
            let clip_muted = self
                .clip_player
                .timeline()
                .get(span.clip_index)
                .is_some_and(tg_editor::video::VideoClip::is_muted);
            let base_col = palette_tints[span.clip_index % palette_tints.len()];
            let fill = if clip_muted {
                Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), 130)
            } else {
                base_col
            };
            ui.painter().rect_filled(seg, CornerRadius::same(4), fill);
            if span.clip_index == self.selected_clip_index {
                ui.painter().rect_stroke(
                    seg,
                    CornerRadius::same(4),
                    Stroke::new(2.0, colors.accent),
                    egui::StrokeKind::Inside,
                );
            }
            if seg.width() >= 34.0 {
                let lbl = if seg.width() >= 76.0 {
                    format!(
                        "#{} {}{}",
                        span.clip_index + 1,
                        format_stamp(span.duration_ms(), 1),
                        if clip_muted { " M" } else { "" }
                    )
                } else {
                    format!("#{}", span.clip_index + 1)
                };
                ui.painter().text(
                    seg.center(),
                    Align2::CENTER_CENTER,
                    lbl,
                    FontId::proportional(10.5),
                    Color32::WHITE,
                );
            }
        }

        // Global playhead line (`st::videoTimelinePlayheadWidth`).
        let playhead_frac =
            (self.clip_player.global_position_ms() as f32 / total_ms as f32).clamp(0.0, 1.0);
        let px = strip.left() + playhead_frac * strip.width();
        ui.painter().line_segment(
            [
                Pos2::new(px, strip.top() - 1.0),
                Pos2::new(px, strip.bottom() + 1.0),
            ],
            Stroke::new(st::video_timeline_playhead_width.0 as f32, Color32::WHITE),
        );

        // Clip editing toolbar (`+ Clip`, `Split`, `Move Left`, `Move Right`, `Mute Clip`, `Remove Clip`).
        ui.add_space(4.0);
        let selected_muted = self
            .clip_player
            .timeline()
            .get(self.selected_clip_index)
            .is_some_and(tg_editor::video::VideoClip::is_muted);
        let can_split = {
            let pos = self.clip_player.global_position_ms();
            spans
                .iter()
                .any(|s| pos > s.global_start_ms && pos < s.global_end_ms)
        };
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("+ Clip").clicked() {
                let _ = self.append_sample_clip(6_000);
            }
            if ui
                .add_enabled(can_split, egui::Button::new("Split").small())
                .on_hover_text("Split clip at current playhead")
                .clicked()
            {
                let _ = self.split_at_playhead();
            }
            if ui
                .add_enabled(
                    self.selected_clip_index > 0,
                    egui::Button::new("Move Left").small(),
                )
                .clicked()
            {
                let _ = self.move_selected_clip(-1);
            }
            if ui
                .add_enabled(
                    self.selected_clip_index + 1 < clip_count,
                    egui::Button::new("Move Right").small(),
                )
                .clicked()
            {
                let _ = self.move_selected_clip(1);
            }
            let mute_lbl = if selected_muted {
                "Unmute Clip"
            } else {
                "Mute Clip"
            };
            if ui.small_button(mute_lbl).clicked() {
                let _ = self.set_selected_clip_muted(!selected_muted);
            }
            if ui
                .add_enabled(clip_count > 1, egui::Button::new("Remove Clip").small())
                .clicked()
            {
                let _ = self.remove_selected_clip();
            }
        });
    }

    fn paint_overlay_visibility_controls(&mut self, ui: &mut Ui, colors: &HistoryColors) {
        let entries: Vec<ItemId> = self
            .overlay_scene
            .active_items_by_z()
            .into_iter()
            .map(|e| e.id)
            .collect();
        if entries.is_empty() {
            return;
        }
        let selected = self
            .selected_overlay_id
            .filter(|id| entries.contains(id))
            .unwrap_or(entries[entries.len() - 1]);
        self.selected_overlay_id = Some(selected);

        let total_ms = self.clip_player.total_duration_ms().max(1_000);
        let range = self.model.timeline.overlay_visibility().range_for(selected);
        let range_label = match range {
            Some(r) => format!(
                "{} – {}",
                format_stamp(r.start_ms, 1),
                format_stamp(r.end_ms, 1)
            ),
            None => "Full video".to_owned(),
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("Overlay #{} timing: {range_label}", selected.0))
                    .size(11.0)
                    .color(colors.sub_text_fg),
            );
            if ui.small_button("First Half").clicked() {
                let mid = (total_ms / 2).max(100);
                let _ = self.set_overlay_visibility(selected, 0, mid);
            }
            if ui.small_button("Second Half").clicked() {
                let mid = (total_ms / 2).max(100);
                let _ = self.set_overlay_visibility(selected, mid, total_ms);
            }
            if ui.small_button("Full Span").clicked() {
                let _ = self.clear_overlay_visibility(selected);
            }
        });
    }

    fn paint_trim_timeline(&mut self, ui: &mut Ui, colors: &HistoryColors) {
        let strip_h = 46.0;
        let total_w = ui.available_width().max(200.0);
        let handle_w = 10.0;
        let (outer, resp) =
            ui.allocate_exact_size(Vec2::new(total_w, strip_h + 18.0), Sense::click_and_drag());
        let strip = Rect::from_min_max(
            Pos2::new(outer.left() + handle_w, outer.top() + 14.0),
            Pos2::new(outer.right() - handle_w, outer.top() + 14.0 + strip_h),
        );

        // Background + frame thumbnails
        ui.painter().rect_filled(
            strip,
            CornerRadius::same(6),
            Color32::from_rgb(0x1A, 0x20, 0x28),
        );
        if !self.thumb_textures.is_empty() {
            let count = self.thumb_textures.len();
            let slot_w = strip.width() / count as f32;
            for (i, tex) in self.thumb_textures.iter().enumerate() {
                let slot = Rect::from_min_size(
                    Pos2::new(strip.left() + i as f32 * slot_w, strip.top()),
                    Vec2::new(slot_w.ceil(), strip.height()),
                );
                ui.painter().image(
                    tex.id(),
                    slot,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }

        let from_frac = self
            .model
            .timeline
            .fraction_at_time(self.model.timeline.from_ms()) as f32;
        let till_frac = self
            .model
            .timeline
            .fraction_at_time(self.model.timeline.till_ms()) as f32;
        let cover_frac = self
            .model
            .timeline
            .fraction_at_time(self.model.timeline.cover_ms()) as f32;

        let x_left = strip.left() + from_frac.clamp(0.0, 1.0) * strip.width();
        let x_right = strip.left() + till_frac.clamp(0.0, 1.0) * strip.width();
        let x_cover = strip.left() + cover_frac.clamp(0.0, 1.0) * strip.width();

        // Pointer drag handling for left handle, right handle, or cover playhead
        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let d_left = (pos.x - x_left).abs();
            let d_right = (pos.x - x_right).abs();
            let d_cover = (pos.x - x_cover).abs();
            self.drag_target = if d_left <= 14.0 && d_left <= d_right {
                DragTarget::LeftHandle
            } else if d_right <= 14.0 {
                DragTarget::RightHandle
            } else if d_cover <= 14.0 || (pos.x >= x_left && pos.x <= x_right) {
                DragTarget::CoverHead
            } else if pos.x < x_left {
                DragTarget::LeftHandle
            } else {
                DragTarget::RightHandle
            };
        }
        if (resp.dragged() || resp.clicked())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let frac = f64::from(((pos.x - strip.left()) / strip.width().max(1.0)).clamp(0.0, 1.0));
            let t_ms = self.model.timeline.time_at_fraction(frac);
            match self.drag_target {
                DragTarget::LeftHandle => {
                    self.model.timeline.set_from(t_ms);
                    self.range_input = self.model.timeline.duration_label_text();
                }
                DragTarget::RightHandle => {
                    self.model.timeline.set_till(t_ms);
                    self.range_input = self.model.timeline.duration_label_text();
                }
                DragTarget::CoverHead | DragTarget::None => {
                    self.model.timeline.set_cover(t_ms);
                }
            }
        }
        if resp.drag_stopped() {
            self.drag_target = DragTarget::None;
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }

        // Recompute positions after drag update
        let x_left = strip.left()
            + (self
                .model
                .timeline
                .fraction_at_time(self.model.timeline.from_ms()) as f32)
                .clamp(0.0, 1.0)
                * strip.width();
        let x_right = strip.left()
            + (self
                .model
                .timeline
                .fraction_at_time(self.model.timeline.till_ms()) as f32)
                .clamp(0.0, 1.0)
                * strip.width();
        let x_cover = strip.left()
            + (self
                .model
                .timeline
                .fraction_at_time(self.model.timeline.cover_ms()) as f32)
                .clamp(0.0, 1.0)
                * strip.width();

        // Darken trimmed-out regions
        let dim = Color32::from_rgba_unmultiplied(0, 0, 0, 150);
        if x_left > strip.left() {
            ui.painter().rect_filled(
                Rect::from_min_max(strip.min, Pos2::new(x_left, strip.bottom())),
                CornerRadius::ZERO,
                dim,
            );
        }
        if x_right < strip.right() {
            ui.painter().rect_filled(
                Rect::from_min_max(Pos2::new(x_right, strip.top()), strip.max),
                CornerRadius::ZERO,
                dim,
            );
        }

        // Active selection border + Left/Right trim handles
        let sel_rect = Rect::from_min_max(
            Pos2::new(x_left, strip.top()),
            Pos2::new(x_right.max(x_left + 4.0), strip.bottom()),
        );
        ui.painter().rect_stroke(
            sel_rect,
            CornerRadius::same(4),
            Stroke::new(2.0, colors.accent),
            egui::StrokeKind::Inside,
        );
        let left_handle = Rect::from_min_max(
            Pos2::new(x_left - handle_w * 0.5, strip.top()),
            Pos2::new(x_left + handle_w * 0.5, strip.bottom()),
        );
        let right_handle = Rect::from_min_max(
            Pos2::new(x_right - handle_w * 0.5, strip.top()),
            Pos2::new(x_right + handle_w * 0.5, strip.bottom()),
        );
        ui.painter()
            .rect_filled(left_handle, CornerRadius::same(3), colors.accent);
        ui.painter()
            .rect_filled(right_handle, CornerRadius::same(3), colors.accent);

        // Cover-frame playhead line + dot above strip (`VideoTimeline::paintOverlay`)
        ui.painter().line_segment(
            [
                Pos2::new(x_cover, strip.top() - 3.0),
                Pos2::new(x_cover, strip.bottom() + 2.0),
            ],
            Stroke::new(2.2, Color32::WHITE),
        );
        ui.painter()
            .circle_filled(Pos2::new(x_cover, strip.top() - 7.0), 4.5, colors.accent);
        ui.painter().circle_stroke(
            Pos2::new(x_cover, strip.top() - 7.0),
            4.5,
            Stroke::new(1.2, Color32::WHITE),
        );
    }

    fn paint_audio_track_timeline(&mut self, ui: &mut Ui, colors: &HistoryColors) {
        let clip_span = (self.model.timeline.till_ms() - self.model.timeline.from_ms()).max(1_000);
        if self.audio_timeline.max_clip_duration_ms != Some(clip_span) {
            self.audio_timeline.set_max_clip_duration(Some(clip_span));
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Audio Track:")
                    .size(12.0)
                    .color(colors.text_fg),
            );
            if self.audio_timeline.track.is_none() {
                if ui.small_button("+ Attach Soundtrack").clicked() {
                    let clip = AudioTrackClip::new(
                        "Background Track",
                        "Sample Audio",
                        (clip_span * 2).max(30_000),
                    );
                    self.audio_timeline.set_track(clip);
                }
            } else if ui.small_button("Remove Track").clicked() {
                self.audio_timeline.clear_track();
            }
        });

        let Some(track_snap) = self.audio_timeline.track.clone() else {
            return;
        };
        let bar_w = ui.available_width().max(180.0);
        let (strip, resp) = ui.allocate_exact_size(Vec2::new(bar_w, 34.0), Sense::click_and_drag());
        ui.painter().rect_filled(
            strip,
            CornerRadius::same(6),
            Color32::from_rgb(0x18, 0x22, 0x2E),
        );

        let bars = self.audio_timeline.normalized_waveform_bars(40);
        let slot = strip.width() / bars.len().max(1) as f32;
        let from_f = self
            .audio_timeline
            .fraction_at_time(track_snap.trim_from_ms) as f32;
        let till_f = self
            .audio_timeline
            .fraction_at_time(track_snap.trim_till_ms) as f32;

        for (i, &amp) in bars.iter().enumerate() {
            let frac = (i as f32 + 0.5) / bars.len().max(1) as f32;
            let in_sel = frac >= from_f && frac <= till_f;
            let bh = (amp * (strip.height() - 10.0)).max(3.0);
            let x = strip.left() + i as f32 * slot + slot * 0.25;
            let rect = Rect::from_min_size(
                Pos2::new(x, strip.center().y - bh * 0.5),
                Vec2::new((slot * 0.55).max(1.5), bh),
            );
            let col = if in_sel && !track_snap.muted {
                colors.accent
            } else {
                Color32::from_rgba_unmultiplied(160, 175, 190, 95)
            };
            ui.painter().rect_filled(rect, CornerRadius::same(1), col);
        }

        // Dragging on the waveform strip slides the trimmed audio window.
        if (resp.dragged() || resp.clicked())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let frac = f64::from(((pos.x - strip.left()) / strip.width().max(1.0)).clamp(0.0, 1.0));
            let target_ms = self.audio_timeline.time_at_fraction(frac);
            self.audio_timeline.slide_window_to(target_ms);
        }

        let sel_rect = Rect::from_min_max(
            Pos2::new(strip.left() + from_f * strip.width(), strip.top()),
            Pos2::new(strip.left() + till_f * strip.width(), strip.bottom()),
        );
        ui.painter().rect_stroke(
            sel_rect,
            CornerRadius::same(4),
            Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );

        ui.horizontal(|ui| {
            let mut muted = track_snap.muted;
            if ui.checkbox(&mut muted, "Mute track").changed() {
                self.audio_timeline.set_muted(muted);
            }
            let mut vol = track_snap.volume;
            if ui
                .add(
                    egui::Slider::new(&mut vol, 0.0..=1.0)
                        .fixed_decimals(2)
                        .show_value(false),
                )
                .changed()
            {
                self.audio_timeline.set_volume(vol);
            }
            ui.label(
                egui::RichText::new(format!(
                    "{} ({}–{})",
                    track_snap.title,
                    format_stamp(track_snap.trim_from_ms, 1),
                    format_stamp(track_snap.trim_till_ms, 1),
                ))
                .size(11.0)
                .color(colors.sub_text_fg),
            );
        });
    }
}

fn procedural_video_frame(width: u32, height: u32, time_frac: f32, seed: u8) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(width, height);
    let shift = time_frac * 95.0 + f32::from(seed % 60);
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / width.max(1) as f32;
            let fy = y as f32 / height.max(1) as f32;
            let r = (40.0 + fx * 120.0 + shift * 0.8).clamp(0.0, 255.0) as u8;
            let g = (75.0 + fy * 110.0 + (1.0 - time_frac) * 50.0).clamp(0.0, 255.0) as u8;
            let b = (120.0 + (1.0 - fx) * 90.0 + time_frac * 45.0).clamp(0.0, 255.0) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::compose_boxes::{AttachFilesAction, AttachFilesBoxState};

    #[test]
    fn send_files_box_opens_video_editor_and_records_modifications() {
        let mut box_state = AttachFilesBoxState::new(false, false);
        box_state.add_sample_video("demo_clip.mp4", 1920, 1080, 3_500_000);
        let idx = box_state
            .prepared
            .files
            .iter()
            .position(|f| f.path == "demo_clip.mp4")
            .expect("video added to prepared list");

        let mut modal = VideoEditorModalState::open_for_file(
            idx,
            "demo_clip.mp4",
            1920,
            1080,
            10_000,
            3_500_000,
            None,
            7,
        );
        modal.model.timeline.set_trim(1_500, 6_500);
        modal.model.timeline.set_cover(3_000);
        modal.model.quality.set_shorter_side(720);
        modal.model.set_gif(true);

        let outcome = modal.finalize_outcome();
        assert_eq!(outcome.modifications.from_ms, 1_500);
        assert_eq!(outcome.modifications.till_ms, 6_500);
        assert_eq!(outcome.modifications.cover_ms, 3_000);
        assert_eq!(outcome.modifications.quality, 720);
        assert!(outcome.modifications.gif);

        box_state
            .edited_videos
            .insert("demo_clip.mp4".to_owned(), outcome.modifications);
        match box_state.build_action_for_send() {
            AttachFilesAction::Send(sub) => {
                let saved = sub
                    .edited_videos
                    .get("demo_clip.mp4")
                    .expect("edited video modifications included in submission");
                assert_eq!(saved.from_ms, 1_500);
                assert_eq!(saved.till_ms, 6_500);
                assert!(saved.gif);
            }
            other => panic!("expected AttachFilesAction::Send, got {other:?}"),
        }
    }

    #[test]
    fn video_editor_supports_audio_track_timeline_and_scene_overlays() {
        let mut modal = VideoEditorModalState::open_for_file(
            0,
            "story_clip.mp4",
            1080,
            1920,
            15_000,
            4_000_000,
            None,
            3,
        );
        let clip = AudioTrackClip::new("Synth Theme", "Demo Artist", 45_000);
        modal.audio_timeline.set_track(clip);
        assert!(modal.audio_timeline.track.is_some());
        modal.audio_timeline.slide_window_to(10_000);
        let track = modal.audio_timeline.track.as_ref().expect("track");
        assert_eq!(track.trim_from_ms, 10_000);
        assert_eq!(track.selected_duration_ms(), 15_000);

        modal.overlay_scene.add_weather(WeatherItem::new(
            WeatherData::default(),
            modal.model.source_info.dimensions,
            PointF::new(540.0, 400.0),
        ));
        let areas = collect_story_areas(&modal.overlay_scene, modal.model.source_info.dimensions);
        assert_eq!(areas.weather.len(), 1);
    }

    #[test]
    fn d3_video_editor_export_uses_account_temp_dir_and_cleans_up() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let acc_edit_dir = dir.path().join("temp_data").join("video_edit");

        set_active_video_editor_temp_dir(Some(acc_edit_dir.clone()));
        let modal =
            VideoEditorModalState::open_for_file(0, "clip.mp4", 1280, 720, 5_000, 500_000, None, 1);
        assert_eq!(
            modal.resolve_export_temp_dir(),
            acc_edit_dir,
            "video editor temp export must use account temp folder, not %TEMP%"
        );

        std::fs::create_dir_all(&acc_edit_dir).expect("mkdir");
        let stale = acc_edit_dir.join("video_edit_0_0_1.mp4");
        std::fs::write(&stale, b"stale export").expect("write");
        cleanup_video_editor_temp_dir(&acc_edit_dir);
        assert!(
            !stale.exists(),
            "cleanup must delete stale video editor temp MP4s"
        );

        set_active_video_editor_temp_dir(None);
    }
}
