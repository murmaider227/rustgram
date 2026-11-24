//! Animated video userpic player and cache (`dialogs/video_userpic.rs` core model).

use std::collections::HashMap;

use egui::{ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions, Ui};

use crate::stand_in::types::PeerId;

/// Default frame duration in milliseconds when a decoded video frame has no explicit timing.
pub const DEFAULT_VIDEO_USERPIC_FRAME_MS: u64 = 40;

/// Single decoded RGBA frame for a dialog's animated video userpic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoUserpicFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixel buffer (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Display duration of this frame in milliseconds.
    pub duration_ms: u64,
}

impl VideoUserpicFrame {
    /// Creates a new RGBA frame, validating buffer length.
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, duration_ms: u64) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if width == 0 || height == 0 || rgba.len() != expected {
            return None;
        }
        Some(Self {
            width,
            height,
            rgba,
            duration_ms: duration_ms.max(1),
        })
    }
}

/// Playback / decode state for a peer's [`DialogVideoUserpic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoUserpicState {
    /// No video loaded yet.
    #[default]
    Idle,
    /// Video media has been requested (`videoWanted(Small)`).
    Loading,
    /// Video frames are decoded and ready to paint.
    Ready,
    /// Video decoding failed (`_video.setBad()`); falls back to static userpic.
    Bad,
}

/// Media provider abstraction matching `Data::PhotoMedia::videoContent(Small | Large)`
/// (`dialogs_video_userpic.cpp:70-84`).
pub trait VideoUserpicSource {
    /// Returns small-size video bytes if available (`Data::PhotoSize::Small`).
    fn video_bytes_small(&self, peer_id: PeerId, photo_id: i64) -> Option<Vec<u8>>;

    /// Returns large-size video bytes fallback (`Data::PhotoSize::Large`).
    fn video_bytes_large(&self, peer_id: PeerId, photo_id: i64) -> Option<Vec<u8>> {
        let _ = (peer_id, photo_id);
        None
    }

    /// Decodes video bytes into looping RGBA frames.
    fn decode_frames(&self, bytes: &[u8]) -> Result<Vec<VideoUserpicFrame>, String>;
}

