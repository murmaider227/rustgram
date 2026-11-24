//! Story photo decoding (`tg-media`) and video playback (`tg-video::StreamingPlayer`).
//!
//! Port origin: `Telegram/SourceFiles/media/stories/media_stories_sibling.cpp` and
//! `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp` (story media loaders).

use std::collections::HashMap;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use tg_data::FullStoryId;
use tg_media::image::prepare::blur;
use tg_media::image::{ReadArgs, read_image};
use tg_stories::{PlaybackStatus, TrackState};
use tg_video::{
    DURATION_UNAVAILABLE, FfmpegStreamDecoder, FrameData, FramePopOutcome, OutputPixelFormat,
    PlaybackMode, PlaybackOptions, PlayerStage, StreamingPlayer, VideoFrame,
};

use crate::stories::backend::StoryMediaAsset;

/// Decoded RGBA image kept in CPU memory until uploaded to an `egui` texture.
#[derive(Clone, Debug)]
pub struct DecodedRgba {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Tightly-packed RGBA8 bytes (`width * height * 4`).
    pub rgba: Vec<u8>,
}

impl DecodedRgba {
    /// Converts this pixel buffer into an [`egui::ColorImage`].
    #[must_use]
    pub fn to_color_image(&self) -> ColorImage {
        ColorImage::from_rgba_unmultiplied([self.width, self.height], &self.rgba)
    }
}

/// Decodes a photo story asset through `tg_media::image::read_image` and builds a blurred
/// thumbnail via `tg_media::image::prepare::blur` for sibling previews.
pub fn decode_photo_via_tg_media(bytes: &[u8]) -> Result<(DecodedRgba, DecodedRgba), String> {
    let args = ReadArgs {
        content: bytes.to_vec(),
        path: None,
        max_size: Some((1080, 1920)),
        force_opaque: true,
        return_content: false,
    };
    let res = read_image(args).map_err(|e| format!("tg-media decode failed: {e}"))?;
    let width = res.image.width() as usize;
    let height = res.image.height() as usize;
    let full = DecodedRgba {
        width,
        height,
        rgba: res.image.as_raw().clone(),
    };

    // Downscale and apply tdesktop's radius-3 triangle blur for blurred sibling/preload fallback.
    let small = image::imageops::resize(
        &res.image,
        (res.image.width() / 4).max(16),
        (res.image.height() / 4).max(16),
        image::imageops::FilterType::Triangle,
    );
    let blurred_img = blur(small, true);
    let blurred = DecodedRgba {
        width: blurred_img.width() as usize,
        height: blurred_img.height() as usize,
        rgba: blurred_img.into_raw(),
    };
    Ok((full, blurred))
}

/// Active video story playback controller backed by `tg_video::StreamingPlayer`.
pub struct StoryVideoPlayback {
    story_id: FullStoryId,
    player: StreamingPlayer<FfmpegStreamDecoder>,
    texture: Option<TextureHandle>,
    last_frame_pts_ms: Option<i64>,
    duration_ms: i64,
    muted: bool,
    volume: f64,
    paused: bool,
}

impl StoryVideoPlayback {
    /// Opens a video story from raw bytes using `FfmpegStreamDecoder` + `StreamingPlayer`.
    pub fn from_bytes(
        story_id: FullStoryId,
        bytes: Vec<u8>,
        silent: bool,
        volume: f64,
        muted: bool,
        now_ms: i64,
    ) -> Result<Self, String> {
        let options = video_playback_options(silent, volume, muted);
        let decoder = FfmpegStreamDecoder::from_bytes(bytes, options.clone())
            .map_err(|e| format!("tg-video open bytes failed: {e}"))?;
        Self::from_decoder(story_id, decoder, options, volume, muted, now_ms)
    }

    /// Opens a video story from a local file path using `FfmpegStreamDecoder` + `StreamingPlayer`.
    pub fn from_path(
        story_id: FullStoryId,
        path: &std::path::Path,
        silent: bool,
        volume: f64,
        muted: bool,
        now_ms: i64,
    ) -> Result<Self, String> {
        let options = video_playback_options(silent, volume, muted);
        let decoder = FfmpegStreamDecoder::from_file(path, options.clone())
            .map_err(|e| format!("tg-video open file failed: {e}"))?;
        Self::from_decoder(story_id, decoder, options, volume, muted, now_ms)
    }

    fn from_decoder(
        story_id: FullStoryId,
        decoder: FfmpegStreamDecoder,
        options: PlaybackOptions,
        volume: f64,
        muted: bool,
        now_ms: i64,
    ) -> Result<Self, String> {
        let mut player = StreamingPlayer::new(decoder, options)
            .map_err(|e| format!("StreamingPlayer::new failed: {e}"))?;
        let raw_dur = player.total_duration_ms();
        let duration_ms = if raw_dur > 0 && raw_dur != DURATION_UNAVAILABLE {
            raw_dur
        } else {
            5_000
        };
        player.start(now_ms);
        let _ = player.fill_buffers();
        Ok(Self {
            story_id,
            player,
            texture: None,
            last_frame_pts_ms: None,
            duration_ms,
            muted,
            volume,
            paused: false,
        })
    }

