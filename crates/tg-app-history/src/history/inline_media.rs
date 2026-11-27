//! Inline chat media playback runtime: GIF/video/round-video/WebM-sticker decoding via
//! `tg-video` & `tg_app_media::media::inline::InlinePlayback`, per-size `tg-lottie` frame cache,
//! `tg-video` thumbnail generation, photo loading progress ring & blur-up from stripped
//! thumbnails, spoiler particle veils with click-to-reveal, `tg-audio::PlayerInstance` +
//! top `AudioPlayerBar` for voice/music/round-video audio, `CoreSettings` autoplay policy,
//! and automatic offscreen pausing.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/media/history_view_gif.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_sticker.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_document.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_photo.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_media_spoiler.cpp`

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense, Shape,
    Stroke, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_audio::{
    AudioDataSource, AudioInput, AudioMsgId, AudioResult, AudioType, DocumentId,
    FullMsgId as AudioFullMsgId, InMemoryPlaylistProvider, MediaKind, MemoryAudioSource,
    MsgId as AudioMsgIdNum, PeerId as AudioPeerId, PlaylistContext, PlaylistEntry,
    PlaylistProvider, SPEED_MAX, SPEED_MIN, TrackState, encode_wav_pcm_s16le,
};
use tg_core::settings::{CoreSettings, PowerSavingFlags};
use tg_lottie::Composition;
use tg_ui::effects::{InfiniteRadialAnimation, RadialAnimation, paint_image_spoiler};
use tg_ui::style::InfiniteRadialAnimationStyle;

use crate::history::backend::{DocumentAttachment, HistoryMedia, HistoryMessage};
use crate::history::media::{format_duration, format_file_size};
use crate::history::photo_frame::{PhotoFit, photo_frame};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_media::media::backend::{AudioTrackMetadata, MediaBackend, MediaItem, MediaItemId};
use tg_app_media::media::inline::{
    InlineDocumentData, InlineFrameState, InlineMediaSource, InlinePlayback, InlinePlaybackKind,
};
use tg_app_media::media::player_bar::{AudioPlayerBar, AudioPlayerBarEvent};

mod asset_lru;
mod decode_pool;
mod downloads;
mod round_video;

#[cfg(test)]
pub(crate) use downloads::WaitingRadial;
pub use downloads::{MSG_FILE_OVER_DURATION_SECS, MediaDownloadAction, PhotoLink};

#[cfg(test)]
mod tests_decode;
#[cfg(test)]
mod tests_photo_radial;
#[cfg(test)]
pub(crate) mod tests_tgb3138_colored;

use asset_lru::{AssetLru, TexturePool, texture_bytes};
use decode_pool::{DecodeJob, DecodeOutput, DecodeQueue, LottieFrameKey};
pub use round_video::{RoundVideoPhase, paint_round_video_inline, round_remaining_secs};

/// GPU memory budget of decoded inline photos (display-size textures).
const FULL_PHOTO_BUDGET_BYTES: usize = 128 * 1024 * 1024;
/// GPU memory budget of generated video thumbnails.
const VIDEO_THUMB_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// GPU memory budget of rasterised in-chat Lottie frames (all sizes and steps).
const LOTTIE_FRAME_BUDGET_BYTES: usize = 96 * 1024 * 1024;
/// Parsed Lottie compositions kept (counted per sticker).
const LOTTIE_COMP_BUDGET: usize = 256;
/// Evicted Lottie frame textures kept for reuse.
const LOTTIE_TEXTURE_POOL_CAP: usize = 48;
/// Decoded pixels uploaded per frame before the rest waits for the next frame.
const UPLOAD_BUDGET_BYTES_PER_FRAME: usize = 16 * 1024 * 1024;
/// Photos are decoded at their display size rounded up to this many physical pixels.
const PHOTO_DECODE_BUCKET_PX: u32 = 128;
/// Largest side a photo is decoded at.
const PHOTO_DECODE_MAX_SIDE_PX: u32 = 4096;

/// Turns on background decoding of chat media and emoji sprite sheets for the whole process.
/// The desktop binary calls this at startup; tests and screenshot harnesses keep decoding
/// synchronously so their first frame is complete.
pub fn enable_background_decode() {
    tg_emoji::background::set_background_decode(true);
}

/// What [`HistoryMediaRuntime::lottie_frame_per_size`] has for a sticker on this frame.
#[derive(Clone)]
pub enum LottieFrameState {
    /// A frame to paint (the current step, or the last painted one while it is rendered).
    Ready {
        /// Frame texture.
        texture: TextureHandle,
        /// `true` while the animation is still running (keep repainting).
        animated: bool,
    },
    /// The sticker is still being parsed or its first frame rendered in the background; paint
    /// nothing for it on this frame (a repaint follows when it is ready).
    Pending,
    /// The Lottie JSON does not parse.
    Unavailable,
}

/// Square side in physical pixels a photo shown in `rect` is decoded at.
fn photo_decode_side(rect: Rect, pixels_per_point: f32) -> u32 {
    let side = (rect.width().max(rect.height()) * pixels_per_point.max(0.5)).ceil();
    let side = if side.is_finite() { side as u32 } else { 0 };
    side.div_ceil(PHOTO_DECODE_BUCKET_PX)
        .saturating_mul(PHOTO_DECODE_BUCKET_PX)
        .clamp(PHOTO_DECODE_BUCKET_PX, PHOTO_DECODE_MAX_SIDE_PX)
}

/// `true` when two video payloads are the same file or the same bytes.
fn same_video_data(a: &InlineDocumentData, b: &InlineDocumentData) -> bool {
    match (a, b) {
        (InlineDocumentData::File(x), InlineDocumentData::File(y)) => x == y,
        (InlineDocumentData::Bytes(x), InlineDocumentData::Bytes(y)) => x == y,
        (InlineDocumentData::Stream(x), InlineDocumentData::Stream(y)) => x.same_loader(y),
        _ => false,
    }
}

fn photo_ready_bytes(desc: &InlineMediaDescriptor) -> Option<&Arc<[u8]>> {
    match desc.photo_load.as_ref()? {
        PhotoLoadState::Ready { bytes } => Some(bytes),
        PhotoLoadState::Loading { .. } | PhotoLoadState::Pending | PhotoLoadState::NotLoaded => {
            None
        }
    }
}

/// Duration of the blur-up crossfade from stripped thumbnail to full photo (`240ms`).
pub const PHOTO_BLUR_UP_DURATION_SECS: f64 = 0.24;
/// Frame interval of the photo loading arc (tdesktop animations run at 60 fps).
pub const RADIAL_FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);
/// `st::msgFileLayout.thumbSize`: the circle the loading arc is drawn in.
const PHOTO_RADIAL_SIZE: f32 = 44.0;
/// `st::msgFileRadialLine`.
const PHOTO_RADIAL_LINE: f32 = 3.0;
/// `msgDateImgBg` (`#00000054`).
const MSG_DATE_IMG_BG: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 0x54);
/// Duration of the spoiler reveal (`revealAnimation` over `st::fadeWrapDuration`, linear).
pub const SPOILER_REVEAL_FADE_SECS: f64 = 0.2;
/// Standard voice/music speed presets cycled by the inline speed button.
pub const INLINE_AUDIO_SPEED_PRESETS: &[f64] = &[1.0, 1.5, 2.0];

/// Returns whether real hardware audio output (`cpal`) should be enabled by default.
///
/// Enabled in the running desktop app (`#[cfg(not(test))]`) unless `RUSTGRAM_HEADLESS_AUDIO`
/// (or legacy `TGFORK_HEADLESS_AUDIO`) is set;
/// always `false` in `#[cfg(test)]` so unit tests and screenshot harnesses remain headless.
#[must_use]
pub fn default_hardware_audio_enabled() -> bool {
    cfg!(not(test))
        && std::env::var_os("RUSTGRAM_HEADLESS_AUDIO").is_none()
        && std::env::var_os("TGFORK_HEADLESS_AUDIO").is_none()
}

/// Autoplay & media playback policy synchronized from [`tg_core::settings::CoreSettings`].
#[derive(Debug, Clone, PartialEq)]
pub struct AutoplayPolicy {
    /// Autoplay muted GIFs in chat history (`CoreSettings::autoplay.gifs`).
    pub autoplay_gifs: bool,
    /// Autoplay muted videos in chat history (`CoreSettings::autoplay.videos`).
    pub autoplay_videos: bool,
    /// Autoplay muted round video notes in chat history (`CoreSettings::autoplay.round_videos`).
    pub autoplay_round_videos: bool,
    /// Loop animated Lottie stickers (`CoreSettings::loop_animated_stickers`).
    pub loop_animated_stickers: bool,
    /// Autoplay WebM VP9 alpha video stickers (`CoreSettings::autoplay.video_stickers`).
    pub autoplay_video_stickers: bool,
    /// Active power-saving flags from `CoreSettings::power_saving` (or `PowerSavingFlags::ALL`
    /// when `power_saving_force_all && !ignore_battery_saving`).
    pub power_saving_flags: PowerSavingFlags,
    /// Power saving mode active (disables general inline autoplay when `ANIMATIONS` is disabled).
    pub power_saving_active: bool,
    /// Default voice playback speed from `CoreSettings::voice_playback_speed`.
    pub voice_speed: f64,
    /// Default music playback speed from `CoreSettings::audio_playback_speed`.
    pub audio_speed: f64,
    /// Default song volume from `CoreSettings::song_volume`.
    pub song_volume: f64,
}

impl Default for AutoplayPolicy {
    fn default() -> Self {
        Self::from_core_settings(&CoreSettings::default())
    }
}

impl AutoplayPolicy {
    /// Derives [`AutoplayPolicy`] from the typed fields of a [`CoreSettings`] instance.
    #[must_use]
    pub fn from_core_settings(settings: &CoreSettings) -> Self {
        let effective_flags = if settings.power_saving_force_all && !settings.ignore_battery_saving
        {
            PowerSavingFlags::ALL
        } else {
            settings.power_saving
        };
        Self {
            autoplay_gifs: settings.autoplay.gifs,
            autoplay_videos: settings.autoplay.videos,
            autoplay_round_videos: settings.autoplay.round_videos,
            loop_animated_stickers: settings.loop_animated_stickers,
            autoplay_video_stickers: settings.autoplay.video_stickers,
            power_saving_flags: effective_flags,
            power_saving_active: effective_flags.contains(PowerSavingFlags::ANIMATIONS),
            voice_speed: settings
                .voice_playback_speed
                .value
                .clamp(SPEED_MIN, SPEED_MAX),
            audio_speed: settings
                .audio_playback_speed
                .value
                .clamp(SPEED_MIN, SPEED_MAX),
            song_volume: settings.song_volume.clamp(0.0, 1.0),
        }
    }

    /// Returns `true` if the given [`InlinePlaybackKind`] is allowed to autoplay.
    #[must_use]
    pub const fn allows_inline(&self, kind: InlinePlaybackKind) -> bool {
        if self.power_saving_active
            || self
                .power_saving_flags
                .contains(PowerSavingFlags::ANIMATIONS)
        {
            return false;
        }
        match kind {
            InlinePlaybackKind::Gif => self.autoplay_gifs,
            InlinePlaybackKind::RoundVideo => self.autoplay_round_videos && self.autoplay_videos,
            InlinePlaybackKind::VideoSticker => {
                self.autoplay_video_stickers
                    && !self
                        .power_saving_flags
                        .contains(PowerSavingFlags::STICKERS_CHAT)
            }
        }
    }