/// Applies an anti-aliased circular ellipse mask (`ImageRoundRadius::Ellipse`) to an RGBA8 frame.
#[must_use]
pub fn apply_ellipse_mask(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if width == 0 || height == 0 || rgba.len() != expected {
        return Vec::new();
    }
    let mut out = rgba.to_vec();
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let rx = cx.max(1.0);
    let ry = cy.max(1.0);
    let edge = 0.5 / rx.min(ry);

    for y in 0..height {
        let dy = ((y as f32 + 0.5) - cy) / ry;
        for x in 0..width {
            let dx = ((x as f32 + 0.5) - cx) / rx;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha_mul = if dist <= 1.0 - edge {
                1.0
            } else if dist >= 1.0 + edge {
                0.0
            } else {
                ((1.0 + edge - dist) / (2.0 * edge)).clamp(0.0, 1.0)
            };
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4 + 3;
            out[idx] = (f32::from(out[idx]) * alpha_mul).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Per-peer video userpic state machine (`Dialogs::Ui::VideoUserpic`).
pub struct DialogVideoUserpic {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Last observed profile photo ID (`_videoPhotoId`).
    video_photo_id: Option<i64>,
    /// Current state (`Idle` / `Loading` / `Ready` / `Bad`).
    state: VideoUserpicState,
    /// Circularly masked RGBA frames ready for texture upload.
    frames: Vec<VideoUserpicFrame>,
    /// Total loop duration in milliseconds.
    total_duration_ms: u64,
    /// Timestamp when playback started (`startReady`).
    started_at_ms: Option<u64>,
    /// Last painted pixel diameter (`_lastSize`).
    last_size: u32,
    /// Cached egui texture for the last painted frame.
    cached_texture: Option<(usize, TextureHandle)>,
}

impl std::fmt::Debug for DialogVideoUserpic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogVideoUserpic")
            .field("peer_id", &self.peer_id)
            .field("video_photo_id", &self.video_photo_id)
            .field("state", &self.state)
            .field("frames_len", &self.frames.len())
            .field("total_duration_ms", &self.total_duration_ms)
            .field("started_at_ms", &self.started_at_ms)
            .field("last_size", &self.last_size)
            .field("has_cached_texture", &self.cached_texture.is_some())
            .finish()
    }
}

impl DialogVideoUserpic {
    /// Creates a new [`DialogVideoUserpic`] for `peer_id`.
    #[must_use]
    pub const fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            video_photo_id: None,
            state: VideoUserpicState::Idle,
            frames: Vec::new(),
            total_duration_ms: 0,
            started_at_ms: None,
            last_size: 0,
            cached_texture: None,
        }
    }

    /// Current playback/loading state.
    #[must_use]
    pub const fn state(&self) -> VideoUserpicState {
        self.state
    }

    /// Last observed profile photo ID.
    #[must_use]
    pub const fn video_photo_id(&self) -> Option<i64> {
        self.video_photo_id
    }

    /// Last requested paint diameter in pixels.
    #[must_use]
    pub const fn last_size(&self) -> u32 {
        self.last_size
    }

    /// Number of decoded frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Updates the peer's current photo ID; if it changed, resets video state & frame cache
    /// (`dialogs_video_userpic.cpp:44-58`).
    pub fn sync_photo_id(&mut self, photo_id: Option<i64>) {
        if self.video_photo_id != photo_id {
            self.video_photo_id = photo_id;
            self.state = if photo_id.is_some() {
                VideoUserpicState::Loading
            } else {
                VideoUserpicState::Idle
            };
            self.frames.clear();
            self.total_duration_ms = 0;
            self.started_at_ms = None;
            self.cached_texture = None;
        }
    }

    /// Attempts to pull and decode video bytes from `source` (`Small` falling back to `Large`).
    pub fn poll_source(&mut self, source: &dyn VideoUserpicSource) {
        if matches!(
            self.state,
            VideoUserpicState::Ready | VideoUserpicState::Bad
        ) {
            return;
        }
        let Some(photo_id) = self.video_photo_id else {
            return;
        };
        let bytes = source
            .video_bytes_small(self.peer_id, photo_id)
            .filter(|b| !b.is_empty())
            .or_else(|| {
                source
                    .video_bytes_large(self.peer_id, photo_id)
                    .filter(|b| !b.is_empty())
            });
        let Some(bytes) = bytes else {
            self.state = VideoUserpicState::Loading;
            return;
        };

        match source.decode_frames(&bytes) {
            Ok(raw_frames) if !raw_frames.is_empty() => {
                self.set_decoded_frames(raw_frames);
            }
            _ => {
                self.state = VideoUserpicState::Bad;
                self.frames.clear();
                self.total_duration_ms = 0;
            }
        }
    }

    /// Marks video loading as failed (`_video.setBad()`); the static userpic stays visible.
    pub fn mark_bad(&mut self) {
        self.state = VideoUserpicState::Bad;
        self.frames.clear();
        self.total_duration_ms = 0;
        self.cached_texture = None;
    }

    /// Installs decoded frames, applying the circular ellipse mask (`ImageRoundRadius::Ellipse`).
    pub fn set_decoded_frames(&mut self, raw_frames: Vec<VideoUserpicFrame>) {
        if raw_frames.is_empty() {
            self.state = VideoUserpicState::Bad;
            self.frames.clear();
            self.total_duration_ms = 0;
            return;
        }
        let mut masked = Vec::with_capacity(raw_frames.len());
        let mut total = 0u64;
        for frame in raw_frames {
            let rgba = apply_ellipse_mask(frame.width, frame.height, &frame.rgba);
            if let Some(m) =
                VideoUserpicFrame::new(frame.width, frame.height, rgba, frame.duration_ms)
            {
                total = total.saturating_add(m.duration_ms);
                masked.push(m);
            }
        }
        if masked.is_empty() {
            self.state = VideoUserpicState::Bad;
            return;
        }
        self.frames = masked;
        self.total_duration_ms = total.max(1);
        self.state = VideoUserpicState::Ready;
        self.cached_texture = None;
    }

    /// Resolves the active frame index at `now_ms`.
    ///
    /// When `paused` is `true`, freezes on frame `0` (matching `now = paused ? 0 : crl::now()`
    /// in `dialogs_video_userpic.cpp:92`).
    #[must_use]
    pub fn current_frame_index(&mut self, now_ms: u64, paused: bool) -> Option<usize> {
        if self.state != VideoUserpicState::Ready || self.frames.is_empty() {
            return None;
        }
        if paused || self.frames.len() == 1 {
            return Some(0);
        }
        let start = *self.started_at_ms.get_or_insert(now_ms);
        let elapsed = now_ms.saturating_sub(start) % self.total_duration_ms.max(1);
        let mut acc = 0u64;
        for (idx, frame) in self.frames.iter().enumerate() {
            acc = acc.saturating_add(frame.duration_ms);
            if elapsed < acc {
                return Some(idx);
            }
        }
        Some(0)
    }

    /// Paints the animated video userpic into `rect`, or runs `fallback` if video frames are not
    /// ready (`VideoUserpic::paintLeft` in `dialogs_video_userpic.cpp:34-97`).
    ///
    /// Returns `true` if a video frame was painted.
    pub fn paint_or_fallback<F>(
        &mut self,
        ui: &Ui,
        painter: &Painter,
        rect: Rect,
        now_ms: u64,
        paused: bool,
        fallback: F,
    ) -> bool
    where
        F: FnOnce(&Painter, Rect),
    {
        self.last_size = rect.width().round().max(1.0) as u32;
        let Some(frame_idx) = self.current_frame_index(now_ms, paused) else {
            fallback(painter, rect);
            return false;
        };

        let needs_upload = self
            .cached_texture
            .as_ref()
            .is_none_or(|(cached_idx, _)| *cached_idx != frame_idx);
        if needs_upload {
            let frame = &self.frames[frame_idx];
            let img = ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            let tex = ui.ctx().load_texture(
                format!("dialog_video_userpic_{}_{frame_idx}", self.peer_id.bare()),
                img,
                TextureOptions::LINEAR,
            );
            self.cached_texture = Some((frame_idx, tex));
        }

        if let Some((_, ref tex)) = self.cached_texture {
            painter.image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            if !paused && self.frames.len() > 1 {
                ui.ctx().request_repaint();
            }
            true
        } else {
            fallback(painter, rect);
            false
        }
    }
}