    /// Returns the story ID this video player belongs to.
    #[must_use]
    pub const fn story_id(&self) -> FullStoryId {
        self.story_id
    }

    /// Returns the decoded cover or latest video frame, if available.
    #[must_use]
    pub fn current_frame(&self) -> Option<&VideoFrame> {
        self.player.current_video_frame().or_else(|| {
            self.player
                .info()
                .video
                .as_ref()
                .and_then(|v| v.cover.as_ref())
        })
    }

    /// Returns total video duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    /// Returns whether audio is currently muted.
    #[must_use]
    pub const fn is_muted(&self) -> bool {
        self.muted
    }

    /// Returns current audio volume in `[0.0, 1.0]`.
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.volume
    }

    /// Sets pause/resume state on the underlying [`StreamingPlayer`].
    pub fn set_paused(&mut self, paused: bool, now_ms: i64) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if paused {
            self.player.pause(now_ms);
        } else {
            self.player.resume(now_ms);
        }
    }

    /// Updates volume and mute flag.
    pub fn set_volume(&mut self, volume: f64, muted: bool) {
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = muted;
        let effective = if muted { 0.0 } else { self.volume };
        let _ = self.player.set_volume(effective);
    }

    /// Advances the `StreamingPlayer` to `now_ms`, refreshes the optional `egui` texture,
    /// and returns a [`tg_stories::TrackState`] snapshot for [`tg_stories::StoryViewerController`].
    pub fn step(&mut self, ctx: Option<&Context>, now_ms: i64) -> TrackState {
        let outcome = self.player.step_video(now_ms);
        if let Ok(FramePopOutcome::Ready { frame, .. }) = outcome {
            if let Some(egui_ctx) = ctx {
                self.upload_frame(egui_ctx, &frame);
            }
            self.last_frame_pts_ms = Some(frame.pts_ms);
        } else if self.texture.is_none()
            && let Some(egui_ctx) = ctx
            && let Some(cover) = self.current_frame().cloned()
        {
            self.upload_frame(egui_ctx, &cover);
        }

        // Drain audio queue so buffers don't grow unbounded when audio output isn't attached.
        while self.player.pop_audio_frame().is_some() {}

        let pos = self.player.position_ms(now_ms).clamp(0, self.duration_ms);
        let status = match self.player.stage() {
            PlayerStage::Finished => PlaybackStatus::StoppedAtEnd,
            PlayerStage::Failed | PlayerStage::Uninitialized => PlaybackStatus::Stopped,
            PlayerStage::Ready | PlayerStage::Started => {
                if self.player.is_paused() {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                }
            }
        };

        TrackState {
            status,
            position: if status == PlaybackStatus::StoppedAtEnd {
                self.duration_ms
            } else {
                pos
            },
            length: self.duration_ms,
            frequency: 1_000,
        }
    }

    /// Returns the current uploaded video frame texture, if ready.
    #[must_use]
    pub fn texture(&self) -> Option<&TextureHandle> {
        self.texture.as_ref()
    }

    fn upload_frame(&mut self, ctx: &Context, frame: &VideoFrame) {
        if let Some(image) = video_frame_to_color_image(frame) {
            if let Some(tex) = &mut self.texture {
                tex.set(image, TextureOptions::LINEAR);
            } else {
                self.texture = Some(ctx.load_texture(
                    format!(
                        "story_video_{}_{}",
                        self.story_id.peer.raw(),
                        self.story_id.story
                    ),
                    image,
                    TextureOptions::LINEAR,
                ));
            }
        }
    }
}

fn video_playback_options(silent: bool, volume: f64, muted: bool) -> PlaybackOptions {
    PlaybackOptions {
        mode: if silent {
            PlaybackMode::Video
        } else {
            PlaybackMode::Both
        },
        volume: if muted { 0.0 } else { volume.clamp(0.0, 1.0) },
        sync_video_by_audio: false,
        output_format: OutputPixelFormat::Rgba,
        apply_rotation: true,
        loop_playback: false,
        ..PlaybackOptions::default()
    }
}

/// Converts a decoded `tg_video::VideoFrame` into an [`egui::ColorImage`].
#[must_use]
pub fn video_frame_to_color_image(frame: &VideoFrame) -> Option<ColorImage> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    match &frame.data {
        FrameData::Rgba(rgba) if rgba.len() >= w * h * 4 => Some(
            ColorImage::from_rgba_unmultiplied([w, h], &rgba[..w * h * 4]),
        ),
        FrameData::Bgra(bgra) if bgra.len() >= w * h * 4 => {
            let mut rgba = Vec::with_capacity(w * h * 4);
            for &[b, g, r, a] in bgra[..w * h * 4].as_chunks::<4>().0 {
                rgba.extend_from_slice(&[r, g, b, a]);
            }
            Some(ColorImage::from_rgba_unmultiplied([w, h], &rgba))
        }
        _ => None,
    }
}