    /// Returns `true` if animated Lottie stickers are allowed to autoplay in chat.
    #[must_use]
    pub const fn allows_lottie_autoplay(&self) -> bool {
        !self.power_saving_active
            && !self
                .power_saving_flags
                .contains(PowerSavingFlags::STICKERS_CHAT)
    }

    /// Returns `true` if animated Lottie stickers should loop continuously.
    #[must_use]
    pub const fn allows_lottie_loop(&self) -> bool {
        self.allows_lottie_autoplay() && self.loop_animated_stickers
    }

    /// Returns `true` if spoiler particle animations are permitted (`PowerSavingFlags::CHAT_SPOILER`).
    #[must_use]
    pub const fn allows_spoiler_particles(&self) -> bool {
        !self.power_saving_active
            && !self
                .power_saving_flags
                .contains(PowerSavingFlags::CHAT_SPOILER)
    }

    /// Returns `true` if chat visual effects (summary star particles, birthday confetti) are permitted
    /// (`PowerSavingFlags::CHAT_EFFECTS`).
    #[must_use]
    pub const fn allows_chat_effects(&self) -> bool {
        !self.power_saving_active
            && !self
                .power_saving_flags
                .contains(PowerSavingFlags::CHAT_EFFECTS)
    }

    /// Returns `true` when global power-saving / reduced-motion mode is active.
    #[must_use]
    pub const fn power_saving_enabled(&self) -> bool {
        self.power_saving_active
    }
}

pub use tg_app_core::core_types::dto::inline_media::*;

/// Result of evaluating an inline video/GIF/round-video/video-sticker frame.
#[derive(Clone)]
pub struct InlineClipPaintResult {
    /// Decoded frame texture, if ready.
    pub texture: Option<TextureHandle>,
    /// Playback progress in `0.0..=1.0`.
    pub progress: f32,
    /// Total duration in ms (`0` if unknown).
    pub duration_ms: i64,
    /// True if actively playing frames.
    pub is_playing: bool,
    /// True if round video is currently playing with sound enabled via `tg-audio`.
    pub sound_on: bool,
}

/// Composite [`AudioDataSource`] checking history-registered tracks first and falling back to
/// the shared app [`MediaBackend`] (e.g. `MockSession`) so ONE [`AudioPlayerBar`] serves both
/// chat history and shared-media playback.
struct CompositeAudioSource {
    overlay: Arc<MemoryAudioSource>,
    fallback: Mutex<Option<Arc<dyn AudioDataSource>>>,
}

impl CompositeAudioSource {
    fn new(overlay: Arc<MemoryAudioSource>) -> Self {
        Self {
            overlay,
            fallback: Mutex::new(None),
        }
    }

    fn set_fallback(&self, source: Arc<dyn AudioDataSource>) {
        if let Ok(mut guard) = self.fallback.lock() {
            *guard = Some(source);
        }
    }
}

impl AudioDataSource for CompositeAudioSource {
    fn open(&self, id: &AudioMsgId) -> AudioResult<AudioInput> {
        match self.overlay.open(id) {
            Ok(input) => Ok(input),
            Err(overlay_err) => {
                let fallback = self.fallback.lock().ok().and_then(|g| g.clone());
                if let Some(fb) = fallback {
                    fb.open(id)
                } else {
                    Err(overlay_err)
                }
            }
        }
    }

    fn open_stream(&self, id: &AudioMsgId) -> AudioResult<Box<dyn tg_audio::AudioStream>> {
        if self.overlay.contains(id.document_id) {
            return self.overlay.open_stream(id);
        }
        let fallback = self.fallback.lock().ok().and_then(|g| g.clone());
        match fallback {
            Some(fb) => fb.open_stream(id),
            None => self.overlay.open_stream(id),
        }
    }
}

/// Composite [`PlaylistProvider`] unifying history chat playlists with the shared app
/// [`MediaBackend`] playlists.
struct CompositePlaylistProvider {
    overlay: Arc<InMemoryPlaylistProvider>,
    fallback: Mutex<Option<Arc<dyn PlaylistProvider>>>,
}

impl CompositePlaylistProvider {
    fn new(overlay: Arc<InMemoryPlaylistProvider>) -> Self {
        Self {
            overlay,
            fallback: Mutex::new(None),
        }
    }

    fn set_fallback(&self, provider: Arc<dyn PlaylistProvider>) {
        if let Ok(mut guard) = self.fallback.lock() {
            *guard = Some(provider);
        }
    }
}

impl PlaylistProvider for CompositePlaylistProvider {
    fn playlist_for(&self, context: PlaylistContext, audio_type: AudioType) -> Vec<PlaylistEntry> {
        let local = self.overlay.playlist_for(context, audio_type);
        if !local.is_empty() {
            return local;
        }
        let fallback = self.fallback.lock().ok().and_then(|g| g.clone());
        fallback.map_or_else(Vec::new, |fb| fb.playlist_for(context, audio_type))
    }

    fn mark_media_read(&self, document_id: DocumentId, context_id: AudioFullMsgId) {
        self.overlay.mark_media_read(document_id, context_id);
        let fallback = self.fallback.lock().ok().and_then(|g| g.clone());
        if let Some(fb) = fallback {
            fb.mark_media_read(document_id, context_id);
        }
    }
}

/// Shared [`MediaBackend`] adapter backing the single unified [`AudioPlayerBar`] across both
/// chat history voice/music/round-video tracks and shared-media tracks.
struct SharedAudioBackend {
    overlay_source: Arc<MemoryAudioSource>,
    overlay_playlists: Arc<InMemoryPlaylistProvider>,
    composite_source: Arc<CompositeAudioSource>,
    composite_playlists: Arc<CompositePlaylistProvider>,
    metadata: Mutex<HashMap<AudioMsgId, AudioTrackMetadata>>,
}

impl SharedAudioBackend {
    fn new() -> Self {
        let overlay_source = Arc::new(MemoryAudioSource::new());
        let overlay_playlists = Arc::new(InMemoryPlaylistProvider::new());
        let composite_source = Arc::new(CompositeAudioSource::new(Arc::clone(&overlay_source)));
        let composite_playlists = Arc::new(CompositePlaylistProvider::new(Arc::clone(
            &overlay_playlists,
        )));
        Self {
            overlay_source,
            overlay_playlists,
            composite_source,
            composite_playlists,
            metadata: Mutex::new(HashMap::new()),
        }
    }

    fn attach_fallback(&self, base: &dyn MediaBackend) {
        self.composite_source.set_fallback(base.audio_data_source());
        self.composite_playlists
            .set_fallback(base.playlist_provider());
    }

    fn register_track(
        &self,
        audio_id: AudioMsgId,
        audio_bytes: Vec<u8>,
        ext_hint: Option<String>,
        meta: AudioTrackMetadata,
    ) {
        self.overlay_source
            .insert(audio_id.document_id, audio_bytes, ext_hint);
        if let Ok(mut guard) = self.metadata.lock() {
            guard.insert(audio_id, meta);
        }
    }

    fn register_streamed_track(
        &self,
        audio_id: AudioMsgId,
        open: tg_audio::StreamedInputOpener,
        meta: AudioTrackMetadata,
    ) {
        self.stream_track(audio_id, open);
        if let Ok(mut guard) = self.metadata.lock() {
            guard.insert(audio_id, meta);
        }
    }

    /// Makes the registered `audio_id` stream through `open` (its metadata stays).
    fn stream_track(&self, audio_id: AudioMsgId, open: tg_audio::StreamedInputOpener) {
        self.overlay_source
            .insert_streamed(audio_id.document_id, open);
    }

    fn set_playlist(&self, peer_id: PeerId, audio_type: AudioType, entries: Vec<PlaylistEntry>) {
        let ctx = PlaylistContext {
            peer_id: AudioPeerId(peer_id.0),
            ..PlaylistContext::default()
        };
        self.overlay_playlists
            .set_playlist(ctx, audio_type, entries);
    }
}

/// Ephemeral [`MediaBackend`] view combining [`SharedAudioBackend`] metadata with an optional
/// host [`MediaBackend`] (such as `MockSession`) during [`AudioPlayerBar::show`].
struct CompositeMetadataBackend<'a> {
    overlay: &'a SharedAudioBackend,
    base: Option<&'a dyn MediaBackend>,
}

impl MediaBackend for CompositeMetadataBackend<'_> {
    fn siblings_in_chat(&self, peer_id: i64, item_id: MediaItemId) -> Vec<MediaItem> {
        self.base
            .map_or_else(Vec::new, |b| b.siblings_in_chat(peer_id, item_id))
    }

    fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
        self.base.and_then(|b| b.get_item(id))
    }

    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        if let Some(base) = self.base {
            base.load_bytes(id)
        } else {
            anyhow::bail!("not an image viewer backend")
        }
    }

    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn tg_video::RangeLoader>> {
        if let Some(base) = self.base {
            base.open_range_loader(id)
        } else {
            anyhow::bail!("not a video viewer backend")
        }
    }

    fn save_to_disk(
        &self,
        id: MediaItemId,
        destination: &std::path::Path,
    ) -> anyhow::Result<PathBuf> {
        if let Some(base) = self.base {
            base.save_to_disk(id, destination)
        } else {
            Ok(destination.to_path_buf())
        }
    }

    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Some(base) = self.base {
            base.copy_item(id)
        } else {
            Ok(())
        }
    }

    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Some(base) = self.base {
            base.forward_item(id)
        } else {
            Ok(())
        }
    }

    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Some(base) = self.base {
            base.delete_item(id)
        } else {
            Ok(())
        }
    }

    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        if let Some(base) = self.base {
            base.show_in_chat(peer_id, msg_id);
        }
    }

    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        if let Some(base) = self.base {
            base.open_in_external_app(id)
        } else {
            Ok(PathBuf::new())
        }
    }

    // The trait default would call `open_in_external_app` again, re-running the launch
    // warning check the user just confirmed, so the confirmed launch must reach the base.
    fn open_in_external_app_confirmed(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        if let Some(base) = self.base {
            base.open_in_external_app_confirmed(id)
        } else {
            Ok(PathBuf::new())
        }
    }

    fn forward_item_to(&self, id: MediaItemId, to_peer: i64) -> anyhow::Result<()> {
        if let Some(base) = self.base {
            base.forward_item_to(id, to_peer)
        } else {
            Ok(())
        }
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.overlay
            .audio_track_metadata(id)
            .or_else(|| self.base.and_then(|b| b.audio_track_metadata(id)))
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.overlay.audio_data_source()
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.overlay.playlist_provider()
    }
}

impl MediaBackend for SharedAudioBackend {
    fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
        Vec::new()
    }

    fn get_item(&self, _id: MediaItemId) -> Option<MediaItem> {
        None
    }

    fn load_bytes(&self, _id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("not an image viewer backend")
    }

    fn open_range_loader(
        &self,
        _id: MediaItemId,
    ) -> anyhow::Result<Arc<dyn tg_video::RangeLoader>> {
        anyhow::bail!("not a video viewer backend")
    }

    fn save_to_disk(
        &self,
        _id: MediaItemId,
        destination: &std::path::Path,
    ) -> anyhow::Result<PathBuf> {
        Ok(destination.to_path_buf())
    }

    fn copy_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }

    fn forward_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }

    fn delete_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
        Ok(())
    }

    fn show_in_chat(&self, _peer_id: i64, _msg_id: i32) {}

    fn open_in_external_app(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::new())
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.metadata.lock().ok()?.get(&id).cloned()
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        Arc::clone(&self.composite_source) as Arc<dyn AudioDataSource>
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        Arc::clone(&self.composite_playlists) as Arc<dyn PlaylistProvider>
    }
}

