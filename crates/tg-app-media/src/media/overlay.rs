//! Fullscreen media viewer overlay for photos, videos, GIFs, and documents.
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view_overlay_widget.h` and
//! `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Area, Color32, ColorImage, CornerRadius, CursorIcon, FontFamily, FontId, Id, Key,
    Order, Pos2, Rect, Role, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo,
};
use image::DynamicImage;
use tg_app_core::chat_helpers::backend::AttachedStickersMedia;
use tg_text::{EntityType, TextWithEntities, utf16_range_to_utf8};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::tg_style::st;
use tg_video::{
    FfmpegStreamDecoder, FrameData, FramePopOutcome, OutputPixelFormat, PlaybackMode,
    PlaybackOptions, PlayerStage, SliceConfig, StreamingPlayer,
};

use crate::media::backend::{MediaBackend, MediaItem, MediaItemId, MediaItemKind};
use crate::media::controls::{PlaybackControlsAction, PlaybackControlsState, TimestampChapter};
use crate::media::group_thumbs::{GroupThumbsStrip, album_strip_range};
use crate::media::ocr_selection::{
    OcrOverlayState, OcrSelection, image_rect_to_screen, recognized_text_in_reading_order,
};
use crate::media::pip::{PipRect, PipResumeState, PipWindow};
use crate::media::strings;
use crate::media::style::{
    MEDIAVIEW_BG, MEDIAVIEW_CAPTION_BG, MEDIAVIEW_CAPTION_FG, MEDIAVIEW_CAPTION_MARGIN,
    MEDIAVIEW_CAPTION_PAD_X, MEDIAVIEW_CAPTION_PAD_Y, MEDIAVIEW_CAPTION_RADIUS,
    MEDIAVIEW_CONTROL_BG, MEDIAVIEW_CONTROL_BG_OVER, MEDIAVIEW_CONTROL_FG,
    MEDIAVIEW_HIDE_DURATION_MS, MEDIAVIEW_MENU_BG, MEDIAVIEW_MENU_BG_OVER,
    MEDIAVIEW_MENU_DANGER_FG, MEDIAVIEW_MENU_FG, MEDIAVIEW_SAVE_MSG_BG, MEDIAVIEW_SAVE_MSG_FG,
    MEDIAVIEW_SAVE_MSG_SHOWN_MS, MEDIAVIEW_SHOW_DURATION_MS, MEDIAVIEW_SUB_FG,
    MEDIAVIEW_TEXT_LINK_FG, MEDIAVIEW_VIDEO_BG, MEDIAVIEW_WAIT_HIDE_MS, MediaColors,
    paint_centered_style_icon, paint_centered_style_icon_alpha, paint_transparent_checkerboard,
    with_opacity,
};
use crate::media::video_stream::{
    PlaybackSponsoredEvent, PlaybackSponsoredOverlay, SponsoredForVideo, ThemeFilePreview,
    ThemePreviewAction, VideoStreamAction, VideoStreamDescriptor, VideoStreamViewer, is_theme_file,
};
use crate::media::zoom::ZoomState;
use tg_platform::ocr::{OcrResult, recognize_rgba8};
use tg_platform::screenshot_protection::{ReasonId, ScreenshotProtectionManager};

/// Standard arrow-key seek step in fullscreen video mode (`kSeekTimeMs = 5s`).
pub const SEEK_STEP_SHORT_MS: i64 = 5_000;
/// Long `J`/`L` keyboard seek step (`kSeekTimeMsLong = 10s`).
pub const SEEK_STEP_LONG_MS: i64 = 10_000;

/// Ordered gallery sibling navigator (`_sharedMediaData` / `moveToNext`).
#[derive(Debug, Clone, Default)]
pub struct GalleryNavigator {
    items: Vec<MediaItem>,
    current_index: usize,
}

impl GalleryNavigator {
    /// Creates a navigator from sibling `items`, focusing `target_id` if present.
    #[must_use]
    pub fn new(items: Vec<MediaItem>, target_id: MediaItemId) -> Self {
        let current_index = items
            .iter()
            .position(|item| item.id == target_id)
            .unwrap_or(0);
        Self {
            items,
            current_index,
        }
    }

    /// Returns `true` when there are no items in the gallery.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns total number of gallery items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns all sibling items in gallery order.
    #[must_use]
    pub fn items(&self) -> &[MediaItem] {
        &self.items
    }

    /// Jumps directly to `index` if in bounds. Returns `true` if index changed.
    pub fn jump_to(&mut self, index: usize) -> bool {
        if index < self.items.len() && index != self.current_index {
            self.current_index = index;
            true
        } else {
            false
        }
    }

    /// Returns the 0-based active index.
    #[must_use]
    pub const fn current_index(&self) -> usize {
        self.current_index
    }

    /// Returns the currently displayed [`MediaItem`], if any.
    #[must_use]
    pub fn current(&self) -> Option<&MediaItem> {
        self.items.get(self.current_index)
    }

    /// Returns `true` when navigating backward (`delta = -1`) is possible.
    #[must_use]
    pub fn can_prev(&self) -> bool {
        self.current_index > 0
    }

    /// Returns `true` when navigating forward (`delta = +1`) is possible.
    #[must_use]
    pub fn can_next(&self) -> bool {
        self.current_index + 1 < self.items.len()
    }

    /// Moves by `delta` (`-1` for previous, `+1` for next). Returns `true` if index changed.
    pub fn move_by(&mut self, delta: i32) -> bool {
        if self.items.is_empty() || delta == 0 {
            return false;
        }
        let target = self.current_index as isize + delta as isize;
        if target < 0 || (target as usize) >= self.items.len() {
            return false;
        }
        self.current_index = target as usize;
        true
    }

    /// Removes the current item (e.g. after Delete) and keeps index valid.
    pub fn remove_current(&mut self) -> Option<MediaItem> {
        if self.items.is_empty() {
            return None;
        }
        let removed = self.items.remove(self.current_index);
        if self.current_index >= self.items.len() && !self.items.is_empty() {
            self.current_index = self.items.len() - 1;
        }
        Some(removed)
    }

    /// Returns localized gallery counter text (`lng_mediaview_n_of_amount` /
    /// `lng_mediaview_file_n_of_amount`) when multiple siblings exist.
    #[must_use]
    pub fn counter_text(&self) -> Option<String> {
        if self.items.len() <= 1 {
            return None;
        }
        let n = (self.current_index + 1).to_string();
        let amount = self.items.len().to_string();
        match self.current().map(|i| i.kind) {
            Some(MediaItemKind::Photo) | None => {
                Some(tg_lang::tr::lng_mediaview_n_of_amount(&n, &amount))
            }
            Some(_) => {
                let file_word = tg_lang::tr::lng_mediaview_doc_image();
                Some(tg_lang::tr::lng_mediaview_file_n_of_amount(
                    &file_word, &n, &amount,
                ))
            }
        }
    }
}

/// `Storage::SharedMediaType` the viewer navigates for an item (`OverlayWidget::sharedMediaType`):
/// photos and videos share one list, GIFs, round videos, live streams and files each have their
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SharedMediaKind {
    PhotoVideo,
    Gif,
    RoundVoice,
    Stream,
    File,
}

const fn shared_media_kind(kind: MediaItemKind) -> SharedMediaKind {
    match kind {
        MediaItemKind::Photo | MediaItemKind::Video => SharedMediaKind::PhotoVideo,
        MediaItemKind::Gif => SharedMediaKind::Gif,
        MediaItemKind::RoundVideo => SharedMediaKind::RoundVoice,
        MediaItemKind::VideoStream => SharedMediaKind::Stream,
        MediaItemKind::Theme | MediaItemKind::Document => SharedMediaKind::File,
    }
}

/// Keeps only the siblings in the same shared-media list as `target_id` (a `.txt` file is never
/// part of a photo's "Photo N of M"). Unknown targets keep the list as given.
#[must_use]
pub fn filter_gallery_siblings(items: Vec<MediaItem>, target_id: MediaItemId) -> Vec<MediaItem> {
    let Some(kind) = items
        .iter()
        .find(|item| item.id == target_id)
        .map(|item| shared_media_kind(item.kind))
    else {
        return items;
    };
    items
        .into_iter()
        .filter(|item| shared_media_kind(item.kind) == kind)
        .collect()
}

/// Opacity of the viewer's over-controls (header, arrows, caption, thumbnails, bottom bar):
/// tdesktop's `_controlsOpacity`, shown on activity and hidden `mediaviewWaitHide` ms later over
/// `mediaviewHideDuration` (`OverlayWidget::activateControls` / `hideControls`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlsFade {
    last_activity_ms: i64,
    opacity: f32,
    last_step_ms: i64,
}

impl Default for ControlsFade {
    fn default() -> Self {
        Self {
            last_activity_ms: 0,
            opacity: 1.0,
            last_step_ms: 0,
        }
    }
}

impl ControlsFade {
    /// Shows the controls and restarts the hide timer (`activateControls`).
    pub const fn activate(&mut self, now_ms: i64) {
        self.last_activity_ms = now_ms;
    }

    /// Advances the fade to `now_ms`; `hold` keeps the controls shown (menu open, mouse pressed).
    pub fn step(&mut self, now_ms: i64, hold: bool) -> f32 {
        let dt = (now_ms - self.last_step_ms).max(0) as f32;
        self.last_step_ms = now_ms;
        if hold {
            self.last_activity_ms = now_ms;
        }
        let idle_ms = (now_ms - self.last_activity_ms).max(0) as u64;
        if idle_ms <= MEDIAVIEW_WAIT_HIDE_MS {
            let step = dt / MEDIAVIEW_SHOW_DURATION_MS.max(1) as f32;
            self.opacity = (self.opacity + step).min(1.0);
        } else {
            let fade_elapsed = (idle_ms - MEDIAVIEW_WAIT_HIDE_MS) as f32;
            let target = 1.0 - (fade_elapsed / MEDIAVIEW_HIDE_DURATION_MS.max(1) as f32);
            self.opacity = self.opacity.min(target.clamp(0.0, 1.0));
        }
        self.opacity
    }

    /// Current opacity in `[0, 1]`.
    #[must_use]
    pub const fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Returns `true` while the fade still changes and needs repaints.
    #[must_use]
    pub fn is_animating(&self, now_ms: i64) -> bool {
        let idle_ms = (now_ms - self.last_activity_ms).max(0) as u64;
        (self.opacity > 0.0 && idle_ms <= MEDIAVIEW_WAIT_HIDE_MS + MEDIAVIEW_HIDE_DURATION_MS)
            || (idle_ms <= MEDIAVIEW_WAIT_HIDE_MS && self.opacity < 1.0)
    }
}

/// Action events emitted by [`MediaOverlayWidget::show`] to the host window.
#[derive(Debug)]
pub enum OverlayEvent {
    /// User closed the viewer (`Esc`, close button, or backdrop click).
    Closed,
    /// User clicked "Show in Chat".
    ShowInChat {
        /// Chat peer ID.
        peer_id: i64,
        /// Message ID.
        msg_id: i32,
    },
    /// User clicked "Forward".
    Forward(MediaItemId),
    /// User deleted `item_id`.
    Deleted(MediaItemId),
    /// Media was saved to `path`.
    Saved(PathBuf),
    /// Media or current video frame was copied.
    Copied(MediaItemId),
    /// User requested opening media in external application.
    OpenExternal(PathBuf),
    /// Opening an executable, unknown-extension, extensionless, or IP-revealing item requires
    /// explicit user confirmation (`Data::LaunchWithWarning`).
    ConfirmOpenExternal {
        /// Target media item ID.
        item_id: MediaItemId,
        /// Sanitized display filename.
        file_name: String,
    },
    /// User switched the active video into Picture-in-Picture mode.
    EnterPip(Box<PipWindow>),
    /// Event emitted by the live RTMP/HLS video stream viewer (`media_view_video_stream`).
    VideoStream(VideoStreamAction),
    /// Event emitted by the video-viewer sponsored message overlay (`media_view_playback_sponsored`).
    Sponsored(PlaybackSponsoredEvent),
    /// User clicked "Apply Theme" inside the `.tdesktop-theme`/`.tdesktop-palette` preview card.
    ApplyThemePreview {
        /// Media item ID.
        item_id: MediaItemId,
        /// Theme display name.
        theme_name: String,
        /// Original raw theme file bytes.
        raw_bytes: Vec<u8>,
    },
    /// User clicked "Attached Stickers" (`tr::lng_context_attached_stickers`) for a photo/video
    /// that carries attached stickers or masks (`OverlayWidget::showAttachedStickers`).
    AttachedStickers {
        /// Real photo or document reference to pass to `messages.getAttachedStickers`.
        media: AttachedStickersMedia,
        /// Display label describing the inspected media.
        media_label: String,
    },
}