/// Texture and video-player cache shared across the story viewer and sibling previews.
#[derive(Default)]
pub struct StoryMediaCache {
    photos: HashMap<FullStoryId, TextureHandle>,
    blurred_photos: HashMap<FullStoryId, TextureHandle>,
    video_covers: HashMap<FullStoryId, TextureHandle>,
    active_video: Option<StoryVideoPlayback>,
    failed_ids: HashMap<FullStoryId, String>,
}

impl StoryMediaCache {
    /// Creates an empty [`StoryMediaCache`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensures a photo texture (and blurred sibling preview) is decoded via `tg-media`
    /// and uploaded to `ctx`.
    pub fn ensure_photo(
        &mut self,
        ctx: &Context,
        story_id: FullStoryId,
        asset: &StoryMediaAsset,
    ) -> Option<&TextureHandle> {
        if self.photos.contains_key(&story_id) {
            return self.photos.get(&story_id);
        }
        if self.failed_ids.contains_key(&story_id) {
            return None;
        }
        let StoryMediaAsset::PhotoBytes(bytes) = asset else {
            return None;
        };
        match decode_photo_via_tg_media(bytes) {
            Ok((full, blurred)) => {
                let full_tex = ctx.load_texture(
                    format!("story_photo_{}_{}", story_id.peer.raw(), story_id.story),
                    full.to_color_image(),
                    TextureOptions::LINEAR,
                );
                let blur_tex = ctx.load_texture(
                    format!("story_blur_{}_{}", story_id.peer.raw(), story_id.story),
                    blurred.to_color_image(),
                    TextureOptions::LINEAR,
                );
                self.photos.insert(story_id, full_tex);
                self.blurred_photos.insert(story_id, blur_tex);
                self.photos.get(&story_id)
            }
            Err(err) => {
                self.failed_ids.insert(story_id, err);
                None
            }
        }
    }

    /// Returns a cached photo texture (or video cover frame) for sibling previews.
    #[must_use]
    pub fn preview_texture(&self, story_id: FullStoryId) -> Option<&TextureHandle> {
        self.photos
            .get(&story_id)
            .or_else(|| self.video_covers.get(&story_id))
            .or_else(|| self.blurred_photos.get(&story_id))
    }

    /// Ensures the active video player matches `story_id`, opening via `tg-video::StreamingPlayer`.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_video(
        &mut self,
        ctx: Option<&Context>,
        story_id: FullStoryId,
        asset: &StoryMediaAsset,
        silent: bool,
        volume: f64,
        muted: bool,
        now_ms: i64,
    ) -> Option<&mut StoryVideoPlayback> {
        if self
            .active_video
            .as_ref()
            .is_some_and(|v| v.story_id() == story_id)
        {
            return self.active_video.as_mut();
        }
        if self.failed_ids.contains_key(&story_id) {
            return None;
        }
        let created = match asset {
            StoryMediaAsset::VideoBytes(bytes) => StoryVideoPlayback::from_bytes(
                story_id,
                bytes.clone(),
                silent,
                volume,
                muted,
                now_ms,
            ),
            StoryMediaAsset::VideoPath(path) => {
                StoryVideoPlayback::from_path(story_id, path, silent, volume, muted, now_ms)
            }
            StoryMediaAsset::PhotoBytes(_) => return None,
        };
        match created {
            Ok(player) => {
                if let Some(egui_ctx) = ctx
                    && let Some(cover) = player.current_frame()
                    && let Some(img) = video_frame_to_color_image(cover)
                {
                    let tex = egui_ctx.load_texture(
                        format!("story_vcover_{}_{}", story_id.peer.raw(), story_id.story),
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.video_covers.insert(story_id, tex);
                }
                self.active_video = Some(player);
                self.active_video.as_mut()
            }
            Err(err) => {
                self.failed_ids.insert(story_id, err);
                None
            }
        }
    }

    /// Returns a mutable reference to the active video player if it matches `story_id`.
    pub fn active_video_mut(&mut self, story_id: FullStoryId) -> Option<&mut StoryVideoPlayback> {
        self.active_video
            .as_mut()
            .filter(|v| v.story_id() == story_id)
    }

    /// Returns the active video texture handle if the active video matches `story_id`.
    #[must_use]
    pub fn active_video_texture(&self, story_id: FullStoryId) -> Option<&TextureHandle> {
        self.active_video
            .as_ref()
            .filter(|v| v.story_id() == story_id)
            .and_then(StoryVideoPlayback::texture)
    }

    /// Stops and drops the active video player (e.g., when switching to a photo or closing viewer).
    pub fn clear_active_video(&mut self) {
        self.active_video = None;
    }

    /// Returns the last decode error recorded for `story_id`, if any.
    #[must_use]
    pub fn last_error(&self, story_id: FullStoryId) -> Option<&str> {
        self.failed_ids.get(&story_id).map(String::as_str)
    }
}