/// Cache of [`DialogVideoUserpic`] instances indexed by [`PeerId`].
#[derive(Debug, Default)]
pub struct DialogVideoUserpicCache {
    entries: HashMap<PeerId, DialogVideoUserpic>,
}

impl DialogVideoUserpicCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns (or creates) the [`DialogVideoUserpic`] entry for `peer_id`.
    pub fn entry_mut(&mut self, peer_id: PeerId) -> &mut DialogVideoUserpic {
        self.entries
            .entry(peer_id)
            .or_insert_with(|| DialogVideoUserpic::new(peer_id))
    }

    /// Drops `peer_id`'s video userpic (`_videoUserpics.remove(peer)`).
    pub fn remove(&mut self, peer_id: PeerId) {
        self.entries.remove(&peer_id);
    }

    /// Returns a read-only reference to `peer_id`'s video userpic, if cached.
    #[must_use]
    pub fn get(&self, peer_id: PeerId) -> Option<&DialogVideoUserpic> {
        self.entries.get(&peer_id)
    }

    /// Number of tracked peer video userpics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no peers are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Paints a dialog userpic using `video_userpic` when ready, falling back to `fallback`
/// (`Dialogs::Ui::PaintUserpic` in `dialogs_video_userpic.cpp:158-173`).
pub fn paint_dialog_userpic<F>(
    ui: &Ui,
    painter: &Painter,
    rect: Rect,
    video_userpic: Option<&mut DialogVideoUserpic>,
    now_ms: u64,
    paused: bool,
    fallback: F,
) -> bool
where
    F: FnOnce(&Painter, Rect),
{
    if let Some(video) = video_userpic {
        video.paint_or_fallback(ui, painter, rect, now_ms, paused, fallback)
    } else {
        fallback(painter, rect);
        false
    }
}

/// Result of asking the session for a video avatar's decoded frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoUserpicFetch {
    /// Still downloading or decoding; ask again on a later frame.
    Pending,
    /// Decoded looping frames.
    Ready(Vec<VideoUserpicFrame>),
    /// The video could not be loaded; keep the static userpic.
    Failed,
}

/// Pixel size video avatars are decoded at for the dialogs list (2x the 42px row photo).
pub const VIDEO_USERPIC_DECODE_SIZE: u32 = 96;

/// Upper bound on decoded frames per video avatar (profile videos loop in under 10 s).
pub const VIDEO_USERPIC_MAX_FRAMES: usize = 300;

/// Decodes a profile video (MP4) into looping RGBA frames of `size`x`size` pixels with FFmpeg
/// (`Media::Clip` reader used by `VideoUserpic`).
pub fn decode_video_userpic_bytes(
    bytes: &[u8],
    size: u32,
) -> Result<Vec<VideoUserpicFrame>, String> {
    let mut generator =
        tg_video::FrameGenerator::from_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
    let target = tg_video::Size {
        width: size,
        height: size,
    };
    let mut frames = Vec::new();
    while frames.len() < VIDEO_USERPIC_MAX_FRAMES {
        let Some(generated) = generator
            .render_next(Some(target))
            .map_err(|e| e.to_string())?
        else {
            break;
        };
        let frame = generated.frame;
        let rgba = match frame.data {
            tg_video::FrameData::Rgba(px) => px,
            tg_video::FrameData::Bgra(mut px) => {
                for chunk in px.as_chunks_mut::<4>().0 {
                    chunk.swap(0, 2);
                }
                px
            }
            tg_video::FrameData::Yuv420(_) | tg_video::FrameData::Nv12(_) => continue,
        };
        let duration = u64::try_from(generated.duration_ms)
            .ok()
            .filter(|&d| d > 0)
            .unwrap_or(DEFAULT_VIDEO_USERPIC_FRAME_MS);
        if let Some(f) = VideoUserpicFrame::new(frame.width, frame.height, rgba, duration) {
            frames.push(f);
        }
        if generated.last {
            break;
        }
    }
    if frames.is_empty() {
        Err("video userpic has no decodable frames".to_owned())
    } else {
        Ok(frames)
    }
}