struct BlurUpEntry {
    texture: TextureHandle,
    /// First paint with this texture; `None` until then (and after a descriptor reload, which
    /// replays the blur-up exactly like before).
    started_at_secs: Option<f64>,
    /// Square side the photo was decoded to fit.
    max_side: u32,
    /// `true` when the source is larger than `max_side` (a larger display needs a new decode).
    downscaled: bool,
}

/// Complete inline media playback coordinator owned by [`crate::history::HistoryWidget`] /
/// [`crate::history::inner::HistoryInnerWidget`].
pub struct HistoryMediaRuntime {
    /// Inline `tg-video::ClipReader` manager from `tg_app_media::media::inline`.
    pub inline: InlinePlayback,
    /// Autoplay & speed policy derived from `CoreSettings`.
    pub policy: AutoplayPolicy,
    /// Registered per-message rich media descriptors (bytes/files/spoilers/photo progress/audio).
    descriptors: HashMap<MsgId, InlineMediaDescriptor>,
    /// Messages currently visible in the viewport during this frame.
    visible_this_frame: HashSet<MsgId>,
    /// Messages that were visible in the previous frame.
    visible_last_frame: HashSet<MsgId>,
    /// Messages currently paused solely because they scrolled offscreen.
    offscreen_paused: HashSet<MsgId>,
    /// Messages whose spoiler veil has been clicked to reveal -> reveal timestamp (secs).
    revealed_spoilers: HashMap<MsgId, f64>,
    /// Round video message currently playing with sound enabled (unmuted).
    unmuted_round_msg: Option<MsgId>,
    /// Per-round-video circular seek ring state (`HistoryView::VideoMessageSeek`).
    round_seeks: HashMap<MsgId, crate::history::video_message_seek::VideoMessageSeek>,
    /// Last explicitly sought normalized position (`0.0..=1.0`) per round video message.
    round_seek_positions: HashMap<MsgId, f64>,
    /// Circle-masked document thumbnails shown until a round video's first frame decodes.
    round_thumbs: HashMap<MsgId, Option<TextureHandle>>,
    /// Round thumbnails being prepared in the background (`msg_id -> media revision`).
    round_thumbs_pending: HashMap<MsgId, u64>,
    /// Whether the sound-on round's audio track has reported active since it was started, so a
    /// track that has not started yet is not mistaken for one that has finished.
    round_sound_seen_active: bool,
    /// Generated `tg-video` thumbnail textures for videos lacking `stripped_thumb`.
    video_thumbs: AssetLru<MsgId, Option<TextureHandle>>,
    /// Video thumbnails being generated in the background (`msg_id -> media revision`).
    video_thumbs_pending: HashMap<MsgId, u64>,
    /// Decoded inline photo textures (display size) with blur-up start timestamps.
    full_photos: AssetLru<MsgId, BlurUpEntry>,
    /// Photos being decoded in the background (`msg_id -> (media revision, max side)`).
    photos_pending: HashMap<MsgId, (u64, u32)>,
    /// Photos whose bytes failed to decode (`msg_id -> media revision`), not retried.
    photos_failed: HashMap<MsgId, u64>,
    /// Bumped whenever a message's media payload changes, so stale decodes are dropped.
    media_revisions: HashMap<MsgId, u64>,
    next_media_revision: u64,
    /// Size-keyed Lottie compositions and rendered frames `(sticker_id, size_px, step_idx)`.
    lottie_comps: AssetLru<u64, Option<Arc<Composition>>>,
    lottie_parsing: HashSet<u64>,
    lottie_size_frames: AssetLru<LottieFrameKey, TextureHandle>,
    lottie_rendering: HashSet<LottieFrameKey>,
    /// `(sticker_id, size_px)` whose remaining steps were queued for background rendering.
    lottie_prefetched: HashSet<(u64, u32, Option<Color32>)>,
    /// Last frame painted per `(sticker_id, size_px)`, shown while the current step renders.
    lottie_last_shown: HashMap<(u64, u32, Option<Color32>), TextureHandle>,
    /// Evicted Lottie frame textures, refilled for new frames of the same size.
    lottie_texture_pool: TexturePool,
    /// Background decode workers (photos, video thumbnails, Lottie).
    decode: DecodeQueue,
    /// Frame (egui pass) the upload budget below belongs to.
    upload_pass: u64,
    upload_budget_left: usize,
    /// Lottie first-seen timestamp per message (for single-play when `loop_animated_stickers == false`).
    lottie_started_at: HashMap<MsgId, f64>,
    /// Underlying shared audio backend & single unified [`AudioPlayerBar`] (`tg-audio::PlayerInstance`).
    audio_backend: SharedAudioBackend,
    audio_player_bar: AudioPlayerBar,
    /// Whether the unified [`AudioPlayerBar`] was initialized with hardware `cpal` output.
    hardware_audio_enabled: bool,
    /// Mapping from history `MsgId` -> registered `AudioMsgId`.
    registered_audio: HashMap<MsgId, AudioMsgId>,
    /// Active peer id for audio playlists.
    active_peer: PeerId,
    /// Infinite radial spinner state (round video loads).
    spinner: InfiniteRadialAnimation,
    /// Loading arcs of photos (`Photo::_animation->radial`), only while one animates.
    photo_radials: HashMap<MsgId, RadialAnimation>,
    /// Download requests for the backend and the voice message waiting for its file.
    downloads: downloads::MediaDownloads,
    /// Last frame timestamp in ms.
    last_now_ms: i64,
}

impl fmt::Debug for HistoryMediaRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HistoryMediaRuntime")
            .field("active_clips", &self.inline.active_count())
            .field("policy", &self.policy)
            .field("visible_last_frame", &self.visible_last_frame.len())
            .field("offscreen_paused", &self.offscreen_paused.len())
            .field("revealed_spoilers", &self.revealed_spoilers.len())
            .field("unmuted_round_msg", &self.unmuted_round_msg)
            .field("audio_bar_visible", &self.audio_player_bar.is_visible())
            .finish()
    }
}

impl Default for HistoryMediaRuntime {
    fn default() -> Self {
        Self::new(false)
    }
}

impl HistoryMediaRuntime {
    /// Creates a new [`HistoryMediaRuntime`].
    ///
    /// Pass `enable_hardware_audio = false` in tests/headless mode so `tg-audio` advances
    /// deterministically via its software mixer without opening an OS audio device.
    #[must_use]
    pub fn new(enable_hardware_audio: bool) -> Self {
        let audio_backend = SharedAudioBackend::new();
        let audio_player_bar = AudioPlayerBar::new(&audio_backend, enable_hardware_audio);
        let policy = AutoplayPolicy::default();
        let mut inline = InlinePlayback::new();
        inline.set_autoplay_gifs(policy.allows_inline(InlinePlaybackKind::Gif));
        inline.set_autoplay_round_videos(policy.allows_inline(InlinePlaybackKind::RoundVideo));
        let spinner = InfiniteRadialAnimation::new(InfiniteRadialAnimationStyle {
            color: Color32::WHITE,
            thickness: 2.8,
            ..InfiniteRadialAnimationStyle::default_radial(&tg_ui::theme::DayPalette::new())
        });
        Self {
            inline,
            policy,
            descriptors: HashMap::new(),
            visible_this_frame: HashSet::new(),
            visible_last_frame: HashSet::new(),
            offscreen_paused: HashSet::new(),
            revealed_spoilers: HashMap::new(),
            unmuted_round_msg: None,
            round_seeks: HashMap::new(),
            round_seek_positions: HashMap::new(),
            round_thumbs: HashMap::new(),
            round_thumbs_pending: HashMap::new(),
            round_sound_seen_active: false,
            video_thumbs: AssetLru::new(VIDEO_THUMB_BUDGET_BYTES),
            video_thumbs_pending: HashMap::new(),
            full_photos: AssetLru::new(FULL_PHOTO_BUDGET_BYTES),
            photos_pending: HashMap::new(),
            photos_failed: HashMap::new(),
            media_revisions: HashMap::new(),
            next_media_revision: 0,
            lottie_comps: AssetLru::new(LOTTIE_COMP_BUDGET),
            lottie_parsing: HashSet::new(),
            lottie_size_frames: AssetLru::new(LOTTIE_FRAME_BUDGET_BYTES),
            lottie_rendering: HashSet::new(),
            lottie_prefetched: HashSet::new(),
            lottie_last_shown: HashMap::new(),
            lottie_texture_pool: TexturePool::new(LOTTIE_TEXTURE_POOL_CAP),
            decode: DecodeQueue::default(),
            upload_pass: u64::MAX,
            upload_budget_left: UPLOAD_BUDGET_BYTES_PER_FRAME,
            lottie_started_at: HashMap::new(),
            audio_backend,
            audio_player_bar,
            hardware_audio_enabled: enable_hardware_audio,
            registered_audio: HashMap::new(),
            active_peer: PeerId::ZERO,
            spinner,
            photo_radials: HashMap::new(),
            downloads: downloads::MediaDownloads::default(),
            last_now_ms: 0,
        }
    }

    /// Attaches a host [`MediaBackend`] (e.g. `MockSession`) as fallback audio source & playlist
    /// provider so ONE [`AudioPlayerBar`] serves both chat history and shared media sections.
    ///
    /// If `enable_hardware_audio` differs from the current hardware-output mode, the underlying
    /// [`AudioPlayerBar`] is rebuilt with the requested mode while preserving volume.
    pub fn attach_shared_media_backend(
        &mut self,
        base: &dyn MediaBackend,
        enable_hardware_audio: bool,
    ) {
        self.audio_backend.attach_fallback(base);
        if self.hardware_audio_enabled != enable_hardware_audio {
            let volume = self.audio_player_bar.volume();
            let mut bar = AudioPlayerBar::new(&self.audio_backend, enable_hardware_audio);
            bar.set_volume(volume);
            self.audio_player_bar = bar;
            self.hardware_audio_enabled = enable_hardware_audio;
        }
    }

    /// Returns `true` if the shared [`AudioPlayerBar`] is configured for real `cpal` output.
    #[must_use]
    pub const fn hardware_audio_enabled(&self) -> bool {
        self.hardware_audio_enabled
    }

    /// Plays an external audio/voice track from `base` (such as a shared-media item) through the
    /// single unified [`AudioPlayerBar`].
    pub fn play_external_audio_track(
        &mut self,
        base: &dyn MediaBackend,
        audio_id: AudioMsgId,
        enable_hardware_audio: bool,
    ) -> anyhow::Result<()> {
        self.attach_shared_media_backend(base, enable_hardware_audio);
        self.unmuted_round_msg = None;
        self.audio_player_bar.play_item(audio_id, None)
    }

    /// Clears the unmuted round-video marker (e.g. when the shared audio bar is closed).
    pub fn clear_unmuted_round_video(&mut self) {
        self.unmuted_round_msg = None;
    }

    /// Applies [`CoreSettings`] autoplay, sticker loop, audio speed, and volume preferences.
    pub fn apply_core_settings(&mut self, settings: &CoreSettings) {
        self.set_autoplay_policy(AutoplayPolicy::from_core_settings(settings));
    }

    /// Replaces the active [`AutoplayPolicy`] and updates [`InlinePlayback`] & [`AudioPlayerBar`].
    pub fn set_autoplay_policy(&mut self, policy: AutoplayPolicy) {
        self.inline
            .set_autoplay_gifs(policy.allows_inline(InlinePlaybackKind::Gif));
        self.inline
            .set_autoplay_round_videos(policy.allows_inline(InlinePlaybackKind::RoundVideo));
        self.audio_player_bar.set_volume(policy.song_volume);
        self.policy = policy;
    }