/// Active decoded content state held by [`MediaOverlayWidget`].
enum ActiveContent {
    None,
    Photo {
        base_image: DynamicImage,
        texture: Option<TextureHandle>,
        rendered_rotation: i32,
        rendered_flip_h: bool,
        rendered_flip_v: bool,
        has_alpha: bool,
    },
    Video {
        player: Box<StreamingPlayer<FfmpegStreamDecoder>>,
        texture: Option<TextureHandle>,
        rendered_rotation: i32,
        rendered_flip_h: bool,
        rendered_flip_v: bool,
    },
    VideoStream(Box<VideoStreamViewer>),
    ThemePreview(Box<ThemeFilePreview>),
    Error(String),
}

struct ToastBanner {
    text: String,
    shown_at_ms: i64,
}

/// Fullscreen media viewer overlay (`Media::View::OverlayWidget`).
pub struct MediaOverlayWidget {
    open: bool,
    gallery: GalleryNavigator,
    zoom: ZoomState,
    controls: PlaybackControlsState,
    content: ActiveContent,
    more_menu_open: bool,
    caption_expanded: bool,
    revealed_spoilers: HashSet<usize>,
    toast: Option<ToastBanner>,
    last_viewport_size: Vec2,
    last_world_ms: i64,
    save_directory: PathBuf,
    volume: f64,
    speed: f64,
    ocr: OcrOverlayState,
    group_thumbs: GroupThumbsStrip,
    screenshot_protection: ScreenshotProtectionManager,
    protection_reason: Option<ReasonId>,
    playback_sponsored: PlaybackSponsoredOverlay,
    controls_fade: ControlsFade,
}

impl Default for MediaOverlayWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Default folder for "Save as" from the media viewer: the user's Downloads folder
/// (`<USERPROFILE>\Downloads\Rustgram` on Windows, `<HOME>/Downloads/Rustgram` elsewhere), like
/// tdesktop's `Core::App().downloadPath()` default. Falls back to a temp folder only when neither
/// home variable is set.
#[must_use]
pub fn default_save_directory() -> PathBuf {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map_or_else(
            || std::env::temp_dir().join("RustgramDownloads"),
            |home| PathBuf::from(home).join("Downloads").join("Rustgram"),
        )
}