    /// Registers an [`InlineMediaDescriptor`] for `msg_id`.
    pub fn register_descriptor(&mut self, msg_id: MsgId, descriptor: InlineMediaDescriptor) {
        // A reload with the same bytes keeps the decoded pixels (nothing to decode again) but
        // replays the blur-up exactly like a fresh decode did; changed media is decoded anew.
        let old = self.descriptors.get(&msg_id);
        let same_photo = match (
            old.and_then(photo_ready_bytes),
            photo_ready_bytes(&descriptor),
        ) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a[..] == b[..],
            (None, None) => true,
            _ => false,
        };
        let same_video = match (
            old.and_then(|d| d.video_data.as_ref()),
            descriptor.video_data.as_ref(),
        ) {
            (Some(a), Some(b)) => same_video_data(a, b),
            (None, None) => true,
            _ => false,
        };
        if !(same_photo && same_video) {
            self.bump_media_revision(msg_id);
        }
        if same_photo {
            if let Some(entry) = self.full_photos.get_mut(&msg_id) {
                entry.started_at_secs = None;
            }
        } else {
            self.full_photos.remove(&msg_id);
            self.round_thumbs.remove(&msg_id);
        }
        if !same_video {
            self.video_thumbs.remove(&msg_id);
        }
        // A voice / round track registered before its bytes arrived plays the synthesized
        // stand-in: drop it so the next sync registers the downloaded audio.
        let had_audio = self
            .descriptors
            .get(&msg_id)
            .is_some_and(|d| d.audio_bytes.is_some());
        if !had_audio && descriptor.audio_bytes.is_some() && self.unmuted_round_msg != Some(msg_id)
        {
            self.registered_audio.remove(&msg_id);
            self.drop_voice_stream(msg_id);
        }
        self.descriptors.insert(msg_id, descriptor);
        self.stream_registered_voice(msg_id);
        self.play_pending_voice_if_loaded(msg_id);
        self.play_pending_round_if_loaded(msg_id);
    }

    /// Returns the registered [`InlineMediaDescriptor`] for `msg_id`, if any.
    #[must_use]
    pub fn descriptor(&self, msg_id: MsgId) -> Option<&InlineMediaDescriptor> {
        self.descriptors.get(&msg_id)
    }

    /// Updates photo loading progress or transitions a photo to [`PhotoLoadState::Ready`] (blur-up).
    pub fn set_photo_load_state(&mut self, msg_id: MsgId, state: PhotoLoadState) {
        let entry = self.descriptors.entry(msg_id).or_default();
        entry.photo_load = Some(state);
        if matches!(entry.photo_load, Some(PhotoLoadState::Ready { .. })) {
            self.full_photos.remove(&msg_id);
            self.bump_media_revision(msg_id);
        }
    }

    fn bump_media_revision(&mut self, msg_id: MsgId) {
        self.next_media_revision += 1;
        self.media_revisions
            .insert(msg_id, self.next_media_revision);
        self.photos_pending.remove(&msg_id);
        self.photos_failed.remove(&msg_id);
        self.video_thumbs_pending.remove(&msg_id);
        self.round_thumbs_pending.remove(&msg_id);
    }

    fn media_revision(&self, msg_id: MsgId) -> u64 {
        self.media_revisions.get(&msg_id).copied().unwrap_or(0)
    }

    /// Uses background decode workers (`Some(true)`), decodes inline (`Some(false)`), or follows
    /// the process-wide switch [`tg_emoji::background::set_background_decode`] (`None`).
    pub fn set_background_decode(&mut self, background: Option<bool>) {
        self.decode.set_background(background);
    }

    /// Number of decode jobs handed to background workers so far.
    #[must_use]
    pub const fn background_decode_jobs(&self) -> u64 {
        self.decode.submitted()
    }

    /// GPU bytes held by decoded photos, video thumbnails and Lottie frames, and the number of
    /// entries evicted by their memory budgets so far.
    #[must_use]
    pub fn decoded_asset_stats(&self) -> (usize, u64) {
        (
            self.full_photos.bytes() + self.video_thumbs.bytes() + self.lottie_size_frames.bytes(),
            self.full_photos.evicted()
                + self.video_thumbs.evicted()
                + self.lottie_size_frames.evicted(),
        )
    }

    /// Number of Lottie frame textures refilled instead of created.
    #[must_use]
    pub const fn reused_textures(&self) -> u64 {
        self.lottie_texture_pool.reused()
    }

    /// Side of the decoded texture of `msg_id`'s photo, if decoded.
    #[must_use]
    pub fn decoded_photo_size(&self, msg_id: MsgId) -> Option<[usize; 2]> {
        self.full_photos.peek(&msg_id).map(|e| e.texture.size())
    }

    /// Uploads finished background decodes, within this frame's upload budget.
    fn pump_decoded(&mut self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        self.lottie_texture_pool.begin_pass(pass);
        if pass != self.upload_pass {
            self.upload_pass = pass;
            self.upload_budget_left = UPLOAD_BUDGET_BYTES_PER_FRAME;
        }
        while let Some(output) = self.decode.next_output() {
            let cost = decode_output_bytes(&output);
            if cost > self.upload_budget_left
                && self.upload_budget_left < UPLOAD_BUDGET_BYTES_PER_FRAME
            {
                self.decode.defer(output);
                ctx.request_repaint();
                break;
            }
            self.upload_budget_left = self.upload_budget_left.saturating_sub(cost);
            self.apply_decoded(ctx, output);
        }
    }

    /// Runs `job` inline or on the workers; an inline result is applied at once.
    fn run_decode(&mut self, ctx: &egui::Context, job: DecodeJob) {
        if let Some(output) = self.decode.submit(ctx, job) {
            self.apply_decoded(ctx, output);
        }
    }

    fn apply_decoded(&mut self, ctx: &egui::Context, output: DecodeOutput) {
        match output {
            DecodeOutput::Photo {
                msg_id,
                revision,
                max_side,
                image,
            } => {
                if self.photos_pending.get(&msg_id) == Some(&(revision, max_side)) {
                    self.photos_pending.remove(&msg_id);
                }
                if revision != self.media_revision(msg_id) {
                    return;
                }
                if self
                    .descriptors
                    .get(&msg_id)
                    .and_then(photo_ready_bytes)
                    .is_none()
                {
                    return;
                }
                let Some((image, downscaled)) = image else {
                    if !self.full_photos.contains(&msg_id) {
                        self.photos_failed.insert(msg_id, revision);
                    }
                    return;
                };
                // A sharper re-decode for a larger display keeps the running blur-up.
                let started_at_secs = self
                    .full_photos
                    .peek(&msg_id)
                    .and_then(|e| e.started_at_secs);
                let size = texture_bytes(image.size);
                let texture = ctx.load_texture(
                    format!("tg_full_photo_{}", msg_id.0),
                    image,
                    TextureOptions::LINEAR,
                );
                self.full_photos.insert(
                    msg_id,
                    BlurUpEntry {
                        texture,
                        started_at_secs,
                        max_side,
                        downscaled,
                    },
                    size,
                );
            }
            DecodeOutput::VideoThumb {
                msg_id,
                revision,
                image,
            } => {
                if self.video_thumbs_pending.get(&msg_id) == Some(&revision) {
                    self.video_thumbs_pending.remove(&msg_id);
                }
                if revision != self.media_revision(msg_id) {
                    return;
                }
                let size = image.as_ref().map_or(0, |img| texture_bytes(img.size));
                let texture = image.map(|img| {
                    ctx.load_texture(
                        format!("tg_gen_video_thumb_{}", msg_id.0),
                        img,
                        TextureOptions::LINEAR,
                    )
                });
                self.video_thumbs.insert(msg_id, texture, size);
            }
            DecodeOutput::RoundThumb {
                msg_id,
                revision,
                image,
            } => {
                if self.round_thumbs_pending.get(&msg_id) == Some(&revision) {
                    self.round_thumbs_pending.remove(&msg_id);
                }
                if revision != self.media_revision(msg_id) {
                    return;
                }
                let texture = image.map(|img| {
                    ctx.load_texture(
                        format!("tg_round_thumb_{}", msg_id.0),
                        img,
                        TextureOptions::LINEAR,
                    )
                });
                self.round_thumbs.insert(msg_id, texture);
            }
            DecodeOutput::LottieParsed { sticker_id, comp } => {
                self.lottie_parsing.remove(&sticker_id);
                self.lottie_comps.insert(sticker_id, comp, 1);
            }
            DecodeOutput::LottieFrame { key, image } => {
                self.lottie_rendering.remove(&key);
                let (sticker_id, size_bucket, step_idx, overlay) = key;
                let size = texture_bytes(image.size);
                let texture = match self.lottie_texture_pool.take(image.size) {
                    Some(mut reused) => {
                        reused.set(image, TextureOptions::LINEAR);
                        reused
                    }
                    None => ctx.load_texture(
                        format!(
                            "tg_lottie_{sticker_id}_{size_bucket}_{step_idx}{}",
                            if overlay.is_some() { "_selected" } else { "" }
                        ),
                        image,
                        TextureOptions::LINEAR,
                    ),
                };
                for evicted in self.lottie_size_frames.insert(key, texture, size) {
                    let id = evicted.id();
                    self.lottie_last_shown.retain(|_, shown| shown.id() != id);
                    self.lottie_texture_pool.release(evicted);
                }
            }
        }
    }

    /// Marks `msg_id` as spoiler-covered or clears its spoiler flag.
    pub fn set_spoiler(&mut self, msg_id: MsgId, has_spoiler: bool) {
        self.descriptors.entry(msg_id).or_default().has_spoiler = has_spoiler;
        if !has_spoiler {
            self.revealed_spoilers.remove(&msg_id);
        }
    }

    /// Returns `true` if `msg_id` is currently hidden behind an unrevealed spoiler veil.
    #[must_use]
    pub fn is_spoiler_hidden(&self, msg_id: MsgId) -> bool {
        self.descriptors
            .get(&msg_id)
            .is_some_and(|d| d.has_spoiler && !self.revealed_spoilers.contains_key(&msg_id))
    }

    /// Reveals a spoiler-covered media item at `now_secs`.
    pub fn reveal_spoiler(&mut self, msg_id: MsgId, now_secs: f64) {
        self.revealed_spoilers.insert(msg_id, now_secs);
    }

    /// Returns the spoiler veil opacity in `0.0..=1.0` (`1.0` hidden, fades to `0.0` after click).
    #[must_use]
    pub fn spoiler_opacity(&self, msg_id: MsgId, now_secs: f64) -> f32 {
        let Some(desc) = self.descriptors.get(&msg_id) else {
            return 0.0;
        };
        if !desc.has_spoiler {
            return 0.0;
        }
        match self.revealed_spoilers.get(&msg_id) {
            None => 1.0,
            Some(&revealed_at) => {
                let elapsed = (now_secs - revealed_at).max(0.0);
                (1.0 - (elapsed / SPOILER_REVEAL_FADE_SECS) as f32).clamp(0.0, 1.0)
            }
        }
    }

    /// Begins a new paint frame (clears per-frame visible set).
    pub fn begin_frame(&mut self, now_ms: i64) {
        self.last_now_ms = now_ms;
        self.visible_this_frame.clear();
    }

    /// Marks `msg_id` as visible in the viewport during the current frame.
    ///
    /// If it was previously paused offscreen, resumes it idempotently via
    /// [`InlinePlayback::set_item_paused`].
    pub fn mark_visible(&mut self, msg_id: MsgId) {
        self.visible_this_frame.insert(msg_id);
        if self.offscreen_paused.remove(&msg_id) {
            self.inline
                .set_item_paused(media_item_id_for_msg(msg_id), false, self.last_now_ms);
        }
    }

    /// Finishes the paint frame and pauses every inline media item that scrolled offscreen
    /// idempotently via [`InlinePlayback::set_item_paused`].
    pub fn finish_frame(&mut self, now_ms: i64) {
        for &prev_id in &self.visible_last_frame {
            if !self.visible_this_frame.contains(&prev_id) && self.offscreen_paused.insert(prev_id)
            {
                self.inline
                    .set_item_paused(media_item_id_for_msg(prev_id), true, now_ms);
            }
        }
        // Evict long-idle decoders (> 30s offscreen) to keep memory bounded.
        self.inline.evict_stale(now_ms, 30_000);
        // Stand-in frames for stickers being rendered: only what was on screen lately matters.
        if self.lottie_last_shown.len() > 512 {
            self.lottie_last_shown.clear();
        }
        std::mem::swap(&mut self.visible_last_frame, &mut self.visible_this_frame);
        self.visible_this_frame.clear();
    }

    /// Returns `true` if `msg_id` is currently paused because it is offscreen.
    #[must_use]
    pub fn is_paused_offscreen(&self, msg_id: MsgId) -> bool {
        self.offscreen_paused.contains(&msg_id)
    }

    /// Returns the number of cached Lottie frame textures across all sizes.
    #[must_use]
    pub fn lottie_frame_cache_len(&self) -> usize {
        self.lottie_size_frames.len()
    }

    /// Returns a reference to the top [`AudioPlayerBar`].
    #[must_use]
    pub const fn audio_player_bar(&self) -> &AudioPlayerBar {
        &self.audio_player_bar
    }

    /// Returns a mutable reference to the top [`AudioPlayerBar`].
    pub fn audio_player_bar_mut(&mut self) -> &mut AudioPlayerBar {
        &mut self.audio_player_bar
    }

    /// Returns `true` if `msg_id` is currently the unmuted (sound-on) round video.
    #[must_use]
    pub const fn is_round_video_unmuted(&self, msg_id: MsgId) -> bool {
        matches!(self.unmuted_round_msg, Some(id) if id.0 == msg_id.0)
    }

    /// Advances software audio playback by `delta_ms` (used in tests/screenshots/headless mode).
    pub fn advance_audio_ms(&mut self, delta_ms: i64) {
        self.audio_player_bar.advance_software_ms(delta_ms);
    }

    /// Renders the docked top [`AudioPlayerBar`] if an audio/voice/round-video track is active.
    pub fn show_top_audio_bar(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        now_ms: i64,
    ) -> Option<AudioPlayerBarEvent> {
        self.show_top_audio_bar_with_base(ui, None, palette, now_ms)
    }

    /// Renders the docked top [`AudioPlayerBar`] using `base` as fallback metadata backend so
    /// both chat-history tracks and shared-media tracks display their titles & performers in the
    /// single unified bar.
    pub fn show_top_audio_bar_with_base(
        &mut self,
        ui: &mut Ui,
        base: Option<&dyn MediaBackend>,
        palette: &Palette,
        now_ms: i64,
    ) -> Option<AudioPlayerBarEvent> {
        if !self.audio_player_bar.is_visible() {
            return None;
        }
        let composite = CompositeMetadataBackend {
            overlay: &self.audio_backend,
            base,
        };
        let ev = self.audio_player_bar.show(ui, &composite, palette, now_ms);
        if matches!(ev, Some(AudioPlayerBarEvent::Closed)) {
            self.unmuted_round_msg = None;
        }
        ev
    }

    /// Synchronizes voice, music, and round-video tracks from loaded messages into `tg-audio`.
    pub fn sync_peer_audio_tracks(&mut self, peer_id: PeerId, messages: &[HistoryMessage]) {
        self.active_peer = peer_id;
        let mut voice_entries = Vec::new();
        let mut song_entries = Vec::new();

        for msg in messages {
            let Some(ref media) = msg.media else {
                continue;
            };
            match media {
                HistoryMedia::Voice(v) => {
                    let audio_id = self.ensure_voice_registered(
                        peer_id,
                        msg.id,
                        &msg.sender_name,
                        v.duration_secs,
                        false,
                    );
                    voice_entries.push(PlaylistEntry::new(audio_id).with_unread(v.is_unread));
                }
                HistoryMedia::RoundVideo(rv) => {
                    let audio_id = self.ensure_voice_registered(
                        peer_id,
                        msg.id,
                        &msg.sender_name,
                        rv.duration_secs,
                        true,
                    );
                    voice_entries.push(PlaylistEntry::new(audio_id));
                }
                HistoryMedia::Document(doc)
                    if is_music_document(doc, self.descriptors.get(&msg.id)) =>
                {
                    let audio_id = self.ensure_music_registered(peer_id, msg.id, doc);
                    song_entries.push(PlaylistEntry::new(audio_id));
                }
                _ => {}
            }
        }

        if !voice_entries.is_empty() {
            self.audio_backend
                .set_playlist(peer_id, AudioType::Voice, voice_entries);
        }
        if !song_entries.is_empty() {
            self.audio_backend
                .set_playlist(peer_id, AudioType::Song, song_entries);
        }
    }

    /// Ensures a voice or round-video track is registered in `tg-audio` and returns its [`AudioMsgId`].
    pub fn ensure_voice_registered(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
        is_round: bool,
    ) -> AudioMsgId {
        if let Some(&existing) = self.registered_audio.get(&msg_id) {
            if !is_round {
                self.stream_registered_voice(msg_id);
            }
            return existing;
        }
        self.remember_voice_meta(msg_id, sender_name, duration_secs);
        let dur_ms = i64::from(duration_secs.max(1)) * 1000;
        let doc_id = DocumentId(0x5000_0000 + msg_id.0.unsigned_abs() as u64);
        let full = AudioFullMsgId::new(AudioPeerId(peer_id.0), AudioMsgIdNum(i64::from(msg_id.0)));
        let kind = if is_round {
            MediaKind::VideoMessage
        } else {
            MediaKind::VoiceMessage
        };
        let audio_id = AudioMsgId::new(doc_id, full, kind, dur_ms, 0);

        let subtitle = if is_round {
            "Video message".to_owned()
        } else {
            "Voice message".to_owned()
        };
        if self.streams_voice(msg_id) {
            // TGB-3000 / TGB-3116: not cached, so the track streams the file instead of a
            // stand-in tone. A round video plays at once, so it asks for its loader now.
            let open = self.voice_stream_opener(msg_id, is_round);
            if is_round {
                self.request_voice_stream(msg_id);
            }
            self.audio_backend.register_streamed_track(
                audio_id,
                open,
                AudioTrackMetadata {
                    id: audio_id,
                    title: sender_name.to_owned(),
                    subtitle,
                    duration_ms: dur_ms,
                },
            );
            self.registered_audio.insert(msg_id, audio_id);
            return audio_id;
        }

        let (bytes, ext) = self
            .descriptors
            .get(&msg_id)
            .and_then(|d| {
                d.audio_bytes
                    .clone()
                    .map(|b| (b, d.audio_ext.clone().or_else(|| Some("wav".to_owned()))))
            })
            .unwrap_or_else(|| {
                let freq = 260.0 + ((msg_id.0.unsigned_abs() % 7) as f32) * 35.0;
                (
                    synthesize_tone_wav(freq, duration_secs.clamp(2, 12) as f32),
                    Some("wav".to_owned()),
                )
            });

        self.audio_backend.register_track(
            audio_id,
            bytes,
            ext,
            AudioTrackMetadata {
                id: audio_id,
                title: sender_name.to_owned(),
                subtitle,
                duration_ms: dur_ms,
            },
        );
        self.registered_audio.insert(msg_id, audio_id);
        audio_id
    }

    /// Ensures a music document is registered in `tg-audio` and returns its [`AudioMsgId`].
    pub fn ensure_music_registered(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        doc: &DocumentAttachment,
    ) -> AudioMsgId {
        if let Some(&existing) = self.registered_audio.get(&msg_id) {
            return existing;
        }
        let (title, performer, dur_secs) = music_display_info(doc, self.descriptors.get(&msg_id));
        let dur_ms = i64::from(dur_secs.max(1)) * 1000;
        let doc_id = DocumentId(0x6000_0000 + msg_id.0.unsigned_abs() as u64);
        let full = AudioFullMsgId::new(AudioPeerId(peer_id.0), AudioMsgIdNum(i64::from(msg_id.0)));
        let audio_id = AudioMsgId::new(doc_id, full, MediaKind::AudioFile, dur_ms, 0);

        let (bytes, ext) = self
            .descriptors
            .get(&msg_id)
            .and_then(|d| {
                d.audio_bytes
                    .clone()
                    .map(|b| (b, d.audio_ext.clone().or_else(|| Some("wav".to_owned()))))
            })
            .unwrap_or_else(|| {
                let freq = 330.0 + ((msg_id.0.unsigned_abs() % 5) as f32) * 40.0;
                (
                    synthesize_tone_wav(freq, dur_secs.clamp(4, 12) as f32),
                    Some("wav".to_owned()),
                )
            });

        self.audio_backend.register_track(
            audio_id,
            bytes,
            ext,
            AudioTrackMetadata {
                id: audio_id,
                title,
                subtitle: performer,
                duration_ms: dur_ms,
            },
        );
        self.registered_audio.insert(msg_id, audio_id);
        audio_id
    }

    /// Toggles play/pause or seeks a voice message through `tg-audio::PlayerInstance`.
    pub fn toggle_or_seek_voice(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
        seek_frac: Option<f64>,
    ) {
        if self.voice_loaded(msg_id) == Some(false) {
            self.remember_voice_meta(msg_id, sender_name, duration_secs);
            if !self.streams_voice(msg_id) {
                // The backend cannot stream: the play starts the download (TGB-2958).
                self.request_voice_play(peer_id, msg_id, seek_frac);
                return;
            }
            // Not in the media cache: the track starts now and streams the file (TGB-3000).
            self.request_voice_stream(msg_id);
        }
        let audio_id =
            self.ensure_voice_registered(peer_id, msg_id, sender_name, duration_secs, false);
        let ctx = PlaylistContext {
            peer_id: AudioPeerId(peer_id.0),
            ..PlaylistContext::default()
        };
        let current = self.audio_player_bar.player().current(AudioType::Voice);
        let state = self.audio_player_bar.player().get_state(AudioType::Voice);
        let is_this = current == audio_id && !state.state.is_stopped_or_stopping();

        if !is_this {
            let _ = self.audio_player_bar.play_item(audio_id, Some(ctx));
            if let Some(frac) = seek_frac {
                let _ = self
                    .audio_player_bar
                    .player_mut()
                    .finish_seeking(AudioType::Voice, frac.clamp(0.0, 1.0));
            }
        } else if let Some(frac) = seek_frac {
            let _ = self
                .audio_player_bar
                .player_mut()
                .finish_seeking(AudioType::Voice, frac.clamp(0.0, 1.0));
        } else {
            let _ = self
                .audio_player_bar
                .player_mut()
                .play_pause(AudioType::Voice);
        }
    }

    /// Toggles play/pause or seeks a music file through `tg-audio::PlayerInstance`.
    pub fn toggle_or_seek_music(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        doc: &DocumentAttachment,
        seek_frac: Option<f64>,
    ) {
        let audio_id = self.ensure_music_registered(peer_id, msg_id, doc);
        let ctx = PlaylistContext {
            peer_id: AudioPeerId(peer_id.0),
            ..PlaylistContext::default()
        };
        let current = self.audio_player_bar.player().current(AudioType::Song);
        let state = self.audio_player_bar.player().get_state(AudioType::Song);
        let is_this = current == audio_id && !state.state.is_stopped_or_stopping();

        if !is_this {
            let _ = self.audio_player_bar.play_item(audio_id, Some(ctx));
            if let Some(frac) = seek_frac {
                let _ = self
                    .audio_player_bar
                    .player_mut()
                    .finish_seeking(AudioType::Song, frac.clamp(0.0, 1.0));
            }
        } else if let Some(frac) = seek_frac {
            let _ = self
                .audio_player_bar
                .player_mut()
                .finish_seeking(AudioType::Song, frac.clamp(0.0, 1.0));
        } else {
            let _ = self
                .audio_player_bar
                .player_mut()
                .play_pause(AudioType::Song);
        }
    }

    /// Switches a muted round video to sound-on playback (or toggles pause if already unmuted).
    pub fn click_round_video_sound(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
    ) {
        let now_ms = self.last_now_ms;
        self.click_round_video(peer_id, msg_id, sender_name, duration_secs, now_ms);
    }

    /// Seeks a round video message to normalized `progress` in `0.0..=1.0` via the circular seek
    /// ring (`history_view_video_message_seek.cpp`).
    pub fn seek_round_video(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        sender_name: &str,
        duration_secs: u32,
        progress: f64,
    ) {
        let clamped = progress.clamp(0.0, 1.0);
        self.round_seek_positions.insert(msg_id, clamped);
        self.round_seeks
            .entry(msg_id)
            .or_default()
            .set_progress(clamped);
        let audio_id =
            self.ensure_voice_registered(peer_id, msg_id, sender_name, duration_secs, true);
        let ctx = PlaylistContext {
            peer_id: AudioPeerId(peer_id.0),
            ..PlaylistContext::default()
        };
        if self.unmuted_round_msg != Some(msg_id) {
            self.unmuted_round_msg = Some(msg_id);
            self.round_sound_seen_active = false;
            let _ = self.audio_player_bar.play_item(audio_id, Some(ctx));
        }
        let _ = self
            .audio_player_bar
            .player_mut()
            .finish_seeking(AudioType::Voice, clamped);
        let position_ms = (clamped * f64::from(duration_secs) * 1000.0) as i64;
        self.inline
            .seek_item(media_item_id_for_msg(msg_id), position_ms, self.last_now_ms);
    }

    /// Returns the last seek progress (`0.0..=1.0`) recorded for round video `msg_id`, if any.
    #[must_use]
    pub fn round_video_seek_progress(&self, msg_id: MsgId) -> Option<f64> {
        self.round_seek_positions.get(&msg_id).copied()
    }

    /// Returns mutable access to the per-message [`crate::history::video_message_seek::VideoMessageSeek`].
    pub fn round_seek_mut(
        &mut self,
        msg_id: MsgId,
    ) -> &mut crate::history::video_message_seek::VideoMessageSeek {
        self.round_seeks.entry(msg_id).or_default()
    }

    /// Cycles playback speed (`1x -> 1.5x -> 2x -> 1x`) on `PlayerInstance`.
    pub fn cycle_audio_speed(&mut self, audio_type: AudioType) -> f64 {
        let cur_id = self.audio_player_bar.player().current(audio_type);
        let cur = self
            .audio_player_bar
            .player()
            .lookup_playback_speed(&cur_id);
        let next = INLINE_AUDIO_SPEED_PRESETS
            .iter()
            .copied()
            .find(|&s| s > cur + 0.05)
            .unwrap_or(INLINE_AUDIO_SPEED_PRESETS[0]);
        self.audio_player_bar.player_mut().set_playback_speed(next);
        next
    }

    /// Returns `(is_active_item, is_playing, progress, speed, track_state)` for a voice or music message.
    #[must_use]
    pub fn audio_item_state(
        &self,
        msg_id: MsgId,
        audio_type: AudioType,
    ) -> (bool, bool, f32, f64, TrackState) {
        let state = self.audio_player_bar.player().get_state(audio_type);
        let current = self.audio_player_bar.player().current(audio_type);
        let is_this = self
            .registered_audio
            .get(&msg_id)
            .is_some_and(|&id| id == current && !state.state.is_stopped_or_stopping());
        let is_playing = is_this && state.state.show_pause_icon();
        let progress = if is_this {
            state.progress() as f32
        } else {
            0.0
        };
        let speed = self
            .audio_player_bar
            .player()
            .lookup_playback_speed(&current);
        (is_this, is_playing, progress, speed, state)
    }

    /// Queries the next decoded frame for an inline GIF, round video, or WebM VP9 alpha sticker
    /// via `tg_app_media::media::inline::InlinePlayback` (`tg-video::ClipReader`).
    pub fn inline_clip_frame(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        kind: InlinePlaybackKind,
        target_size: Option<(u32, u32)>,
        now_ms: i64,
    ) -> Option<InlineFrameState> {
        self.mark_visible(msg_id);
        let item_id = media_item_id_for_msg(msg_id);
        let video_data = self.descriptors.get(&msg_id)?.video_data.as_ref()?;
        // An open decoder only needs the payload to be created: don't copy the whole video or
        // GIF on every paint once it exists.
        let data = if self.inline.item_position_ms(item_id).is_some() {
            InlineDocumentData::Bytes(Vec::new())
        } else {
            video_data.clone()
        };
        // Respect per-kind autoplay settings from CoreSettings.
        let allowed = self.policy.allows_inline(kind);
        match kind {
            InlinePlaybackKind::Gif | InlinePlaybackKind::VideoSticker => {
                self.inline.set_autoplay_gifs(allowed);
            }
            InlinePlaybackKind::RoundVideo => {
                self.inline.set_autoplay_round_videos(allowed);
            }
        }
        let source = InlineMediaSource {
            id: item_id,
            kind,
            data,
            target_size,
        };
        Some(self.inline.frame_for(ctx, &source, now_ms))
    }

    /// Generates and caches a video thumbnail via `tg-video` when a video has no `stripped_thumb`.
    pub fn get_or_generate_video_thumb(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        max_side: u32,
        round_circle: bool,
    ) -> Option<TextureHandle> {
        self.pump_decoded(ctx);
        if let Some(cached) = self.video_thumbs.get(&msg_id) {
            return cached.clone();
        }
        let revision = self.media_revision(msg_id);
        if self.video_thumbs_pending.get(&msg_id) == Some(&revision) {
            // Being generated: the caller paints the placeholder surface meanwhile.
            return None;
        }
        let data = self.descriptors.get(&msg_id)?.video_data.clone()?;
        self.video_thumbs_pending.insert(msg_id, revision);
        self.run_decode(
            ctx,
            DecodeJob::VideoThumb {
                msg_id,
                revision,
                data,
                max_side,
                round_circle,
            },
        );
        self.video_thumbs.peek(&msg_id).cloned().flatten()
    }

    /// Renders and caches an animated Lottie sticker frame keyed by `(sticker_id, size_px, step_idx)`.
    ///
    /// Respects [`AutoplayPolicy::allows_lottie_loop`] and pauses offscreen. `None` while the
    /// sticker is parsed or rendered in the background, or when it does not parse; see
    /// [`Self::lottie_frame_per_size`] to tell those apart.
    pub fn get_lottie_frame_per_size(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        sticker_id: u64,
        lottie_json: &str,
        time_secs: f64,
        render_px: u32,
    ) -> Option<(TextureHandle, bool)> {
        match self.lottie_frame_per_size(ctx, msg_id, sticker_id, lottie_json, time_secs, render_px)
        {
            LottieFrameState::Ready { texture, animated } => Some((texture, animated)),
            LottieFrameState::Pending | LottieFrameState::Unavailable => None,
        }
    }

    /// Frame of an animated Lottie sticker for this paint, keyed by
    /// `(sticker_id, size_px, step_idx)`: parsing and rasterising run on the decode workers, the
    /// step timing (24-step quantisation, loop policy) is unchanged.
    pub fn lottie_frame_per_size(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        sticker_id: u64,
        lottie_json: &str,
        time_secs: f64,
        render_px: u32,
    ) -> LottieFrameState {
        self.lottie_frame_colored(
            ctx,
            msg_id,
            sticker_id,
            lottie_json,
            time_secs,
            render_px,
            None,
        )
    }

    /// [`Self::lottie_frame_per_size`] with the frames coloured with `overlay`: a selected
    /// message's sticker gets `msgStickerOverlay` (`Sticker::paintLottie` asks its frames
    /// `.colored`), cached apart from the plain frames.
    #[allow(clippy::too_many_arguments)]
    pub fn lottie_frame_colored(
        &mut self,
        ctx: &egui::Context,
        msg_id: MsgId,
        sticker_id: u64,
        lottie_json: &str,
        time_secs: f64,
        render_px: u32,
        overlay: Option<Color32>,
    ) -> LottieFrameState {
        self.mark_visible(msg_id);
        self.pump_decoded(ctx);
        // Quantize render size to 16px buckets so each size has its own crisp cached frames.
        let size_bucket = render_px.clamp(32, 256).div_ceil(16) * 16;
        let last_shown = |rt: &Self| {
            rt.lottie_last_shown
                .get(&(sticker_id, size_bucket, overlay))
                .cloned()
                .map_or(LottieFrameState::Pending, |texture| {
                    LottieFrameState::Ready {
                        texture,
                        animated: true,
                    }
                })
        };

        if !self.lottie_comps.contains(&sticker_id) && self.lottie_parsing.insert(sticker_id) {
            self.run_decode(
                ctx,
                DecodeJob::LottieParse {
                    sticker_id,
                    json: Arc::from(lottie_json),
                },
            );
        }
        let comp = match self.lottie_comps.get(&sticker_id) {
            Some(Some(comp)) => Arc::clone(comp),
            Some(None) => return LottieFrameState::Unavailable,
            None => return last_shown(self),
        };
        let frame_span = (comp.out_point - comp.in_point).max(1.0);
        let fps = comp.frame_rate.clamp(1.0, 60.0);
        let total_steps = (frame_span as u32).clamp(1, 24);
        let duration_secs = frame_span / fps;

        let looping = self.policy.allows_lottie_loop();
        let autoplay = self.policy.allows_lottie_autoplay();

        let (step_idx, still_animating) = if looping {
            let started_at = *self.lottie_started_at.entry(msg_id).or_insert(time_secs);
            let local_secs = (time_secs - started_at).max(0.0);
            let elapsed_frames = (local_secs * fps) % frame_span;
            let idx = ((elapsed_frames / frame_span) * f64::from(total_steps)).floor() as u32
                % total_steps;
            (idx, total_steps > 1)
        } else if let Some(&started_at) = self.lottie_started_at.get(&msg_id) {
            let local_secs = (time_secs - started_at).max(0.0);
            if local_secs < duration_secs {
                let elapsed_frames = (local_secs * fps).min(frame_span - 0.001);
                let idx = ((elapsed_frames / frame_span) * f64::from(total_steps)).floor() as u32
                    % total_steps;
                (idx, total_steps > 1)
            } else {
                (0, false)
            }
        } else if autoplay {
            self.lottie_started_at.insert(msg_id, time_secs);
            (0, total_steps > 1)
        } else {
            (0, false)
        };

        let key = (sticker_id, size_bucket, step_idx, overlay);
        if let Some(tex) = self.lottie_size_frames.get(&key).cloned() {
            self.lottie_last_shown
                .insert((sticker_id, size_bucket, overlay), tex.clone());
            return LottieFrameState::Ready {
                texture: tex,
                animated: still_animating,
            };
        }

        let sample_frame_for =
            |step: u32| comp.in_point + (f64::from(step) / f64::from(total_steps)) * frame_span;
        // In the background the other steps of an animated sticker are queued first, so the
        // newest job (the step needed now) is picked up before them.
        if self.decode.is_background()
            && still_animating
            && self
                .lottie_prefetched
                .insert((sticker_id, size_bucket, overlay))
        {
            for step in (0..total_steps).rev().filter(|&step| step != step_idx) {
                let other = (sticker_id, size_bucket, step, overlay);
                if !self.lottie_size_frames.contains(&other) && self.lottie_rendering.insert(other)
                {
                    self.run_decode(
                        ctx,
                        DecodeJob::LottieFrame {
                            key: other,
                            comp: Arc::clone(&comp),
                            sample_frame: sample_frame_for(step),
                        },
                    );
                }
            }
        }
        if self.lottie_rendering.insert(key) {
            self.run_decode(
                ctx,
                DecodeJob::LottieFrame {
                    key,
                    comp: Arc::clone(&comp),
                    sample_frame: sample_frame_for(step_idx),
                },
            );
        }
        if let Some(tex) = self.lottie_size_frames.get(&key).cloned() {
            self.lottie_last_shown
                .insert((sticker_id, size_bucket, overlay), tex.clone());
            return LottieFrameState::Ready {
                texture: tex,
                animated: still_animating,
            };
        }
        match last_shown(self) {
            LottieFrameState::Ready { texture, .. } => LottieFrameState::Ready {
                texture,
                animated: still_animating,
            },
            other => other,
        }
    }

    /// Replays a non-looping animated sticker from frame 0 (e.g. on click).
    pub fn restart_lottie(&mut self, msg_id: MsgId, now_secs: f64) {
        self.lottie_started_at.insert(msg_id, now_secs);
    }

    /// Paints a static (WebP) sticker from its downloaded file, whole and centered in `rect`:
    /// no blurred backdrop, no download button or loading arc (`Sticker::paintPixmap`).
    /// Returns `false` when the message has no static sticker file to show.
    pub fn paint_static_sticker(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        now_secs: f64,
    ) -> bool {
        self.pump_decoded(ctx);
        let ready = match self
            .descriptors
            .get(&msg_id)
            .and_then(|d| d.photo_load.as_ref())
        {
            None => return false,
            Some(PhotoLoadState::Ready { .. }) => true,
            Some(_) => false,
        };
        let corners = CornerRadius::ZERO;
        if ready {
            self.paint_full_photo(
                ctx,
                painter,
                msg_id,
                rect,
                corners,
                PhotoFit::Contain,
                now_secs,
            );
        } else {
            self.paint_full_photo_cached(
                ctx,
                painter,
                msg_id,
                rect,
                corners,
                PhotoFit::Contain,
                now_secs,
            );
        }
        true
    }

    /// Whether the message's media is a live sticker that has a descriptor (its file comes
    /// from the session), as opposed to demo data drawn from its Lottie source alone.
    #[must_use]
    pub fn has_descriptor(&self, msg_id: MsgId) -> bool {
        self.descriptors.contains_key(&msg_id)
    }

    /// Paints a photo's full picture over its blurred thumbnail once it is
    /// [`PhotoLoadState::Ready`] (fading in, placed by [`photo_frame`] for `fit` so it keeps its
    /// aspect ratio), and the loading arc while it downloads (`Photo::draw`).
    ///
    /// The arc turns and eases toward the progress every frame, so while it is painted this
    /// asks for the next frame at [`RADIAL_FRAME`]; a photo that is not painted asks for none.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_photo_load_and_blur_up(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        rounding: CornerRadius,
        fit: PhotoFit,
        now_secs: f64,
        over: f32,
    ) {
        self.pump_decoded(ctx);
        // Borrow the state: the encoded bytes are never copied by a paint.
        let progress = match self
            .descriptors
            .get(&msg_id)
            .and_then(|d| d.photo_load.as_ref())
        {
            // Not loaded, or still being read from the disk cache: the blurred thumbnail
            // painted by the caller is all there is.
            None | Some(PhotoLoadState::Pending) => {
                self.photo_radials.remove(&msg_id);
                return;
            }
            Some(PhotoLoadState::NotLoaded) => {
                self.photo_radials.remove(&msg_id);
                Self::paint_photo_download(painter, rect, over);
                return;
            }
            Some(PhotoLoadState::Loading { progress, .. }) => Some(progress.unwrap_or(0.0)),
            Some(PhotoLoadState::Ready { .. }) => None,
        };
        let loading = progress.is_some();
        if loading {
            self.paint_full_photo_cached(ctx, painter, msg_id, rect, rounding, fit, now_secs);
        } else {
            self.paint_full_photo(ctx, painter, msg_id, rect, rounding, fit, now_secs);
        }
        self.paint_photo_radial(
            painter,
            msg_id,
            rect,
            progress.map(f64::from),
            now_secs,
            over,
        );
    }

    /// A photo still downloading shows its full picture only when one is already decoded (a
    /// re-download); usually there is none and the blurred thumbnail stays.
    #[allow(clippy::too_many_arguments)]
    fn paint_full_photo_cached(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        rounding: CornerRadius,
        fit: PhotoFit,
        now_secs: f64,
    ) {
        if self.full_photos.peek(&msg_id).is_some() {
            self.draw_full_photo(ctx, painter, msg_id, rect, rounding, fit, now_secs);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_full_photo(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        rounding: CornerRadius,
        fit: PhotoFit,
        now_secs: f64,
    ) {
        let want_side = photo_decode_side(rect, ctx.pixels_per_point());
        let needs_decode = self
            .full_photos
            .peek(&msg_id)
            .is_none_or(|e| e.downscaled && e.max_side < want_side);
        let revision = self.media_revision(msg_id);
        let in_flight = self
            .photos_pending
            .get(&msg_id)
            .is_some_and(|&(rev, side)| rev == revision && side >= want_side);
        let failed = self.photos_failed.get(&msg_id) == Some(&revision);
        if needs_decode
            && !in_flight
            && !failed
            && let Some(bytes) = self
                .descriptors
                .get(&msg_id)
                .and_then(photo_ready_bytes)
                .map(Arc::clone)
        {
            self.photos_pending.insert(msg_id, (revision, want_side));
            self.run_decode(
                ctx,
                DecodeJob::Photo {
                    msg_id,
                    revision,
                    bytes,
                    max_side: want_side,
                },
            );
        }
        self.draw_full_photo(ctx, painter, msg_id, rect, rounding, fit, now_secs);
    }

    /// Draws the decoded picture into `rect` keeping its aspect ratio
    /// (`PrepareWithBlurredBackground`): cropped to cover the box, or centered whole with the
    /// blurred thumbnail around it.
    #[allow(clippy::too_many_arguments)]
    fn draw_full_photo(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        rounding: CornerRadius,
        fit: PhotoFit,
        now_secs: f64,
    ) {
        let Some(entry) = self.full_photos.get_mut(&msg_id) else {
            return;
        };
        let started_at = *entry.started_at_secs.get_or_insert(now_secs);
        let t = ((now_secs - started_at) / PHOTO_BLUR_UP_DURATION_SECS).clamp(0.0, 1.0) as f32;
        let alpha = (t * 255.0).round() as u8;
        let tint = Color32::from_rgba_unmultiplied(255, 255, 255, alpha);
        let frame = photo_frame(rect, entry.texture.size_vec2(), fit);
        let corners = if frame.letterboxed {
            CornerRadius::ZERO
        } else {
            rounding
        };
        painter.add(
            egui::epaint::RectShape::filled(frame.rect, corners, tint)
                .with_texture(entry.texture.id(), frame.uv),
        );
        if t < 1.0 {
            ctx.request_repaint();
        }
    }

    /// The loading arc (`Photo::draw`, `paintInCenter`): a `msgDateImgBg` circle of
    /// `msgFileLayout.thumbSize` with the cancel cross, and the `Ui::RadialAnimation` arc inside
    /// it. After the load finishes the arc completes and fades out, then the entry is dropped.
    fn paint_photo_radial(
        &mut self,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        progress: Option<f64>,
        now_secs: f64,
        over: f32,
    ) {
        let now_ms = now_secs * 1000.0;
        if let Some(progress) = progress {
            let radial = self.photo_radials.entry(msg_id).or_default();
            if !radial.animating() {
                radial.start(progress, now_ms);
            }
        }
        let Some(radial) = self.photo_radials.get_mut(&msg_id) else {
            return;
        };
        radial.update(progress.unwrap_or(1.0), progress.is_none(), now_ms);
        if !radial.animating() {
            self.photo_radials.remove(&msg_id);
            return;
        }
        let circle_opacity = if progress.is_some() {
            1.0
        } else {
            radial.opacity() as f32
        };
        let inner = Rect::from_center_size(rect.center(), Vec2::splat(PHOTO_RADIAL_SIZE));
        painter.circle_filled(
            inner.center(),
            PHOTO_RADIAL_SIZE / 2.0,
            downloads::circle_bg(if progress.is_some() { over } else { 0.0 })
                .gamma_multiply(circle_opacity),
        );
        paint_cancel_cross(
            painter,
            inner.center(),
            Color32::WHITE.gamma_multiply(circle_opacity),
        );
        radial.draw(
            painter,
            inner.shrink(PHOTO_RADIAL_LINE),
            PHOTO_RADIAL_LINE,
            Color32::WHITE,
        );
        painter.ctx().request_repaint_after(RADIAL_FRAME);
    }

    /// The loading arc state of `msg_id` while it animates (tests).
    #[cfg(test)]
    pub(crate) fn photo_radial_state(&self, msg_id: MsgId) -> Option<tg_ui::effects::RadialState> {
        self.photo_radials
            .get(&msg_id)
            .filter(|r| r.animating())
            .map(RadialAnimation::compute_state)
    }

    /// Paints the particle cover of a media spoiler over `rect` (`Media::fillImageSpoiler`) at
    /// `1 - revealAnimation` opacity; the caller paints `_spoiler->background` under it. A
    /// plain spoiler has no label: tdesktop draws a tag only for sensitive or paid media.
    /// With `PowerSaving::kChatSpoiler` the particles stay still.
    ///
    /// Returns `true` if the spoiler is still obscuring the media (so clicking should reveal it).
    pub fn paint_spoiler_overlay(
        &self,
        ctx: &egui::Context,
        painter: &Painter,
        msg_id: MsgId,
        rect: Rect,
        cr: CornerRadius,
        now_secs: f64,
    ) -> bool {
        let opacity = self.spoiler_opacity(msg_id, now_secs);
        if opacity <= 0.0 {
            return false;
        }
        if opacity < 1.0 {
            ctx.request_repaint();
        }
        paint_image_spoiler(
            ctx,
            painter,
            rect,
            cr,
            opacity,
            self.policy.allows_spoiler_particles(),
        );
        self.is_spoiler_hidden(msg_id)
    }
}

/// Converts a history [`MsgId`] into a deterministic [`MediaItemId`] for [`InlinePlayback`].
#[inline]
#[must_use]
pub const fn media_item_id_for_msg(msg_id: MsgId) -> MediaItemId {
    MediaItemId(0x7000_0000_u64.wrapping_add(msg_id.0.unsigned_abs() as u64))
}

/// Returns `true` if `doc` should be rendered as an inline music player bubble rather than a
/// generic file download bubble.
#[must_use]
pub fn is_music_document(
    doc: &DocumentAttachment,
    descriptor: Option<&InlineMediaDescriptor>,
) -> bool {
    if descriptor.is_some_and(|d| d.music_title.is_some() || d.audio_bytes.is_some()) {
        return true;
    }
    let mime = doc.mime_type.to_ascii_lowercase();
    if mime.starts_with("audio/") {
        return true;
    }
    let lower = doc.file_name.to_ascii_lowercase();
    lower.ends_with(".mp3")
        || lower.ends_with(".ogg")
        || lower.ends_with(".flac")
        || lower.ends_with(".wav")
        || lower.ends_with(".m4a")
        || lower.ends_with(".opus")
}

/// Extracts `(title, performer, duration_secs)` for a music document bubble.
#[must_use]
pub fn music_display_info(
    doc: &DocumentAttachment,
    descriptor: Option<&InlineMediaDescriptor>,
) -> (String, String, u32) {
    let default_dur = descriptor
        .and_then(|d| d.music_duration_secs)
        .unwrap_or(184);
    if let Some(d) = descriptor
        && let Some(ref t) = d.music_title
    {
        return (
            t.clone(),
            d.music_performer
                .clone()
                .unwrap_or_else(|| "Unknown Artist".to_owned()),
            default_dur,
        );
    }
    let stem = doc
        .file_name
        .rsplit_once('.')
        .map_or(doc.file_name.as_str(), |(s, _)| s);
    if let Some((artist, title)) = stem.split_once(" - ") {
        (
            title.trim().to_owned(),
            artist.trim().to_owned(),
            default_dur,
        )
    } else {
        (
            stem.to_owned(),
            format_file_size(doc.size_bytes),
            default_dur,
        )
    }
}

/// Paints an interactive inline music file bubble driven by `tg-audio::PlayerInstance`.
#[allow(clippy::too_many_arguments)]
pub fn paint_music_document_bubble(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    doc: &DocumentAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
    runtime: &mut HistoryMediaRuntime,
) {
    let accent = if outgoing && !colors.is_night {
        colors.wave_out_active
    } else {
        colors.accent
    };
    let inactive = if outgoing {
        colors.wave_out_inactive
    } else {
        colors.wave_in_inactive
    };
    let status_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.sub_text_fg
    };

    let (title, performer, dur_secs) = music_display_info(doc, runtime.descriptor(msg_id));
    let (is_active, is_playing, progress, speed, track_state) =
        runtime.audio_item_state(msg_id, AudioType::Song);
    if is_playing {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(40));
    }

    let btn_size = 44.0;
    let btn_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 10.0, rect.top() + 8.0),
        Vec2::splat(btn_size),
    );
    ui.painter()
        .circle_filled(btn_rect.center(), btn_size * 0.5, accent);
    let c = btn_rect.center();
    if is_playing {
        ui.painter().rect_filled(
            Rect::from_center_size(Pos2::new(c.x - 3.6, c.y), Vec2::new(3.2, 13.0)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
        ui.painter().rect_filled(
            Rect::from_center_size(Pos2::new(c.x + 3.6, c.y), Vec2::new(3.2, 13.0)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
    } else {
        let pts = vec![
            Pos2::new(c.x - 4.0, c.y - 7.5),
            Pos2::new(c.x + 8.0, c.y),
            Pos2::new(c.x - 4.0, c.y + 7.5),
        ];
        ui.painter()
            .add(Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
    }

    let btn_resp = ui.interact(
        btn_rect,
        Id::unique(("tg_music_play", msg_id.0)),
        Sense::click(),
    );
    if btn_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if btn_resp.clicked() {
        runtime.toggle_or_seek_music(peer_id, msg_id, doc, None);
    }

    let text_left = btn_rect.right() + 10.0;
    let speed_w = if is_active { 34.0 } else { 0.0 };
    let text_w = (rect.right() - text_left - 10.0 - speed_w).max(60.0);

    // Title
    let title_galley =
        ui.painter()
            .layout_no_wrap(title, FontId::proportional(13.5), colors.bold_fg);
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_left, rect.top() + 8.0),
            Vec2::new(text_w, 18.0),
        ))
        .galley(
            Pos2::new(text_left, rect.top() + 8.0),
            title_galley,
            colors.bold_fg,
        );

    // Interactive seek bar when active, or performer subtitle when idle.
    let sub_y = rect.top() + 28.0;
    if is_active {
        let track_rect =
            Rect::from_min_size(Pos2::new(text_left, sub_y + 4.0), Vec2::new(text_w, 4.0));
        ui.painter()
            .rect_filled(track_rect, CornerRadius::same(2), inactive);
        let fill_w = track_rect.width() * progress.clamp(0.0, 1.0);
        ui.painter().rect_filled(
            Rect::from_min_size(track_rect.min, Vec2::new(fill_w, 4.0)),
            CornerRadius::same(2),
            accent,
        );
        ui.painter().circle_filled(
            Pos2::new(track_rect.left() + fill_w, track_rect.center().y),
            5.0,
            accent,
        );

        let hit_rect = track_rect.expand2(Vec2::new(0.0, 6.0));
        let seek_resp = ui.interact(
            hit_rect,
            Id::unique(("tg_music_seek", msg_id.0)),
            Sense::click_and_drag(),
        );
        if seek_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if (seek_resp.clicked() || seek_resp.dragged())
            && let Some(ptr) = seek_resp.interact_pointer_pos()
        {
            let frac = f64::from(
                ((ptr.x - track_rect.left()) / track_rect.width().max(1.0)).clamp(0.0, 1.0),
            );
            runtime.toggle_or_seek_music(peer_id, msg_id, doc, Some(frac));
        }

        let elapsed = (track_state.position_ms().max(0) / 1000) as u32;
        let total = (track_state.length_ms().max(i64::from(dur_secs) * 1000) / 1000) as u32;
        ui.painter().text(
            Pos2::new(text_left, sub_y + 12.0),
            Align2::LEFT_TOP,
            format!(
                "{} / {} · {}",
                format_duration(elapsed),
                format_duration(total),
                performer
            ),
            FontId::proportional(11.0),
            status_fg,
        );

        // Inline speed chip (`1x`, `1.5x`, `2x`).
        let spd_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 40.0, rect.top() + 10.0),
            Vec2::new(32.0, 20.0),
        );
        let spd_active = (speed - 1.0).abs() > 0.05;
        ui.painter().rect_filled(
            spd_rect,
            CornerRadius::same(6),
            if spd_active { accent } else { inactive },
        );
        let spd_label = format!("{:.1}x", speed).replace(".0x", "x");
        ui.painter().text(
            spd_rect.center(),
            Align2::CENTER_CENTER,
            spd_label,
            FontId::proportional(10.5),
            if spd_active { Color32::WHITE } else { accent },
        );
        let spd_resp = ui.interact(
            spd_rect,
            Id::unique(("tg_music_spd", msg_id.0)),
            Sense::click(),
        );
        if spd_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if spd_resp.on_hover_text("Change playback speed").clicked() {
            runtime.cycle_audio_speed(AudioType::Song);
        }
    } else {
        ui.painter().text(
            Pos2::new(text_left, sub_y),
            Align2::LEFT_TOP,
            format!("{performer} · {}", format_duration(dur_secs)),
            FontId::proportional(12.0),
            status_fg,
        );
    }
}

/// Pixel bytes a finished decode uploads (counted against the per-frame upload budget).
fn decode_output_bytes(output: &DecodeOutput) -> usize {
    match output {
        DecodeOutput::Photo { image, .. } => {
            image.as_ref().map_or(0, |(img, _)| texture_bytes(img.size))
        }
        DecodeOutput::VideoThumb { image, .. } | DecodeOutput::RoundThumb { image, .. } => {
            image.as_ref().map_or(0, |img| texture_bytes(img.size))
        }
        DecodeOutput::LottieParsed { .. } => 0,
        DecodeOutput::LottieFrame { image, .. } => texture_bytes(image.size),
    }
}

/// Decodes inline photo bytes into `(width, height, rgba)` via [`tg_media::image::read_image`],
/// enforcing tdesktop's input byte limit and pixel-area cap before pixel allocation.
pub fn decode_inline_photo_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: bytes.to_vec(),
        ..Default::default()
    })
    .ok()?;
    let (w, h) = decoded.image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, decoded.image.into_raw()))
}

/// Synthesizes a clean deterministic WAV tone buffer (48kHz mono PCM16) for mock voice/music
/// items that don't carry raw encoded audio bytes on disk.
#[must_use]
pub fn synthesize_tone_wav(base_hz: f32, duration_secs: f32) -> Vec<u8> {
    let sample_rate = 48_000_u32;
    let frames = (sample_rate as f32 * duration_secs.clamp(0.5, 15.0)) as usize;
    let mut pcm = Vec::with_capacity(frames);
    let freqs = [base_hz, base_hz * 1.25, base_hz * 1.5];
    for i in 0..frames {
        let t = i as f32 / sample_rate as f32;
        let env = (t * 6.0).min(1.0) * ((duration_secs - t) * 6.0).clamp(0.0, 1.0);
        let sum: f32 = freqs
            .iter()
            .map(|&f| (2.0 * std::f32::consts::PI * f * t).sin())
            .sum();
        let sample = ((sum / freqs.len() as f32) * 0.28 * env * f32::from(i16::MAX)) as i16;
        pcm.push(sample);
    }
    encode_wav_pcm_s16le(&pcm, sample_rate, 1)
}