impl MediaOverlayWidget {
    /// Creates a closed media overlay widget with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            open: false,
            gallery: GalleryNavigator::default(),
            zoom: ZoomState::default(),
            controls: PlaybackControlsState::default(),
            content: ActiveContent::None,
            more_menu_open: false,
            caption_expanded: false,
            revealed_spoilers: HashSet::new(),
            toast: None,
            last_viewport_size: Vec2::new(1280.0, 800.0),
            last_world_ms: 0,
            save_directory: default_save_directory(),
            volume: 1.0,
            speed: 1.0,
            ocr: OcrOverlayState::default(),
            group_thumbs: GroupThumbsStrip::new(),
            screenshot_protection: ScreenshotProtectionManager::new(),
            protection_reason: None,
            playback_sponsored: PlaybackSponsoredOverlay::new(),
            controls_fade: ControlsFade::default(),
        }
    }

    /// Opacity of the header, arrows, caption, thumbnails and bottom bar (`_controlsOpacity`).
    #[must_use]
    pub const fn controls_opacity(&self) -> f32 {
        self.controls_fade.opacity()
    }

    /// Returns `true` when the currently displayed media item forbids saving/forwarding
    /// (`OverlayWidget::contentNeedsScreenshotProtection` in `media_view_overlay_widget.cpp:6199`).
    #[must_use]
    pub fn content_needs_screenshot_protection(&self) -> bool {
        self.open
            && self
                .gallery
                .current()
                .is_some_and(|item| !item.allows_save || !item.allows_forward)
    }

    /// Synchronizes Windows `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` for `hwnd`
    /// with [`Self::content_needs_screenshot_protection`].
    pub fn sync_screenshot_protection(&mut self, hwnd: isize) {
        let needed = self.content_needs_screenshot_protection();
        match self.protection_reason {
            Some(id) => {
                let _ = self.screenshot_protection.set_reason_window(id, hwnd);
                let _ = self.screenshot_protection.set_reason_active(id, needed);
            }
            None if hwnd != 0 && needed => {
                if let Ok(id) = self.screenshot_protection.add_content_reason(hwnd, true) {
                    self.protection_reason = Some(id);
                }
            }
            None => {}
        }
    }

    /// Returns `true` when screenshot protection is currently active on `hwnd`.
    #[must_use]
    pub fn is_screenshot_protected(&self, hwnd: isize) -> bool {
        self.screenshot_protection.is_protected(hwnd)
    }

    /// Returns a shared reference to the current OCR text recognition & selection state.
    #[must_use]
    pub const fn ocr_state(&self) -> &OcrOverlayState {
        &self.ocr
    }

    /// Returns a mutable reference to the current OCR text recognition & selection state.
    pub fn ocr_state_mut(&mut self) -> &mut OcrOverlayState {
        &mut self.ocr
    }

    /// Runs `Windows.Media.Ocr` on the currently displayed photo (if allowed by `allows_save`).
    pub fn recognize_current_photo(&mut self, world_now_ms: i64) -> Option<&OcrResult> {
        let allows_save = self.gallery.current().is_some_and(|i| i.allows_save);
        if !allows_save {
            return None;
        }
        if self.ocr.result.is_some() {
            self.ocr.active = true;
            return self.ocr.result.as_ref();
        }
        let ActiveContent::Photo { base_image, .. } = &self.content else {
            return None;
        };
        let rgba = base_image.to_rgba8();
        match recognize_rgba8(rgba.as_raw(), rgba.width(), rgba.height(), None) {
            Ok(res) if res.has_text() => {
                self.ocr.result = Some(res);
                self.ocr.active = true;
                self.ocr.selection.clear();
                self.ocr.result.as_ref()
            }
            Ok(_) => {
                self.ocr.active = false;
                self.show_toast(strings::OCR_NO_TEXT_FOUND, world_now_ms);
                None
            }
            Err(_) => {
                self.ocr.active = false;
                self.show_toast(strings::OCR_UNAVAILABLE, world_now_ms);
                None
            }
        }
    }

    /// Injects a precomputed [`OcrResult`] and activates OCR text selection (for tests).
    pub fn set_ocr_result_for_test(&mut self, result: OcrResult) {
        self.ocr.result = Some(result);
        self.ocr.active = true;
        self.ocr.selection.clear();
    }

    /// Configures the default directory used by quick Save As actions.
    pub fn set_save_directory(&mut self, dir: PathBuf) {
        self.save_directory = dir;
    }

    /// Directory used by quick Save As actions.
    #[must_use]
    pub fn save_directory(&self) -> &std::path::Path {
        &self.save_directory
    }

    /// Returns `true` when the fullscreen overlay is currently visible.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns a shared reference to the current [`ZoomState`].
    #[must_use]
    pub const fn zoom_state(&self) -> &ZoomState {
        &self.zoom
    }

    /// Returns a mutable reference to the current [`ZoomState`].
    pub fn zoom_state_mut(&mut self) -> &mut ZoomState {
        &mut self.zoom
    }

    /// Returns a shared reference to the gallery navigator.
    #[must_use]
    pub const fn gallery(&self) -> &GalleryNavigator {
        &self.gallery
    }

    /// Returns a shared reference to the video playback controls state.
    #[must_use]
    pub const fn controls(&self) -> &PlaybackControlsState {
        &self.controls
    }

    /// Returns a mutable reference to the video playback controls state.
    pub fn controls_mut(&mut self) -> &mut PlaybackControlsState {
        &mut self.controls
    }

    /// Closes the overlay and releases active streaming/photo buffers.
    pub fn close(&mut self) {
        self.open = false;
        self.content = ActiveContent::None;
        self.more_menu_open = false;
        self.controls.speed_menu_open = false;
        self.ocr.reset();
        self.playback_sponsored = PlaybackSponsoredOverlay::new();
        if let Some(id) = self.protection_reason {
            let _ = self.screenshot_protection.set_reason_active(id, false);
        }
    }

    /// Opens an RTMP/HLS live video stream directly inside the media overlay
    /// (`media_view_video_stream.cpp`).
    pub fn open_video_stream(&mut self, descriptor: VideoStreamDescriptor, world_now_ms: i64) {
        let item = MediaItem {
            id: MediaItemId(descriptor.call_id as u64),
            peer_id: descriptor.peer_id,
            msg_id: descriptor.join_msg_id,
            kind: MediaItemKind::VideoStream,
            width: 1280,
            height: 720,
            duration_ms: 0,
            file_size: 0,
            file_name: descriptor.title.clone(),
            mime_type: "application/x-tg-video-stream".to_owned(),
            sender_name: descriptor.title.clone(),
            date_text: if descriptor.rtmp_stream {
                "LIVE · RTMP".to_owned()
            } else {
                "LIVE".to_owned()
            },
            caption: TextWithEntities::default(),
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: false,
            allows_delete: false,
            allows_save: false,
            local_path: None,
            grouped_id: None,
            attached_stickers_media: None,
        };
        self.gallery = GalleryNavigator::new(vec![item], MediaItemId(descriptor.call_id as u64));
        self.open = true;
        self.more_menu_open = false;
        self.ocr.reset();
        self.playback_sponsored = PlaybackSponsoredOverlay::new();
        self.last_world_ms = world_now_ms;
        self.content =
            ActiveContent::VideoStream(Box::new(VideoStreamViewer::new(descriptor, self.volume)));
    }

    /// Opens a `.tdesktop-theme` / `.tdesktop-palette` file preview directly from raw bytes
    /// (`OverlayWidget::showThemePreview` in `media_view_overlay_widget.cpp`).
    pub fn open_theme_preview_bytes(
        &mut self,
        item_id: MediaItemId,
        file_name: &str,
        bytes: Vec<u8>,
        users_count: u32,
        world_now_ms: i64,
    ) -> Result<(), String> {
        let mut preview = ThemeFilePreview::from_bytes(item_id, file_name, bytes)?;
        preview.users_count = users_count;
        let item = MediaItem {
            id: item_id,
            peer_id: 0,
            msg_id: 0,
            kind: MediaItemKind::Theme,
            width: 640,
            height: 460,
            duration_ms: 0,
            file_size: preview.raw_bytes.len() as u64,
            file_name: file_name.to_owned(),
            mime_type: "application/x-tgtheme-tdesktop".to_owned(),
            sender_name: preview.theme_name.clone(),
            date_text: "Theme Preview".to_owned(),
            caption: TextWithEntities::default(),
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: true,
            allows_delete: false,
            allows_save: true,
            local_path: None,
            grouped_id: None,
            attached_stickers_media: None,
        };
        self.gallery = GalleryNavigator::new(vec![item], item_id);
        self.open = true;
        self.more_menu_open = false;
        self.ocr.reset();
        self.playback_sponsored = PlaybackSponsoredOverlay::new();
        self.last_world_ms = world_now_ms;
        self.content = ActiveContent::ThemePreview(Box::new(preview));
        Ok(())
    }

    /// Attaches video-viewer sponsored message data (`media_view_playback_sponsored`) to the
    /// active video playback session and starts its timer immediately when playing.
    pub fn attach_video_sponsored(
        &mut self,
        data: SponsoredForVideo,
        world_now_ms: i64,
    ) -> Vec<PlaybackSponsoredEvent> {
        let mut events = self.playback_sponsored.set_data(data, world_now_ms);
        if matches!(self.content, ActiveContent::Video { .. }) {
            let is_paused = matches!(
                &self.content,
                ActiveContent::Video { player, .. } if player.is_paused()
            );
            events.extend(self.playback_sponsored.set_paused(is_paused, world_now_ms));
            events.extend(self.playback_sponsored.start(world_now_ms));
        }
        events
    }

    /// Returns a reference to the active [`VideoStreamViewer`], if showing a live stream.
    #[must_use]
    pub fn video_stream(&self) -> Option<&VideoStreamViewer> {
        match &self.content {
            ActiveContent::VideoStream(v) => Some(v),
            _ => None,
        }
    }

    /// Returns a mutable reference to the active [`VideoStreamViewer`], if showing a live stream.
    pub fn video_stream_mut(&mut self) -> Option<&mut VideoStreamViewer> {
        match &mut self.content {
            ActiveContent::VideoStream(v) => Some(v),
            _ => None,
        }
    }

    /// Returns a reference to the active [`ThemeFilePreview`], if previewing a theme file.
    #[must_use]
    pub fn theme_preview(&self) -> Option<&ThemeFilePreview> {
        match &self.content {
            ActiveContent::ThemePreview(t) => Some(t),
            _ => None,
        }
    }

    /// Returns a reference to the video-viewer [`PlaybackSponsoredOverlay`].
    #[must_use]
    pub const fn playback_sponsored(&self) -> &PlaybackSponsoredOverlay {
        &self.playback_sponsored
    }

    /// Returns a mutable reference to the video-viewer [`PlaybackSponsoredOverlay`].
    pub fn playback_sponsored_mut(&mut self) -> &mut PlaybackSponsoredOverlay {
        &mut self.playback_sponsored
    }

    /// Opens `item_id` in `peer_id`'s gallery through `backend`.
    pub fn open_item(
        &mut self,
        backend: &dyn MediaBackend,
        peer_id: i64,
        item_id: MediaItemId,
        world_now_ms: i64,
    ) -> anyhow::Result<()> {
        let mut siblings =
            filter_gallery_siblings(backend.siblings_in_chat(peer_id, item_id), item_id);
        if siblings.is_empty()
            && let Some(item) = backend.get_item(item_id)
        {
            siblings.push(item);
        }
        self.gallery = GalleryNavigator::new(siblings, item_id);
        self.open = !self.gallery.is_empty();
        self.last_world_ms = world_now_ms;
        self.controls_fade = ControlsFade::default();
        self.controls_fade.activate(world_now_ms);
        self.load_current_item(backend, world_now_ms, 0, false)
    }

    /// Re-opens a video returning from PiP with the saved [`PipResumeState`].
    pub fn reopen_from_pip(
        &mut self,
        backend: &dyn MediaBackend,
        item: &MediaItem,
        resume: PipResumeState,
        world_now_ms: i64,
    ) -> anyhow::Result<()> {
        self.volume = resume.volume.clamp(0.0, 1.0);
        self.speed = resume.speed.clamp(tg_video::SPEED_MIN, tg_video::SPEED_MAX);
        let mut siblings =
            filter_gallery_siblings(backend.siblings_in_chat(item.peer_id, item.id), item.id);
        if siblings.is_empty() {
            siblings.push(item.clone());
        }
        self.gallery = GalleryNavigator::new(siblings, item.id);
        self.open = !self.gallery.is_empty();
        self.last_world_ms = world_now_ms;
        self.controls_fade = ControlsFade::default();
        self.controls_fade.activate(world_now_ms);
        self.load_current_item(backend, world_now_ms, resume.position_ms, resume.paused)
    }

    /// Navigates gallery by `delta` (`-1` previous, `+1` next) and reloads the active media.
    pub fn navigate(&mut self, backend: &dyn MediaBackend, delta: i32, world_now_ms: i64) -> bool {
        if !self.gallery.move_by(delta) {
            return false;
        }
        let _ = self.load_current_item(backend, world_now_ms, 0, false);
        true
    }

    /// Steps video decoding at `world_now_ms` without rendering UI (useful in tests & screenshots).
    pub fn step_video(&mut self, world_now_ms: i64) {
        self.last_world_ms = world_now_ms;
        if let ActiveContent::Video { player, .. } = &mut self.content {
            let _ = player.step_video(world_now_ms);
            let dur = player
                .total_duration_ms()
                .max(self.gallery.current().map_or(0, |i| i.duration_ms));
            let pos = player.position_ms(world_now_ms);
            let playing = !player.is_paused() && player.stage() == PlayerStage::Started;
            self.controls.update_playback(playing, pos, dur, 1.0);
        }
    }

    /// Seeks the active video to `position_ms`.
    pub fn seek_video(&mut self, position_ms: i64, world_now_ms: i64) {
        if let ActiveContent::Video { player, .. } = &mut self.content {
            let _ = player.seek(position_ms.max(0), Some(world_now_ms));
            let _ = player.step_video(world_now_ms);
            let dur = player
                .total_duration_ms()
                .max(self.gallery.current().map_or(0, |i| i.duration_ms));
            let playing = !player.is_paused() && player.stage() == PlayerStage::Started;
            self.controls
                .update_playback(playing, position_ms.max(0), dur, 1.0);
        }
    }

    /// Sets video chapter timestamps on the playback controls bar.
    pub fn set_video_timestamps(&mut self, timestamps: Vec<TimestampChapter>) {
        self.controls.timestamps = timestamps;
    }

    fn load_current_item(
        &mut self,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        start_pos_ms: i64,
        start_paused: bool,
    ) -> anyhow::Result<()> {
        self.more_menu_open = false;
        self.controls.speed_menu_open = false;
        self.caption_expanded = false;
        self.revealed_spoilers.clear();
        self.controls.timestamps.clear();
        self.ocr.reset();
        self.playback_sponsored = PlaybackSponsoredOverlay::new();

        let Some(item) = self.gallery.current().cloned() else {
            self.content = ActiveContent::None;
            self.open = false;
            return Ok(());
        };

        let bottom_skip = if item.kind.shows_playback_controls() {
            136.0
        } else {
            88.0
        };
        self.zoom.set_content(
            f64::from(item.width.max(1)),
            f64::from(item.height.max(1)),
            item.rotation_degrees,
        );
        self.zoom.resize_viewport(
            f64::from(self.last_viewport_size.x),
            f64::from(self.last_viewport_size.y),
            56.0,
            bottom_skip,
        );

        match item.kind {
            MediaItemKind::Photo => match backend.load_bytes(item.id) {
                Ok(bytes) => match tg_media::image::read_image(tg_media::image::ReadArgs {
                    content: bytes,
                    ..tg_media::image::ReadArgs::default()
                }) {
                    Ok(read) => {
                        let img = DynamicImage::ImageRgba8(read.image);
                        let w = f64::from(img.width().max(1));
                        let h = f64::from(img.height().max(1));
                        self.zoom.set_content(w, h, item.rotation_degrees);
                        self.zoom.resize_viewport(
                            f64::from(self.last_viewport_size.x),
                            f64::from(self.last_viewport_size.y),
                            56.0,
                            bottom_skip,
                        );
                        self.content = ActiveContent::Photo {
                            base_image: img,
                            texture: None,
                            rendered_rotation: -1,
                            rendered_flip_h: false,
                            rendered_flip_v: false,
                            has_alpha: item.has_alpha,
                        };
                    }
                    Err(err) => {
                        self.content =
                            ActiveContent::Error(format!("Failed to decode photo: {err}"));
                    }
                },
                Err(err) => {
                    self.content = ActiveContent::Error(format!("Failed to load photo: {err}"));
                }
            },
            MediaItemKind::Video | MediaItemKind::Gif | MediaItemKind::RoundVideo => {
                match backend.open_range_loader(item.id) {
                    Ok(loader) => {
                        let opts = PlaybackOptions {
                            mode: if item.kind == MediaItemKind::Gif {
                                PlaybackMode::Video
                            } else {
                                PlaybackMode::Both
                            },
                            position_ms: start_pos_ms.max(0),
                            speed: self.speed,
                            volume: self.volume,
                            loop_playback: item.kind.loops_by_default(),
                            output_format: OutputPixelFormat::Rgba,
                            apply_rotation: true,
                            ..PlaybackOptions::default()
                        };
                        match FfmpegStreamDecoder::from_range_loader(
                            loader,
                            None,
                            SliceConfig::default(),
                            opts.clone(),
                        )
                        .and_then(|dec| StreamingPlayer::new(dec, opts))
                        {
                            Ok(mut player) => {
                                if let Some(vinfo) = &player.info().video {
                                    self.zoom.set_content(
                                        f64::from(vinfo.size.width.max(1)),
                                        f64::from(vinfo.size.height.max(1)),
                                        item.rotation_degrees,
                                    );
                                    self.zoom.resize_viewport(
                                        f64::from(self.last_viewport_size.x),
                                        f64::from(self.last_viewport_size.y),
                                        56.0,
                                        bottom_skip,
                                    );
                                }
                                if start_paused {
                                    player.pause(world_now_ms);
                                } else {
                                    player.start(world_now_ms);
                                }
                                let _ = player.step_video(world_now_ms);

                                let dur = player.total_duration_ms().max(item.duration_ms);
                                self.controls.set_volume(self.volume);
                                self.controls.set_speed(self.speed);
                                self.controls.in_fullscreen = false;
                                self.controls.opacity = 1.0;
                                self.controls.note_activity(world_now_ms);
                                self.controls.update_playback(
                                    !start_paused,
                                    start_pos_ms.max(0),
                                    dur,
                                    1.0,
                                );
                                self.content = ActiveContent::Video {
                                    player: Box::new(player),
                                    texture: None,
                                    rendered_rotation: self.zoom.rotation,
                                    rendered_flip_h: self.zoom.flip_h,
                                    rendered_flip_v: self.zoom.flip_v,
                                };
                            }
                            Err(err) => {
                                self.content = ActiveContent::Error(format!(
                                    "Video playback unavailable: {err}"
                                ));
                            }
                        }
                    }
                    Err(err) => {
                        self.content = ActiveContent::Error(format!("Failed to open video: {err}"));
                    }
                }
            }
            MediaItemKind::VideoStream => {
                let descriptor = VideoStreamDescriptor {
                    call_id: item.id.0 as i64,
                    access_hash: 0,
                    peer_id: item.peer_id,
                    title: if item.file_name.is_empty() {
                        item.sender_name.clone()
                    } else {
                        item.file_name.clone()
                    },
                    call_link_slug: String::new(),
                    join_msg_id: item.msg_id,
                    rtmp_stream: item.file_name.starts_with("rtmp_"),
                    stream_url: None,
                    stream_dc_id: 0,
                    is_live_story: false,
                };
                self.content = ActiveContent::VideoStream(Box::new(VideoStreamViewer::new(
                    descriptor,
                    self.volume,
                )));
            }
            MediaItemKind::Theme => match backend.load_bytes(item.id) {
                Ok(bytes) => match ThemeFilePreview::from_bytes(item.id, &item.file_name, bytes) {
                    Ok(preview) => {
                        self.content = ActiveContent::ThemePreview(Box::new(preview));
                    }
                    Err(err) => {
                        self.content = ActiveContent::Error(err);
                    }
                },
                Err(err) => {
                    self.content =
                        ActiveContent::Error(format!("Failed to load theme file: {err}"));
                }
            },
            MediaItemKind::Document => {
                if is_theme_file(&item.file_name, &item.mime_type) {
                    match backend.load_bytes(item.id) {
                        Ok(bytes) => {
                            match ThemeFilePreview::from_bytes(item.id, &item.file_name, bytes) {
                                Ok(preview) => {
                                    self.content = ActiveContent::ThemePreview(Box::new(preview));
                                }
                                Err(err) => {
                                    self.content = ActiveContent::Error(err);
                                }
                            }
                        }
                        Err(err) => {
                            self.content =
                                ActiveContent::Error(format!("Failed to load theme file: {err}"));
                        }
                    }
                } else {
                    self.content = ActiveContent::Error(format!(
                        "Document preview: {} ({} bytes)",
                        item.file_name, item.file_size
                    ));
                }
            }
        }
        Ok(())
    }

    fn show_toast(&mut self, text: impl Into<String>, now_ms: i64) {
        self.toast = Some(ToastBanner {
            text: text.into(),
            shown_at_ms: now_ms,
        });
    }

    /// Renders the fullscreen media viewer overlay over `viewport_rect` and returns emitted events.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        viewport_rect: Rect,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
    ) -> Vec<OverlayEvent> {
        let mut events = Vec::new();
        if !self.open {
            return events;
        }
        self.last_world_ms = world_now_ms;

        // Track viewport resize.
        let vp_size = viewport_rect.size();
        if (vp_size - self.last_viewport_size).length_sq() > 1.0 {
            self.last_viewport_size = vp_size;
            let bottom_skip = if self
                .gallery
                .current()
                .is_some_and(|i| i.kind.shows_playback_controls())
            {
                136.0
            } else {
                88.0
            };
            self.zoom.resize_viewport(
                f64::from(vp_size.x),
                f64::from(vp_size.y),
                56.0,
                bottom_skip,
            );
        }

        // Advance video streaming & upload frame texture.
        self.update_active_texture(ctx, world_now_ms);

        // Handle keyboard shortcuts (`OverlayWidget::handleKeyPress`).
        self.handle_keyboard(ctx, backend, world_now_ms, &mut events);
        if !self.open {
            return events;
        }

        let Some(item) = self.gallery.current().cloned() else {
            self.close();
            events.push(OverlayEvent::Closed);
            return events;
        };

        ensure_tdesktop_fonts(ctx);
        let colors = MediaColors::from_ctx(ctx);
        let mv_bg = colors.color_or("mediaviewBg", MEDIAVIEW_BG);
        let mv_video_bg = colors.color_or("mediaviewVideoBg", MEDIAVIEW_VIDEO_BG);
        let mv_control_bg = colors.color_or("mediaviewControlBg", MEDIAVIEW_CONTROL_BG);
        let mv_control_fg = colors.color_or("mediaviewControlFg", MEDIAVIEW_CONTROL_FG);
        let mv_menu_bg = colors.color_or("mediaviewMenuBg", MEDIAVIEW_MENU_BG);
        let mv_save_bg = colors.color_or("mediaviewSaveMsgBg", MEDIAVIEW_SAVE_MSG_BG);
        let mv_save_fg = colors.color_or("mediaviewSaveMsgFg", MEDIAVIEW_SAVE_MSG_FG);

        Area::new(Id::unique("tg_media_view_overlay"))
            .order(Order::Foreground)
            .fade_in(false)
            .fixed_pos(viewport_rect.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(viewport_rect);
                let (_, bg_resp) =
                    ui.allocate_exact_size(viewport_rect.size(), Sense::click_and_drag());
                let painter = ui.painter().clone();

                // Backdrop (`mediaviewVideoBg` in fullscreen video, otherwise `mediaviewBg`).
                let bg_color = if self.zoom.full_screen_video {
                    mv_video_bg
                } else {
                    mv_bg
                };
                painter.rect_filled(viewport_rect, CornerRadius::ZERO, bg_color);

                // Mouse wheel: Ctrl+Wheel zooms around cursor; plain wheel navigates gallery.
                let (scroll_delta, ctrl_down, pointer_pos) = ctx.input(|i| {
                    (
                        i.smooth_scroll_delta,
                        i.modifiers.ctrl || i.modifiers.command,
                        i.pointer.hover_pos(),
                    )
                });
                if scroll_delta.y.abs() > 1.0 && ui.rect_contains_pointer(viewport_rect) {
                    if ctrl_down {
                        let anchor = pointer_pos.map(|p| {
                            (
                                f64::from(p.x - viewport_rect.min.x),
                                f64::from(p.y - viewport_rect.min.y),
                            )
                        });
                        if scroll_delta.y > 0.0 {
                            self.zoom.zoom_in(anchor);
                        } else {
                            self.zoom.zoom_out(anchor);
                        }
                    } else if !self.zoom.can_drag() {
                        if scroll_delta.y < -18.0 {
                            self.navigate(backend, 1, world_now_ms);
                        } else if scroll_delta.y > 18.0 {
                            self.navigate(backend, -1, world_now_ms);
                        }
                    }
                }

                // Content rect in global viewport coordinates.
                let content_rect = self
                    .zoom
                    .content_rect()
                    .translate(viewport_rect.min.to_vec2());

                // Paint photo/video frame, live video stream, theme preview card, or error card.
                let mut stream_actions = Vec::new();
                let mut theme_preview_action = None;
                let mut theme_preview_rect = None;
                let is_stream_mode = matches!(self.content, ActiveContent::VideoStream(_));
                let is_theme_preview = matches!(self.content, ActiveContent::ThemePreview(_));

                match &mut self.content {
                    ActiveContent::Photo {
                        texture: Some(tex),
                        has_alpha,
                        ..
                    } => {
                        if *has_alpha {
                            paint_transparent_checkerboard(&painter, content_rect, 12.0, &colors);
                        }
                        painter.image(
                            tex.id(),
                            content_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ActiveContent::Video {
                        texture: Some(tex), ..
                    } => {
                        painter.image(
                            tex.id(),
                            content_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ActiveContent::VideoStream(stream) => {
                        stream_actions = stream.show(ui, viewport_rect, world_now_ms);
                    }
                    ActiveContent::ThemePreview(preview) => {
                        theme_preview_action = preview.show(ui, viewport_rect);
                        theme_preview_rect = Some(preview.last_card_rect());
                    }
                    ActiveContent::Error(msg) => {
                        let card =
                            Rect::from_center_size(viewport_rect.center(), Vec2::new(380.0, 110.0));
                        painter.rect_filled(card, CornerRadius::same(10), mv_menu_bg);
                        painter.text(
                            card.center(),
                            Align2::CENTER_CENTER,
                            msg.as_str(),
                            font_id_from_style(st::mediaview_font),
                            mv_control_fg,
                        );
                    }
                    _ => {
                        let card =
                            Rect::from_center_size(viewport_rect.center(), Vec2::new(220.0, 64.0));
                        painter.rect_filled(card, CornerRadius::same(8), mv_control_bg);
                        painter.text(
                            card.center(),
                            Align2::CENTER_CENTER,
                            tg_lang::tr::lng_contacts_loading(),
                            font_id_from_style(st::mediaview_font),
                            mv_control_fg,
                        );
                    }
                }

                for act in stream_actions {
                    if matches!(act, VideoStreamAction::Close) {
                        self.close();
                        events.push(OverlayEvent::VideoStream(act));
                        events.push(OverlayEvent::Closed);
                        return;
                    }
                    if let VideoStreamAction::SetVolume(vol) = act {
                        self.volume = vol;
                    }
                    events.push(OverlayEvent::VideoStream(act));
                }

                if let Some(action) = theme_preview_action {
                    match action {
                        ThemePreviewAction::Cancel => {
                            self.close();
                            events.push(OverlayEvent::Closed);
                            return;
                        }
                        ThemePreviewAction::Share(id) => {
                            events.push(OverlayEvent::Forward(id));
                        }
                        ThemePreviewAction::Apply {
                            item_id,
                            theme_name,
                            raw_bytes,
                        } => {
                            self.show_toast(format!("Applied theme: {theme_name}"), world_now_ms);
                            events.push(OverlayEvent::ApplyThemePreview {
                                item_id,
                                theme_name,
                                raw_bytes,
                            });
                        }
                    }
                }

                // Pan dragging when zoomed in, double-click zoom reset / fullscreen video,
                // or OCR text selection dragging when OCR mode is active over a photo.
                let mut ocr_consumed_pointer = false;
                if self.ocr.active
                    && matches!(self.content, ActiveContent::Photo { .. })
                    && self.ocr.result.is_some()
                {
                    ocr_consumed_pointer = self.paint_and_handle_ocr_overlay(
                        ui,
                        viewport_rect,
                        content_rect,
                        &colors,
                        world_now_ms,
                    );
                }

                if !ocr_consumed_pointer {
                    if bg_resp.dragged() && self.zoom.can_drag() {
                        let d = bg_resp.drag_delta();
                        self.zoom.drag_by(f64::from(d.x), f64::from(d.y));
                        ctx.set_cursor_icon(CursorIcon::Grabbing);
                    } else if bg_resp.hovered()
                        && self.zoom.can_drag()
                        && pointer_pos.is_some_and(|p| content_rect.contains(p))
                    {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }

                    if bg_resp.double_clicked() {
                        if item.kind.shows_playback_controls() {
                            self.zoom.toggle_fullscreen_video(136.0);
                            self.controls.in_fullscreen = self.zoom.full_screen_video;
                        } else {
                            self.zoom.zoom_reset();
                        }
                    } else if bg_resp.clicked()
                        && let Some(pos) = bg_resp.interact_pointer_pos()
                    {
                        if self.more_menu_open {
                            self.more_menu_open = false;
                        } else if let Some(tp_rect) = theme_preview_rect {
                            if !tp_rect.contains(pos) {
                                self.close();
                                events.push(OverlayEvent::Closed);
                                return;
                            }
                        } else if !is_stream_mode
                            && !content_rect.contains(pos)
                            && !self.zoom.full_screen_video
                        {
                            self.close();
                            events.push(OverlayEvent::Closed);
                            return;
                        } else if item.kind.shows_playback_controls() {
                            self.toggle_video_play_pause(world_now_ms);
                        }
                    }
                }

                // Track pointer activity for controls fade.
                let (pointer_moved, pointer_down) = ctx.input(|i| {
                    (
                        i.pointer.delta().length_sq() > 0.5,
                        i.pointer.any_down() || i.pointer.any_pressed(),
                    )
                });
                if pointer_moved {
                    self.controls.note_activity(world_now_ms);
                    self.controls_fade.activate(world_now_ms);
                }
                self.controls
                    .step_fade(world_now_ms, !self.zoom.full_screen_video);
                let chrome_opacity = self
                    .controls_fade
                    .step(world_now_ms, self.more_menu_open || pointer_down);
                if self.controls_fade.is_animating(world_now_ms) {
                    ctx.request_repaint();
                }
                let mut chrome_painter = painter.clone();
                chrome_painter.multiply_opacity(chrome_opacity);

                // ── Left / Right gallery navigation chevrons (`st::mediaviewLeft` / `Right`) ──
                if !self.zoom.full_screen_video {
                    let ctrl_size = st::mediaview_control_size.0 as f32;
                    if self.gallery.can_prev() {
                        let left_btn = Rect::from_center_size(
                            Pos2::new(viewport_rect.min.x + 42.0, viewport_rect.center().y),
                            Vec2::splat(ctrl_size),
                        );
                        let resp = ui.interact(left_btn, Id::unique("mv_nav_prev"), Sense::click());
                        chrome_painter.circle_filled(
                            left_btn.center(),
                            ctrl_size * 0.42,
                            if resp.hovered() {
                                MEDIAVIEW_CONTROL_BG_OVER
                            } else {
                                mv_control_bg
                            },
                        );
                        paint_centered_style_icon(
                            ctx,
                            &chrome_painter,
                            &st::mediaview_left,
                            left_btn,
                            &colors,
                        );
                        if resp.clicked() {
                            self.navigate(backend, -1, world_now_ms);
                        }
                    }
                    if self.gallery.can_next() {
                        let right_btn = Rect::from_center_size(
                            Pos2::new(viewport_rect.max.x - 42.0, viewport_rect.center().y),
                            Vec2::splat(ctrl_size),
                        );
                        let resp =
                            ui.interact(right_btn, Id::unique("mv_nav_next"), Sense::click());
                        chrome_painter.circle_filled(
                            right_btn.center(),
                            ctrl_size * 0.42,
                            if resp.hovered() {
                                MEDIAVIEW_CONTROL_BG_OVER
                            } else {
                                mv_control_bg
                            },
                        );
                        paint_centered_style_icon(
                            ctx,
                            &chrome_painter,
                            &st::mediaview_right,
                            right_btn,
                            &colors,
                        );
                        if resp.clicked() {
                            self.navigate(backend, 1, world_now_ms);
                        }
                    }
                }

                // ── Top header bar (sender name, date, counter, zoom badge, close button) ──
                if !self.zoom.full_screen_video {
                    ui.scope(|ui| {
                        ui.multiply_opacity(chrome_opacity);
                        self.paint_header(ui, viewport_rect, &item, &colors, &mut events);
                    });
                }

                // ── Video PlaybackControls pill (for videos) + Sponsored video overlay ──
                let mut caption_bottom_offset = 66.0_f32;
                if item.kind.shows_playback_controls() {
                    let controls_margin = if self.zoom.full_screen_video {
                        22.0
                    } else {
                        62.0
                    };
                    let controls_rect = self.controls.layout_rect(viewport_rect, controls_margin);
                    let sponsored_events =
                        self.playback_sponsored
                            .show(ui, controls_rect, world_now_ms);
                    for ev in sponsored_events {
                        events.push(OverlayEvent::Sponsored(ev));
                    }
                    let actions =
                        self.controls
                            .show(ui, controls_rect, Id::unique("mv_playback_controls"));
                    caption_bottom_offset = viewport_rect.max.y - controls_rect.min.y + 10.0;
                    for action in actions {
                        self.handle_playback_action(
                            action,
                            viewport_rect,
                            &item,
                            world_now_ms,
                            &mut events,
                        );
                    }
                }

                // ── Album Group Thumbnails Strip (`media_view_group_thumbs`): only the
                // items of the current message's album (`ComputeContext` = `groupId`). ──
                let album = album_strip_range(self.gallery.items(), self.gallery.current_index());
                if !self.zoom.full_screen_video
                    && let Some(album) = album
                {
                    let strip_h = GroupThumbsStrip::height();
                    let strip_rect = Rect::from_min_size(
                        Pos2::new(
                            viewport_rect.min.x + 120.0,
                            viewport_rect.max.y - caption_bottom_offset - strip_h - 4.0,
                        ),
                        Vec2::new((viewport_rect.width() - 240.0).max(60.0), strip_h),
                    );
                    let picked = ui
                        .scope(|ui| {
                            ui.multiply_opacity(chrome_opacity);
                            self.group_thumbs.show(
                                ui,
                                strip_rect,
                                &self.gallery.items()[album.clone()],
                                self.gallery.current_index() - album.start,
                            )
                        })
                        .inner
                        .map(|i| i + album.start);
                    if let Some(target_idx) = picked {
                        let delta = target_idx as i32 - self.gallery.current_index() as i32;
                        if delta != 0 {
                            self.navigate(backend, delta, world_now_ms);
                        }
                    }
                    caption_bottom_offset += strip_h + 6.0;
                } else {
                    self.group_thumbs.clear();
                }

                // ── Caption with rich entities ──
                if !self.zoom.full_screen_video && !item.caption.text.is_empty() {
                    ui.scope(|ui| {
                        ui.multiply_opacity(chrome_opacity);
                        self.paint_caption(
                            ui,
                            viewport_rect,
                            &item.caption,
                            caption_bottom_offset,
                            &colors,
                        );
                    });
                }

                // ── Bottom-left sender/file info + Bottom-right action toolbar ──
                if !self.zoom.full_screen_video && !is_stream_mode && !is_theme_preview {
                    ui.scope(|ui| {
                        ui.multiply_opacity(chrome_opacity);
                        self.paint_bottom_actions(
                            ui,
                            viewport_rect,
                            &item,
                            backend,
                            &colors,
                            world_now_ms,
                            &mut events,
                        );
                    });
                }

                // ── Save/Copy toast banner (`st::mediaviewSaveMsgCheck` + `mediaviewSaveMsgStyle`) ──
                if let Some(toast) = &self.toast {
                    let elapsed = (world_now_ms - toast.shown_at_ms).max(0) as u64;
                    if elapsed < MEDIAVIEW_SAVE_MSG_SHOWN_MS + 500 {
                        let alpha = if elapsed <= MEDIAVIEW_SAVE_MSG_SHOWN_MS {
                            1.0
                        } else {
                            1.0 - (elapsed - MEDIAVIEW_SAVE_MSG_SHOWN_MS) as f32 / 500.0
                        };
                        let toast_rect = Rect::from_center_size(
                            Pos2::new(viewport_rect.center().x, viewport_rect.min.y + 84.0),
                            Vec2::new(340.0, 44.0),
                        );
                        painter.rect_filled(
                            toast_rect,
                            CornerRadius::same(8),
                            with_opacity(mv_save_bg, alpha),
                        );
                        let check_rect = Rect::from_min_size(
                            Pos2::new(toast_rect.min.x + 12.0, toast_rect.center().y - 12.0),
                            Vec2::splat(24.0),
                        );
                        paint_centered_style_icon_alpha(
                            ctx,
                            &painter,
                            &st::mediaview_save_msg_check,
                            check_rect,
                            &colors,
                            alpha,
                        );
                        painter.text(
                            Pos2::new(check_rect.max.x + 8.0, toast_rect.center().y),
                            Align2::LEFT_CENTER,
                            &toast.text,
                            font_id_from_style(st::mediaview_font),
                            with_opacity(mv_save_fg, alpha),
                        );
                    }
                }
            });

        events
    }

    fn update_active_texture(&mut self, ctx: &egui::Context, world_now_ms: i64) {
        let rot = self.zoom.rotation;
        let fh = self.zoom.flip_h;
        let fv = self.zoom.flip_v;

        match &mut self.content {
            ActiveContent::Photo {
                base_image,
                texture,
                rendered_rotation,
                rendered_flip_h,
                rendered_flip_v,
                ..
            } => {
                if texture.is_none()
                    || *rendered_rotation != rot
                    || *rendered_flip_h != fh
                    || *rendered_flip_v != fv
                {
                    let transformed = transform_dynamic_image(base_image, rot, fh, fv);
                    let rgba = transformed.to_rgba8();
                    let img = ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    );
                    if let Some(tex) = texture {
                        tex.set(img, TextureOptions::LINEAR);
                    } else {
                        *texture = Some(ctx.load_texture(
                            "tg_media_view_photo",
                            img,
                            TextureOptions::LINEAR,
                        ));
                    }
                    *rendered_rotation = rot;
                    *rendered_flip_h = fh;
                    *rendered_flip_v = fv;
                }
            }
            ActiveContent::Video {
                player,
                texture,
                rendered_rotation,
                rendered_flip_h,
                rendered_flip_v,
            } => {
                let outcome = player.step_video(world_now_ms);
                let needs_transform_refresh =
                    *rendered_rotation != rot || *rendered_flip_h != fh || *rendered_flip_v != fv;

                let frame_to_upload = match outcome {
                    Ok(FramePopOutcome::Ready { frame, .. }) => Some(frame),
                    _ if texture.is_none() || needs_transform_refresh => {
                        player.current_video_frame().cloned()
                    }
                    _ => None,
                };

                if let Some(frame) = frame_to_upload
                    && let FrameData::Rgba(bytes) = &frame.data
                {
                    let img = if rot != 0 || fh || fv {
                        if let Some(rgba_buf) =
                            image::RgbaImage::from_raw(frame.width, frame.height, bytes.clone())
                        {
                            let dyn_img = DynamicImage::ImageRgba8(rgba_buf);
                            let transformed = transform_dynamic_image(&dyn_img, rot, fh, fv);
                            let out = transformed.to_rgba8();
                            ColorImage::from_rgba_unmultiplied(
                                [out.width() as usize, out.height() as usize],
                                out.as_raw(),
                            )
                        } else {
                            ColorImage::from_rgba_unmultiplied(
                                [frame.width as usize, frame.height as usize],
                                bytes,
                            )
                        }
                    } else {
                        ColorImage::from_rgba_unmultiplied(
                            [frame.width as usize, frame.height as usize],
                            bytes,
                        )
                    };
                    if let Some(tex) = texture {
                        tex.set(img, TextureOptions::LINEAR);
                    } else {
                        *texture = Some(ctx.load_texture(
                            "tg_media_view_video",
                            img,
                            TextureOptions::LINEAR,
                        ));
                    }
                    *rendered_rotation = rot;
                    *rendered_flip_h = fh;
                    *rendered_flip_v = fv;
                }

                let dur = player
                    .total_duration_ms()
                    .max(self.gallery.current().map_or(0, |i| i.duration_ms));
                let pos = player.position_ms(world_now_ms);
                let playing = !player.is_paused() && player.stage() == PlayerStage::Started;
                self.controls.update_playback(playing, pos, dur, 1.0);
                if playing {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }
            ActiveContent::None
            | ActiveContent::VideoStream(_)
            | ActiveContent::ThemePreview(_)
            | ActiveContent::Error(_) => {}
        }
    }

    fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        let is_video = matches!(self.content, ActiveContent::Video { .. });
        let (
            esc,
            left,
            right,
            space,
            k_key,
            j_key,
            l_key,
            h_key,
            v_key,
            plus,
            minus,
            zero,
            save,
            copy,
            ctrl,
            alt,
        ) = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let alt = i.modifiers.alt;
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::K),
                i.key_pressed(Key::J),
                i.key_pressed(Key::L),
                i.key_pressed(Key::H),
                i.key_pressed(Key::V),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
                i.key_pressed(Key::Num0),
                ctrl && i.key_pressed(Key::S),
                ctrl && i.key_pressed(Key::C),
                ctrl,
                alt,
            )
        });

        if esc {
            if self.more_menu_open {
                self.more_menu_open = false;
            } else if self.ocr.active {
                if !self.ocr.selection.clear() {
                    self.ocr.active = false;
                }
            } else if self.zoom.full_screen_video {
                self.zoom.toggle_fullscreen_video(136.0);
                self.controls.in_fullscreen = false;
            } else {
                self.close();
                events.push(OverlayEvent::Closed);
            }
            return;
        }

        if ctrl && ctx.input(|i| i.key_pressed(Key::A)) && self.ocr.active {
            if let Some(ref res) = self.ocr.result {
                self.ocr.selection.select_all(res);
            }
            return;
        }

        if save && let Some(item) = self.gallery.current().cloned() {
            self.trigger_save(&item, backend, world_now_ms, events);
            return;
        }
        if copy {
            if self.ocr.active
                && let Some(text) = self.ocr.copyable_text()
            {
                ctx.copy_text(text);
                self.show_toast(strings::OCR_TEXT_COPIED_TOAST, world_now_ms);
                return;
            }
            if let Some(item) = self.gallery.current().cloned() {
                self.trigger_copy(&item, backend, world_now_ms, events);
                return;
            }
        }

        if is_video {
            if space || k_key {
                self.toggle_video_play_pause(world_now_ms);
                return;
            }
            if j_key {
                self.seek_relative(-SEEK_STEP_LONG_MS, world_now_ms);
                return;
            }
            if l_key {
                self.seek_relative(SEEK_STEP_LONG_MS, world_now_ms);
                return;
            }
            if alt && (left || right) && !self.controls.timestamps.is_empty() {
                let progress = self.controls.displayed_progress();
                let target_pos = if right {
                    self.controls.next_timestamp(progress).map(|t| t.position)
                } else {
                    self.controls
                        .prev_timestamp(progress)
                        .map(|t| t.position)
                        .or(Some(0.0))
                };
                if let Some(frac) = target_pos {
                    let target_ms = (frac * self.controls.duration_ms as f64).round() as i64;
                    self.seek_video(target_ms, world_now_ms);
                }
                return;
            }
            if self.zoom.full_screen_video && (left || right) {
                let delta = if right {
                    SEEK_STEP_SHORT_MS
                } else {
                    -SEEK_STEP_SHORT_MS
                };
                self.seek_relative(delta, world_now_ms);
                return;
            }
        }

        if left {
            self.navigate(backend, -1, world_now_ms);
        } else if right {
            self.navigate(backend, 1, world_now_ms);
        } else if h_key {
            self.zoom.flip_h = !self.zoom.flip_h;
        } else if v_key {
            self.zoom.flip_v = !self.zoom.flip_v;
        } else if ctrl && plus {
            self.zoom.zoom_in(None);
        } else if ctrl && minus {
            self.zoom.zoom_out(None);
        } else if ctrl && zero {
            self.zoom.zoom_reset();
        }
    }

    fn toggle_video_play_pause(&mut self, world_now_ms: i64) {
        let mut now_paused = None;
        if let ActiveContent::Video { player, .. } = &mut self.content {
            if player.stage() == PlayerStage::Finished {
                let _ = player.seek(0, Some(world_now_ms));
                player.start(world_now_ms);
                now_paused = Some(false);
            } else if player.is_paused() {
                player.resume(world_now_ms);
                now_paused = Some(false);
            } else {
                player.pause(world_now_ms);
                now_paused = Some(true);
            }
            self.controls.note_activity(world_now_ms);
        }
        if let Some(p) = now_paused {
            let _ = self.playback_sponsored.set_paused(p, world_now_ms);
        }
    }

    fn seek_relative(&mut self, delta_ms: i64, world_now_ms: i64) {
        if let ActiveContent::Video { player, .. } = &mut self.content {
            let dur = player.total_duration_ms().max(0);
            let cur = player.position_ms(world_now_ms);
            let target = (cur + delta_ms).clamp(0, dur.max(0));
            let _ = player.seek(target, Some(world_now_ms));
            let _ = player.step_video(world_now_ms);
            self.controls.note_activity(world_now_ms);
        }
    }

    fn handle_playback_action(
        &mut self,
        action: PlaybackControlsAction,
        viewport_rect: Rect,
        item: &MediaItem,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        self.controls.note_activity(world_now_ms);
        match action {
            PlaybackControlsAction::Play => {
                if let ActiveContent::Video { player, .. } = &mut self.content {
                    if player.stage() == PlayerStage::Finished {
                        let _ = player.seek(0, Some(world_now_ms));
                    }
                    player.start(world_now_ms);
                }
                let _ = self.playback_sponsored.set_paused(false, world_now_ms);
            }
            PlaybackControlsAction::Pause => {
                if let ActiveContent::Video { player, .. } = &mut self.content {
                    player.pause(world_now_ms);
                }
                let _ = self.playback_sponsored.set_paused(true, world_now_ms);
            }
            PlaybackControlsAction::SeekProgress(pos_ms)
            | PlaybackControlsAction::SeekFinished(pos_ms) => {
                self.seek_video(pos_ms, world_now_ms);
            }
            PlaybackControlsAction::VolumeChanged(vol) => {
                self.volume = vol;
                if let ActiveContent::Video { player, .. } = &mut self.content {
                    let _ = player.set_volume(vol);
                }
            }
            PlaybackControlsAction::VolumeToggled => {
                self.volume = self.controls.volume;
                if let ActiveContent::Video { player, .. } = &mut self.content {
                    let _ = player.set_volume(self.volume);
                }
            }
            PlaybackControlsAction::SpeedChanged(speed) => {
                self.speed = speed;
                if let ActiveContent::Video { player, .. } = &mut self.content {
                    let _ = player.set_speed(speed, world_now_ms);
                }
            }
            PlaybackControlsAction::ToggleFullScreen => {
                self.zoom.toggle_fullscreen_video(136.0);
                self.controls.in_fullscreen = self.zoom.full_screen_video;
            }
            PlaybackControlsAction::ToPictureInPicture => {
                let prev = std::mem::replace(&mut self.content, ActiveContent::None);
                if let ActiveContent::Video { player, .. } = prev {
                    let screen = PipRect::new(
                        viewport_rect.min.x.round() as i32,
                        viewport_rect.min.y.round() as i32,
                        viewport_rect.width().round() as i32,
                        viewport_rect.height().round() as i32,
                    );
                    let pip = PipWindow::new(item.clone(), *player, screen, self.volume);
                    self.open = false;
                    events.push(OverlayEvent::EnterPip(Box::new(pip)));
                } else {
                    self.content = prev;
                }
            }
        }
    }

    fn paint_header(
        &mut self,
        ui: &mut Ui,
        viewport_rect: Rect,
        item: &MediaItem,
        colors: &MediaColors<'_>,
        events: &mut Vec<OverlayEvent>,
    ) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter();
        let control_fg = colors.color_or("mediaviewControlFg", MEDIAVIEW_CONTROL_FG);
        let control_bg = colors.color_or("mediaviewControlBg", MEDIAVIEW_CONTROL_BG);
        let accent = colors.color_or("windowBgActive", Color32::from_rgb(0x40, 0xA7, 0xE3));
        let header_rect =
            Rect::from_min_size(viewport_rect.min, Vec2::new(viewport_rect.width(), 56.0));
        // Subtle top gradient scrim for contrast.
        painter.rect_filled(
            header_rect,
            CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(0, 0, 0, 96),
        );

        // Sender monogram circle + name + date.
        let avatar_center = Pos2::new(header_rect.min.x + 32.0, header_rect.center().y);
        painter.circle_filled(avatar_center, 16.0, accent);
        let initial = item
            .sender_name
            .chars()
            .next()
            .unwrap_or('T')
            .to_uppercase()
            .to_string();
        painter.text(
            avatar_center,
            Align2::CENTER_CENTER,
            initial,
            font_id_from_style(st::mediaview_thick_font),
            Color32::WHITE,
        );

        let text_x = header_rect.min.x + 58.0;
        painter.text(
            Pos2::new(text_x, header_rect.min.y + 18.0),
            Align2::LEFT_CENTER,
            &item.sender_name,
            font_id_from_style(st::mediaview_thick_font),
            control_fg,
        );
        let subtitle = match self.gallery.counter_text() {
            Some(counter) => format!("{}  ·  {}", item.date_text, counter),
            None => item.date_text.clone(),
        };
        painter.text(
            Pos2::new(text_x, header_rect.min.y + 38.0),
            Align2::LEFT_CENTER,
            subtitle,
            font_id_from_style(st::mediaview_font),
            MEDIAVIEW_SUB_FG,
        );

        // Right-side controls: Zoom percentage pill (`-`, `100%`, `+`) + Close button (`st::mediaviewTitleClose`).
        let close_rect = Rect::from_min_size(
            Pos2::new(header_rect.max.x - 46.0, header_rect.min.y + 10.0),
            Vec2::splat(36.0),
        );
        let close_resp = ui
            .interact(close_rect, Id::unique("mv_close_btn"), Sense::click())
            .on_hover_text(tg_lang::tr::lng_close());
        if close_resp.hovered() {
            painter.circle_filled(close_rect.center(), 17.0, MEDIAVIEW_CONTROL_BG_OVER);
        }
        paint_centered_style_icon(
            &ctx,
            painter,
            &st::mediaview_title_close,
            close_rect,
            colors,
        );
        if close_resp.clicked() {
            self.close();
            events.push(OverlayEvent::Closed);
        }

        if !item.kind.shows_playback_controls() {
            let zoom_pill = Rect::from_min_size(
                Pos2::new(close_rect.min.x - 118.0, header_rect.min.y + 13.0),
                Vec2::new(108.0, 30.0),
            );
            painter.rect_filled(zoom_pill, CornerRadius::same(15), control_bg);

            let out_rect = Rect::from_min_size(zoom_pill.min, Vec2::new(28.0, 30.0));
            let pct_rect = Rect::from_min_size(
                Pos2::new(out_rect.max.x, zoom_pill.min.y),
                Vec2::new(52.0, 30.0),
            );
            let in_rect = Rect::from_min_size(
                Pos2::new(pct_rect.max.x, zoom_pill.min.y),
                Vec2::new(28.0, 30.0),
            );

            let out_resp = ui.interact(out_rect, Id::unique("mv_zoom_out"), Sense::click());
            let pct_resp = ui.interact(pct_rect, Id::unique("mv_zoom_reset"), Sense::click());
            let in_resp = ui.interact(in_rect, Id::unique("mv_zoom_in"), Sense::click());

            painter.text(
                out_rect.center(),
                Align2::CENTER_CENTER,
                "\u{2212}",
                font_id_from_style(st::mediaview_thick_font),
                control_fg,
            );
            painter.text(
                pct_rect.center(),
                Align2::CENTER_CENTER,
                format!("{}%", self.zoom.zoom_percent()),
                font_id_from_style(st::mediaview_font),
                control_fg,
            );
            painter.text(
                in_rect.center(),
                Align2::CENTER_CENTER,
                "+",
                font_id_from_style(st::mediaview_thick_font),
                control_fg,
            );

            if out_resp.clicked() {
                self.zoom.zoom_out(None);
            }
            if pct_resp.clicked() {
                self.zoom.zoom_reset();
            }
            if in_resp.clicked() {
                self.zoom.zoom_in(None);
            }
        }
    }

    fn paint_caption(
        &mut self,
        ui: &mut Ui,
        viewport_rect: Rect,
        caption: &TextWithEntities,
        bottom_offset: f32,
        colors: &MediaColors<'_>,
    ) {
        let caption_bg = colors.color_or("mediaviewCaptionBg", MEDIAVIEW_CAPTION_BG);
        let caption_fg = colors.color_or("mediaviewCaptionFg", MEDIAVIEW_CAPTION_FG);
        let max_w = (viewport_rect.width() - 64.0).clamp(240.0, 560.0);
        let job = build_caption_layout_job(
            caption,
            max_w - 2.0 * MEDIAVIEW_CAPTION_PAD_X,
            &self.revealed_spoilers,
        );
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let text_size = galley.size();
        let pill_w = (text_size.x + 2.0 * MEDIAVIEW_CAPTION_PAD_X).min(max_w);
        let pill_h = (text_size.y + 2.0 * MEDIAVIEW_CAPTION_PAD_Y).min(120.0);
        let pill_rect = Rect::from_min_size(
            Pos2::new(
                viewport_rect.center().x - pill_w * 0.5,
                viewport_rect.max.y - bottom_offset - pill_h - MEDIAVIEW_CAPTION_MARGIN,
            ),
            Vec2::new(pill_w, pill_h),
        );

        let painter = ui.painter();
        painter.rect_filled(
            pill_rect,
            CornerRadius::same(MEDIAVIEW_CAPTION_RADIUS as u8),
            caption_bg,
        );
        painter.galley(
            Pos2::new(
                pill_rect.min.x + MEDIAVIEW_CAPTION_PAD_X,
                pill_rect.min.y + MEDIAVIEW_CAPTION_PAD_Y,
            ),
            galley,
            caption_fg,
        );

        // Clicking caption reveals any spoiler entities.
        let has_spoiler = caption
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Spoiler);
        if has_spoiler {
            let cap_resp = ui.interact(pill_rect, Id::unique("mv_caption_spoiler"), Sense::click());
            if cap_resp.clicked() {
                for (idx, ent) in caption.entities.iter().enumerate() {
                    if ent.entity_type() == EntityType::Spoiler {
                        self.revealed_spoilers.insert(idx);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_bottom_actions(
        &mut self,
        ui: &mut Ui,
        viewport_rect: Rect,
        item: &MediaItem,
        backend: &dyn MediaBackend,
        colors: &MediaColors<'_>,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter();
        let footer_h = 54.0;
        let footer_rect = Rect::from_min_size(
            Pos2::new(viewport_rect.min.x, viewport_rect.max.y - footer_h),
            Vec2::new(viewport_rect.width(), footer_h),
        );
        // `OverlayWidget::paint`: photos and videos get no file-name line and no bar behind the
        // bottom-right buttons; only a document shows its name and size.
        let shows_file_line = item.kind == MediaItemKind::Document;
        if shows_file_line {
            painter.rect_filled(
                footer_rect,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(0, 0, 0, 96),
            );
        }

        // Bottom-right icon buttons (`st::mediaviewRotate`, `Share`, `Save`, `More`).
        let btn_sz = Vec2::splat(38.0);
        let y = footer_rect.center().y - btn_sz.y * 0.5;
        let mut x = footer_rect.max.x - 16.0 - btn_sz.x;

        let more_rect = Rect::from_min_size(Pos2::new(x, y), btn_sz);
        x -= btn_sz.x + 6.0;
        let save_rect = Rect::from_min_size(Pos2::new(x, y), btn_sz);
        x -= btn_sz.x + 6.0;
        let forward_rect = Rect::from_min_size(Pos2::new(x, y), btn_sz);
        x -= btn_sz.x + 6.0;
        let rotate_rect = Rect::from_min_size(Pos2::new(x, y), btn_sz);

        // Bottom-left metadata: file name + dimensions / rotation (clipped clear of buttons).
        let rot_suffix = if self.zoom.rotation != 0 {
            format!(" · {}°", self.zoom.rotation)
        } else {
            String::new()
        };
        let info_line = format!(
            "{}  ·  {}×{}{}",
            item.file_name, item.width, item.height, rot_suffix
        );
        let info_left = footer_rect.min.x + st::mediaview_text_left.0 as f32;
        let info_max_w = (rotate_rect.min.x - 12.0 - info_left).max(40.0);
        let info_font = font_id_from_style(st::mediaview_font);
        let info_clip = Rect::from_min_max(
            Pos2::new(info_left, footer_rect.min.y),
            Pos2::new(info_left + info_max_w, footer_rect.max.y),
        );
        if shows_file_line {
            let fitted_info = ellipsize_single_line(ui, &info_line, &info_font, info_max_w);
            painter.with_clip_rect(info_clip).text(
                Pos2::new(info_left, footer_rect.center().y),
                Align2::LEFT_CENTER,
                fitted_info,
                info_font,
                MEDIAVIEW_SUB_FG,
            );
        }

        let more_resp = ui
            .interact(more_rect, ui.scope_id().with("mv_act_more"), Sense::click())
            .on_hover_text("More");
        more_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "More"));
        let save_label = tg_lang::tr::lng_mediaview_save_as();
        let save_resp = ui
            .interact(save_rect, ui.scope_id().with("mv_act_save"), Sense::click())
            .on_hover_text(&save_label);
        save_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &save_label));
        let fwd_label = tg_lang::tr::lng_mediaview_forward();
        let fwd_resp = ui
            .interact(
                forward_rect,
                ui.scope_id().with("mv_act_forward"),
                Sense::click(),
            )
            .on_hover_text(&fwd_label);
        fwd_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &fwd_label));
        let rot_resp = ui
            .interact(
                rotate_rect,
                ui.scope_id().with("mv_act_rotate"),
                Sense::click(),
            )
            .on_hover_text(strings::ROTATE_CW_90);
        rot_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, strings::ROTATE_CW_90));

        for (r, hovered) in [
            (more_rect, more_resp.hovered() || self.more_menu_open),
            (save_rect, save_resp.hovered()),
            (forward_rect, fwd_resp.hovered()),
            (rotate_rect, rot_resp.hovered()),
        ] {
            if hovered {
                painter.circle_filled(r.center(), 18.0, MEDIAVIEW_CONTROL_BG_OVER);
            }
        }

        paint_centered_style_icon(&ctx, painter, &st::mediaview_more, more_rect, colors);
        paint_centered_style_icon(&ctx, painter, &st::mediaview_save, save_rect, colors);
        paint_centered_style_icon(&ctx, painter, &st::mediaview_share, forward_rect, colors);
        paint_centered_style_icon(&ctx, painter, &st::mediaview_rotate, rotate_rect, colors);

        if rot_resp.clicked() {
            self.zoom.rotate_cw();
        }
        if fwd_resp.clicked() && item.allows_forward {
            self.trigger_forward(item, backend, world_now_ms, events);
        }
        if save_resp.clicked() && item.allows_save {
            self.trigger_save(item, backend, world_now_ms, events);
        }
        if more_resp.clicked() {
            self.more_menu_open = !self.more_menu_open;
        }

        // ── More context menu (`OverlayWidget::fillContextMenuActions`) ──
        if self.more_menu_open {
            self.paint_more_menu(ui, more_rect, item, backend, colors, world_now_ms, events);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_more_menu(
        &mut self,
        ui: &mut Ui,
        anchor_rect: Rect,
        item: &MediaItem,
        backend: &dyn MediaBackend,
        colors: &MediaColors<'_>,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        let menu_bg = colors.color_or("mediaviewMenuBg", MEDIAVIEW_MENU_BG);
        let menu_bg_over = colors.color_or("mediaviewMenuBgOver", MEDIAVIEW_MENU_BG_OVER);
        let menu_fg = colors.color_or("mediaviewMenuFg", MEDIAVIEW_MENU_FG);
        let copy_label = if matches!(self.content, ActiveContent::Video { .. }) {
            tg_lang::tr::lng_mediaview_copy_frame()
        } else {
            tg_lang::tr::lng_mediaview_copy()
        };
        let mut entries: Vec<(&str, String, bool)> = vec![
            ("show_in_chat", tg_lang::tr::lng_context_to_msg(), false),
            ("rotate", strings::ROTATE_CW_90.to_owned(), false),
        ];
        if item.allows_save {
            if matches!(self.content, ActiveContent::Photo { .. }) {
                entries.push((
                    "recognize_text",
                    strings::RECOGNIZE_TEXT_LABEL.to_owned(),
                    false,
                ));
            }
            entries.push(("save_as", tg_lang::tr::lng_mediaview_save_as(), false));
            entries.push(("copy", copy_label, false));
            entries.push(("open_external", tg_lang::tr::lng_media_open_with(), false));
        }
        if matches!(
            item.kind,
            MediaItemKind::Photo
                | MediaItemKind::Video
                | MediaItemKind::Gif
                | MediaItemKind::RoundVideo
        ) && item.attached_stickers_media.is_some()
        {
            entries.push((
                "attached_stickers",
                tg_lang::tr::lng_context_attached_stickers(),
                false,
            ));
        }
        if item.allows_forward {
            entries.push(("forward", tg_lang::tr::lng_mediaview_forward(), false));
        }
        if item.allows_delete {
            entries.push(("delete", tg_lang::tr::lng_mediaview_delete(), true));
        }

        let menu_w = 210.0;
        let row_h = 32.0;
        let menu_h = entries.len() as f32 * row_h + 12.0;
        let menu_rect = Rect::from_min_size(
            Pos2::new(anchor_rect.max.x - menu_w, anchor_rect.min.y - menu_h - 8.0),
            Vec2::new(menu_w, menu_h),
        );

        let painter = ui.painter();
        painter.rect_filled(menu_rect, CornerRadius::same(8), menu_bg);
        painter.rect_stroke(
            menu_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 24)),
            egui::StrokeKind::Inside,
        );

        let mut chosen: Option<&str> = None;
        for (idx, (action_id, label, danger)) in entries.iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(
                    menu_rect.min.x + 6.0,
                    menu_rect.min.y + 6.0 + idx as f32 * row_h,
                ),
                Vec2::new(menu_w - 12.0, row_h),
            );
            let resp = ui.interact(
                row,
                ui.scope_id().with(("mv_menu_item", *action_id)),
                Sense::click(),
            );
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                painter.rect_filled(row, CornerRadius::same(5), menu_bg_over);
            }
            painter.text(
                Pos2::new(row.min.x + 12.0, row.center().y),
                Align2::LEFT_CENTER,
                label,
                font_id_from_style(st::mediaview_font),
                if *danger {
                    MEDIAVIEW_MENU_DANGER_FG
                } else {
                    menu_fg
                },
            );
            if resp.clicked() {
                chosen = Some(*action_id);
            }
        }

        if let Some(action_id) = chosen {
            self.more_menu_open = false;
            match action_id {
                "show_in_chat" => {
                    backend.show_in_chat(item.peer_id, item.msg_id);
                    events.push(OverlayEvent::ShowInChat {
                        peer_id: item.peer_id,
                        msg_id: item.msg_id,
                    });
                    self.close();
                }
                "rotate" => {
                    self.zoom.rotate_cw();
                }
                "recognize_text" => {
                    let _ = self.recognize_current_photo(world_now_ms);
                }
                "save_as" => {
                    self.trigger_save(item, backend, world_now_ms, events);
                }
                "copy" => {
                    self.trigger_copy(item, backend, world_now_ms, events);
                }
                "open_external" => {
                    self.trigger_open_external(item, backend, world_now_ms, events);
                }
                "attached_stickers" => {
                    if let Some(media) = item.attached_stickers_media.clone() {
                        let media_label = if !item.file_name.is_empty() {
                            item.file_name.clone()
                        } else if item.kind.is_video_like() {
                            "Video".to_owned()
                        } else {
                            "Photo".to_owned()
                        };
                        events.push(OverlayEvent::AttachedStickers { media, media_label });
                        self.close();
                    }
                }
                "forward" => {
                    self.trigger_forward(item, backend, world_now_ms, events);
                }
                "delete" if backend.delete_item(item.id).is_ok() => {
                    self.gallery.remove_current();
                    events.push(OverlayEvent::Deleted(item.id));
                    if self.gallery.is_empty() {
                        self.close();
                        events.push(OverlayEvent::Closed);
                    } else {
                        let _ = self.load_current_item(backend, world_now_ms, 0, false);
                    }
                }
                _ => {}
            }
        }
    }

    /// Returns the active decode/load error message when the viewer is in its error state.
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        match &self.content {
            ActiveContent::Error(msg) => Some(msg.as_str()),
            _ => None,
        }
    }

    /// Triggers saving the currently displayed gallery item into `save_directory` using a
    /// sanitized, unique filename that cannot escape `save_directory` or overwrite existing files.
    pub fn save_current_item(
        &mut self,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        if let Some(item) = self.gallery.current().cloned() {
            self.trigger_save(&item, backend, world_now_ms, events);
        }
    }

    /// Triggers "Open in External App" on the currently displayed gallery item, emitting
    /// [`OverlayEvent::ConfirmOpenExternal`] when the file type requires a launch warning.
    pub fn request_open_external_current(
        &mut self,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        if let Some(item) = self.gallery.current().cloned() {
            self.trigger_open_external(&item, backend, world_now_ms, events);
        }
    }

    fn trigger_open_external(
        &mut self,
        item: &MediaItem,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        let safe_name = tg_platform::file_safety::sanitize_filename(&item.file_name);
        let ext = tg_media::document::file_extension(&safe_name);
        let needs_warning = ext.is_empty()
            || tg_media::document::is_ip_revealing_filename(&safe_name)
            || matches!(
                tg_media::document::detect_name_type(&safe_name),
                tg_media::document::NameType::Executable | tg_media::document::NameType::Unknown
            );
        if needs_warning {
            events.push(OverlayEvent::ConfirmOpenExternal {
                item_id: item.id,
                file_name: safe_name,
            });
            return;
        }
        match backend.open_in_external_app(item.id) {
            Ok(path) => {
                self.show_toast(strings::OPENED_IN_EXTERNAL_APP, world_now_ms);
                events.push(OverlayEvent::OpenExternal(path));
            }
            Err(err) => {
                self.show_toast(format!("Failed to open file: {err}"), world_now_ms);
            }
        }
    }

    fn trigger_save(
        &mut self,
        item: &MediaItem,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        let dest = match tg_platform::file_safety::resolve_safe_path_in_dir(
            &self.save_directory,
            &item.file_name,
        ) {
            Ok(p) => p,
            Err(err) => {
                self.show_toast(
                    format!("{}: {err}", strings::SAVE_FAILED_PREFIX),
                    world_now_ms,
                );
                return;
            }
        };
        match backend.save_to_disk(item.id, &dest) {
            Ok(saved) => {
                let downloads = tg_lang::tr::lng_mediaview_downloads();
                let toast = if item.kind.is_video_like() {
                    tg_lang::tr::lng_mediaview_video_saved_to(&downloads)
                } else {
                    tg_lang::tr::lng_mediaview_saved_to(&downloads)
                };
                self.show_toast(toast, world_now_ms);
                events.push(OverlayEvent::Saved(saved));
            }
            Err(err) => {
                self.show_toast(
                    format!("{}: {err}", strings::SAVE_FAILED_PREFIX),
                    world_now_ms,
                );
            }
        }
    }

    fn trigger_copy(
        &mut self,
        item: &MediaItem,
        backend: &dyn MediaBackend,
        world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        if backend.copy_item(item.id).is_ok() {
            self.show_toast(strings::COPIED_TO_CLIPBOARD, world_now_ms);
            events.push(OverlayEvent::Copied(item.id));
        }
    }

    fn trigger_forward(
        &mut self,
        item: &MediaItem,
        _backend: &dyn MediaBackend,
        _world_now_ms: i64,
        events: &mut Vec<OverlayEvent>,
    ) {
        if item.allows_forward {
            events.push(OverlayEvent::Forward(item.id));
        }
    }

    /// Paints recognized OCR line boxes, drag selection highlights, and a floating Copy Text pill.
    /// Returns `true` if pointer input on the photo was consumed by OCR text selection.
    fn paint_and_handle_ocr_overlay(
        &mut self,
        ui: &mut Ui,
        viewport_rect: Rect,
        content_rect: Rect,
        colors: &MediaColors<'_>,
        world_now_ms: i64,
    ) -> bool {
        let Some(result) = self.ocr.result.clone() else {
            return false;
        };
        let coord_size = Vec2::new(
            result.coordinate_width.max(1) as f32,
            result.coordinate_height.max(1) as f32,
        );
        let rot = self.zoom.rotation;
        let fh = self.zoom.flip_h;
        let fv = self.zoom.flip_v;
        let painter = ui.painter();

        // Subtle outlines around all recognized text lines.
        let line_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 28);
        let line_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 195, 255, 110));
        for line in &result.lines {
            let screen = image_rect_to_screen(line.rect, coord_size, content_rect, rot, fh, fv);
            painter.rect_filled(screen, CornerRadius::same(3), line_fill);
            painter.rect_stroke(
                screen,
                CornerRadius::same(3),
                line_stroke,
                egui::StrokeKind::Inside,
            );
        }

        // Handle drag selection inside `content_rect`.
        let mut consumed = false;
        let ocr_resp = ui.interact(
            content_rect,
            Id::unique("mv_ocr_select_layer"),
            Sense::click_and_drag(),
        );
        if let Some(hover) = ocr_resp.hover_pos()
            && OcrSelection::position_at(&result, hover, content_rect, rot, fh, fv, false).is_some()
        {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        if ocr_resp.drag_started()
            && let Some(pos) = ocr_resp.interact_pointer_pos()
            && let Some(anchor) =
                OcrSelection::position_at(&result, pos, content_rect, rot, fh, fv, false)
        {
            self.ocr.selection.start(anchor);
            consumed = true;
        }
        if ocr_resp.dragged()
            && self.ocr.selection.is_selecting()
            && let Some(pos) = ocr_resp.interact_pointer_pos()
        {
            if let Some(focus) =
                OcrSelection::position_at(&result, pos, content_rect, rot, fh, fv, true)
            {
                self.ocr.selection.update_focus(focus);
            }
            ui.ctx().set_cursor_icon(CursorIcon::Text);
            consumed = true;
        }
        if ocr_resp.drag_stopped() && self.ocr.selection.is_selecting() {
            self.ocr.selection.finish();
            consumed = true;
        }
        if ocr_resp.clicked()
            && let Some(pos) = ocr_resp.interact_pointer_pos()
            && OcrSelection::position_at(&result, pos, content_rect, rot, fh, fv, false).is_none()
        {
            self.ocr.selection.clear();
            consumed = true;
        }

        // Paint selected character spans.
        let sel_fill = Color32::from_rgba_unmultiplied(64, 167, 227, 115);
        for span in self.ocr.selection.spans(&result) {
            if let Some(img_rect) = OcrSelection::span_image_rect(&result, span) {
                let screen = image_rect_to_screen(img_rect, coord_size, content_rect, rot, fh, fv);
                painter.rect_filled(screen, CornerRadius::same(2), sel_fill);
            }
        }

        // Floating OCR action pill (Select All / Copy Text / Done).
        let pill_rect = Rect::from_center_size(
            Pos2::new(viewport_rect.center().x, viewport_rect.min.y + 78.0),
            Vec2::new(260.0, 32.0),
        );
        let control_bg = colors.color_or("mediaviewControlBg", MEDIAVIEW_CONTROL_BG);
        let control_fg = colors.color_or("mediaviewControlFg", MEDIAVIEW_CONTROL_FG);
        painter.rect_filled(pill_rect, CornerRadius::same(16), control_bg);

        let select_all_rect = Rect::from_min_size(pill_rect.min, Vec2::new(92.0, 32.0));
        let copy_rect = Rect::from_min_size(
            Pos2::new(select_all_rect.max.x, pill_rect.min.y),
            Vec2::new(108.0, 32.0),
        );
        let done_rect = Rect::from_min_size(
            Pos2::new(copy_rect.max.x, pill_rect.min.y),
            Vec2::new(60.0, 32.0),
        );

        let sa_resp = ui.interact(
            select_all_rect,
            Id::unique("mv_ocr_select_all"),
            Sense::click(),
        );
        let cp_resp = ui.interact(copy_rect, Id::unique("mv_ocr_copy"), Sense::click());
        let dn_resp = ui.interact(done_rect, Id::unique("mv_ocr_done"), Sense::click());

        painter.text(
            select_all_rect.center(),
            Align2::CENTER_CENTER,
            strings::SELECT_ALL_OCR_TEXT_LABEL,
            font_id_from_style(st::mediaview_font),
            control_fg,
        );
        painter.text(
            copy_rect.center(),
            Align2::CENTER_CENTER,
            strings::COPY_RECOGNIZED_TEXT_LABEL,
            font_id_from_style(st::mediaview_thick_font),
            MEDIAVIEW_TEXT_LINK_FG,
        );
        painter.text(
            done_rect.center(),
            Align2::CENTER_CENTER,
            tg_lang::tr::lng_close(),
            font_id_from_style(st::mediaview_font),
            control_fg,
        );

        if sa_resp.clicked() {
            self.ocr.selection.select_all(&result);
            consumed = true;
        }
        if cp_resp.clicked() {
            let text = self
                .ocr
                .copyable_text()
                .unwrap_or_else(|| recognized_text_in_reading_order(&result));
            if !text.is_empty() {
                ui.ctx().copy_text(text);
                self.show_toast(strings::OCR_TEXT_COPIED_TOAST, world_now_ms);
            }
            consumed = true;
        }
        if dn_resp.clicked() {
            self.ocr.active = false;
            self.ocr.selection.clear();
            consumed = true;
        }

        consumed
    }
}