/// `historyFileThumbCancel` (`history_file_cancel.png`): a 10 px cross with 2 px strokes.
fn paint_cancel_cross(painter: &Painter, center: Pos2, color: Color32) {
    let half = 5.0;
    let stroke = Stroke::new(2.0, color);
    painter.line_segment(
        [
            center + Vec2::new(-half, -half),
            center + Vec2::new(half, half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + Vec2::new(half, -half),
            center + Vec2::new(-half, half),
        ],
        stroke,
    );
}

#[cfg(test)]
mod tests_sec2a {
    use super::*;

    #[derive(Default)]
    struct RecordingBase {
        calls: Mutex<Vec<&'static str>>,
    }

    impl MediaBackend for RecordingBase {
        fn get_item(&self, _id: MediaItemId) -> Option<MediaItem> {
            None
        }
        fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
            Vec::new()
        }
        fn load_bytes(&self, _id: MediaItemId) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("unused")
        }
        fn open_range_loader(
            &self,
            _id: MediaItemId,
        ) -> anyhow::Result<Arc<dyn tg_video::RangeLoader>> {
            anyhow::bail!("unused")
        }
        fn save_to_disk(
            &self,
            _id: MediaItemId,
            _destination: &std::path::Path,
        ) -> anyhow::Result<PathBuf> {
            anyhow::bail!("unused")
        }
        fn copy_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
            Ok(())
        }
        fn forward_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
            Ok(())
        }
        fn delete_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
            Ok(())
        }
        fn show_in_chat(&self, _peer_id: i64, _msg_id: i32) {}
        fn open_in_external_app(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
            self.calls.lock().expect("calls").push("open");
            Ok(PathBuf::from("warned"))
        }
        fn open_in_external_app_confirmed(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
            self.calls.lock().expect("calls").push("confirmed");
            Ok(PathBuf::from("launched"))
        }
        fn audio_track_metadata(&self, _id: AudioMsgId) -> Option<AudioTrackMetadata> {
            None
        }
        fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
            Arc::new(MemoryAudioSource::new())
        }
        fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
            Arc::new(InMemoryPlaylistProvider::new())
        }
    }

    #[test]
    fn l8_confirmed_external_launch_reaches_the_base_backend() {
        let base = RecordingBase::default();
        let overlay = SharedAudioBackend::new();
        let composite = CompositeMetadataBackend {
            overlay: &overlay,
            base: Some(&base),
        };
        let id = MediaItemId::default();
        let launched = composite
            .open_in_external_app_confirmed(id)
            .expect("launch");
        assert_eq!(launched, PathBuf::from("launched"));
        assert_eq!(*base.calls.lock().expect("calls"), vec!["confirmed"]);
    }
}