/// Applies clockwise rotation (`0/90/180/270`) and optional horizontal/vertical flip to a `DynamicImage`.
pub use tg_app_core::core_types::misc::transform_dynamic_image;

/// Builds a rich-styled `egui::text::LayoutJob` for a `tg_text::TextWithEntities` caption.
#[must_use]
pub fn build_caption_layout_job(
    caption: &TextWithEntities,
    wrap_width: f32,
    revealed_spoilers: &HashSet<usize>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width.max(80.0);

    let text = &caption.text;
    if text.is_empty() {
        return job;
    }

    // Collect UTF-8 split boundaries from all valid entity ranges.
    let mut cuts = vec![0usize, text.len()];
    let mut resolved: Vec<(usize, usize, usize, EntityType)> = Vec::new();
    for (idx, ent) in caption.entities.iter().enumerate() {
        if ent.offset() < 0 || ent.length() <= 0 {
            continue;
        }
        if let Some(range) = utf16_range_to_utf8(text, ent.offset() as usize, ent.length() as usize)
            && range.start < range.end
            && range.end <= text.len()
        {
            cuts.push(range.start);
            cuts.push(range.end);
            resolved.push((idx, range.start, range.end, ent.entity_type()));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    for window in cuts.windows(2) {
        let start = window[0];
        let end = window[1];
        if start >= end {
            continue;
        }
        let mut is_bold = false;
        let mut is_italic = false;
        let mut is_underline = false;
        let mut is_strike = false;
        let mut is_mono = false;
        let mut is_link = false;
        let mut hidden_spoiler = false;

        for &(ent_idx, s, e, kind) in &resolved {
            if s <= start && end <= e {
                match kind {
                    EntityType::Bold => is_bold = true,
                    EntityType::Italic => is_italic = true,
                    EntityType::Underline => is_underline = true,
                    EntityType::StrikeOut => is_strike = true,
                    EntityType::Code | EntityType::Pre => is_mono = true,
                    EntityType::Url
                    | EntityType::CustomUrl
                    | EntityType::Mention
                    | EntityType::MentionName
                    | EntityType::Hashtag
                    | EntityType::Cashtag
                    | EntityType::BotCommand
                    | EntityType::Email
                    | EntityType::Phone => is_link = true,
                    EntityType::Spoiler if !revealed_spoilers.contains(&ent_idx) => {
                        hidden_spoiler = true;
                    }
                    _ => {}
                }
            }
        }

        let color = if hidden_spoiler {
            Color32::from_rgba_unmultiplied(255, 255, 255, 60)
        } else if is_link {
            MEDIAVIEW_TEXT_LINK_FG
        } else {
            MEDIAVIEW_CAPTION_FG
        };
        let mut fmt = TextFormat {
            font_id: FontId::new(
                if is_bold { 13.8 } else { 13.0 },
                if is_mono {
                    FontFamily::Monospace
                } else {
                    FontFamily::Proportional
                },
            ),
            color,
            italics: is_italic,
            underline: if is_underline || is_link {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            strikethrough: if is_strike {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            background: if hidden_spoiler {
                Color32::from_rgba_unmultiplied(255, 255, 255, 48)
            } else if is_mono {
                Color32::from_rgba_unmultiplied(255, 255, 255, 24)
            } else {
                Color32::TRANSPARENT
            },
            ..TextFormat::default()
        };
        if is_bold {
            fmt.color = if is_link {
                MEDIAVIEW_TEXT_LINK_FG
            } else {
                Color32::WHITE
            };
        }

        let slice = &text[start..end];
        job.append(slice, 0.0, fmt);
    }
    job
}

fn ellipsize_single_line(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let full = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE);
    if full.size().x <= max_width {
        return text.to_owned();
    }
    let ellipsis = "\u{2026}";
    let mut best = String::new();
    for (idx, _) in text.char_indices() {
        let candidate = format!("{}{}", &text[..idx], ellipsis);
        let w = ui
            .painter()
            .layout_no_wrap(candidate.clone(), font.clone(), Color32::WHITE)
            .size()
            .x;
        if w <= max_width {
            best = candidate;
        } else {
            break;
        }
    }
    if best.is_empty() {
        ellipsis.to_owned()
    } else {
        best
    }
}

#[cfg(test)]
#[path = "tests_uip_f.rs"]
#[cfg(test)]
mod tests_uip_f;

#[cfg(test)]
#[path = "tests_wp74i_album_strip.rs"]
#[cfg(test)]
mod tests_wp74i_album_strip;
