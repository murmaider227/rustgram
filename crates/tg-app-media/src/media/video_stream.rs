//! Live video stream viewer (`media_view_video_stream`), video-viewer sponsored message overlay
//! (`media_view_playback_sponsored`), and `.tdesktop-theme`/`.tdesktop-palette` theme-file preview
//! for [`crate::media::overlay::MediaOverlayWidget`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/view/media_view_video_stream.h`
//! - `Telegram/SourceFiles/media/view/media_view_video_stream.cpp`
//! - `Telegram/SourceFiles/media/view/media_view_playback_sponsored.h`
//! - `Telegram/SourceFiles/media/view/media_view_playback_sponsored.cpp`
//! - `Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp` (`_themePreview` / `showThemePreview`)

use std::collections::HashSet;

use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_theme::{Color as ThemeColor, Colorizer, ThemeInstance, load_theme_from_bytes};

use crate::media::backend::MediaItemId;
use tg_app_core::core_types::dto::sponsored::SponsoredMessageItem;

// =============================================================================
// 1. Live Video Stream Viewer (`media_view_video_stream.h/.cpp`)
// =============================================================================

/// Maximum number of non-self top paid donors shown in the stream header podium
/// (`Ui::MaxTopPaidDonorsShown()`).
pub const MAX_TOP_PAID_DONORS_SHOWN: usize = 3;

/// Stream video quality tier requested by the viewer (`Calls::VideoQuality`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoStreamQuality {
    /// Low bandwidth preview (`360p`).
    Low,
    /// Standard quality (`720p`).
    Medium,
    /// Full source resolution (`1080p`+).
    #[default]
    Full,
}

impl VideoStreamQuality {
    /// Short pill label displayed in the stream quality toggle button.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "360p",
            Self::Medium => "720p",
            Self::Full => "1080p",
        }
    }

    /// Cycles to the next quality tier (`Full -> Medium -> Low -> Full`).
    #[must_use]
    pub const fn cycle(self) -> Self {
        match self {
            Self::Full => Self::Medium,
            Self::Medium => Self::Low,
            Self::Low => Self::Full,
        }
    }
}

/// Top paid Stars donor entry in a live video stream (`Data::MessageReactionsTopPaid`
/// mapped by `TopVideoStreamDonors` in `media_view_video_stream.cpp:64-86`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamDonor {
    /// Donor peer ID.
    pub peer_id: i64,
    /// Display name.
    pub name: String,
    /// Total Telegram Stars donated in this stream.
    pub stars: u32,
    /// True if this entry represents the current user.
    pub is_my: bool,
}

/// Filters and caps the top donors list identically to `TopVideoStreamDonors` in
/// `media_view_video_stream.cpp:64-86` (keeps up to `MAX_TOP_PAID_DONORS_SHOWN` non-self donors
/// while always retaining `is_my` entries encountered before the cap).
#[must_use]
pub fn top_video_stream_donors(donors: &[VideoStreamDonor]) -> Vec<VideoStreamDonor> {
    let mut still = MAX_TOP_PAID_DONORS_SHOWN;
    let mut result = Vec::with_capacity(donors.len().min(MAX_TOP_PAID_DONORS_SHOWN + 1));
    for item in donors {
        result.push(item.clone());
        if !item.is_my {
            still = still.saturating_sub(1);
            if still == 0 {
                break;
            }
        }
    }
    result
}

/// Returns the peer IDs holding podium places (`TopDonorPlaces` in `media_view_video_stream.cpp:88-104`).
#[must_use]
pub fn top_donor_places(donors: &[VideoStreamDonor]) -> Vec<i64> {
    top_video_stream_donors(donors)
        .into_iter()
        .take(MAX_TOP_PAID_DONORS_SHOWN)
        .map(|d| d.peer_id)
        .collect()
}

/// Live chat comment rendered in the video-stream comments overlay (`Calls::Group::MessagesUi`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamComment {
    /// Message identifier inside the group call.
    pub id: i64,
    /// Sender peer ID.
    pub sender_peer_id: i64,
    /// Sender display name.
    pub sender_name: String,
    /// Plain comment text.
    pub text: String,
    /// Paid Stars attached to this comment (`0` for free comments).
    pub paid_stars: u32,
}

pub use tg_app_core::core_types::misc::VideoStreamDescriptor;

/// Connection / playback status of [`VideoStreamViewer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStreamStatus {
    /// Joining call / fetching initial broadcast chunks (shows shimmer loading gradient).
    Connecting,
    /// Stream is live and rendering frames.
    Live,
    /// Stream ended cleanly (`Delegate::groupCallFinished`).
    Finished,
    /// Stream failed (`Delegate::groupCallFailed`).
    Failed(String),
}

/// Action emitted by [`VideoStreamViewer::show`].
#[derive(Debug, Clone, PartialEq)]
pub enum VideoStreamAction {
    /// User closed the video stream or the stream finished (`VideoStream::closeRequests`).
    Close,
    /// User changed stream audio volume (`0.0..=1.0`, `VideoStream::setVolume`).
    SetVolume(f64),
    /// User toggled the live comments overlay (`VideoStream::toggleCommentsOn`).
    ToggleComments(bool),
    /// User requested a different video quality tier (`_viewport->qualityRequests`).
    RequestQuality(VideoStreamQuality),
    /// User submitted a live chat comment.
    SendComment(String),
    /// Admin requested deleting a live comment (`_messages->deleteRequests`).
    DeleteComment(i64),
}

/// Interactive RTMP/HLS live video stream viewer (`Media::View::VideoStream`).
pub struct VideoStreamViewer {
    descriptor: VideoStreamDescriptor,
    status: VideoStreamStatus,
    volume: f64,
    muted: bool,
    quality: VideoStreamQuality,
    comments_shown: bool,
    can_manage: bool,
    viewer_count: u32,
    donors: Vec<VideoStreamDonor>,
    comments: Vec<VideoStreamComment>,
    comment_draft: String,
    has_video_frames: bool,
    frame_size: (u32, u32),
    pending_rgba: Option<(u32, u32, Vec<u8>)>,
    texture: Option<TextureHandle>,
    close_requested: bool,
}

impl std::fmt::Debug for VideoStreamViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoStreamViewer")
            .field("descriptor", &self.descriptor)
            .field("status", &self.status)
            .field("volume", &self.volume)
            .field("quality", &self.quality)
            .field("comments_shown", &self.comments_shown)
            .field("viewer_count", &self.viewer_count)
            .field("has_video_frames", &self.has_video_frames)
            .field("comments_len", &self.comments.len())
            .finish()
    }
}

impl VideoStreamViewer {
    /// Creates a new [`VideoStreamViewer`] starting in [`VideoStreamStatus::Connecting`].
    #[must_use]
    pub fn new(descriptor: VideoStreamDescriptor, initial_volume: f64) -> Self {
        Self {
            descriptor,
            status: VideoStreamStatus::Connecting,
            volume: initial_volume.clamp(0.0, 1.0),
            muted: false,
            quality: VideoStreamQuality::Full,
            comments_shown: true,
            can_manage: false,
            viewer_count: 0,
            donors: Vec::new(),
            comments: Vec::new(),
            comment_draft: String::new(),
            has_video_frames: false,
            frame_size: (1280, 720),
            pending_rgba: None,
            texture: None,
            close_requested: false,
        }
    }

    /// Returns the stream descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &VideoStreamDescriptor {
        &self.descriptor
    }

    /// Returns current connection/playback status.
    #[must_use]
    pub const fn status(&self) -> &VideoStreamStatus {
        &self.status
    }

    /// Returns `true` once at least one decoded video frame has arrived (`hasVideoWithFramesValue`).
    #[must_use]
    pub const fn has_video_frames(&self) -> bool {
        self.has_video_frames
    }

    /// Returns `true` when the loading shimmer placeholder is still active (`_loading != nullptr`).
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        !self.has_video_frames && matches!(self.status, VideoStreamStatus::Connecting)
    }

    /// Returns current audio volume in `0.0..=1.0`.
    #[must_use]
    pub const fn volume(&self) -> f64 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// Sets playback volume (`VideoStream::setVolume`).
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.muted = false;
        }
    }

    /// Toggles whether live stream comments are shown (`VideoStream::toggleCommentsOn`).
    pub fn set_comments_shown(&mut self, shown: bool) {
        self.comments_shown = shown;
    }

    /// Returns whether live stream comments are currently shown.
    #[must_use]
    pub const fn comments_shown(&self) -> bool {
        self.comments_shown
    }

    /// Returns the selected quality tier.
    #[must_use]
    pub const fn quality(&self) -> VideoStreamQuality {
        self.quality
    }

    /// Updates viewer count and optional stream title from `phone.getGroupCall`.
    pub fn update_metadata(&mut self, title: Option<String>, viewer_count: u32, can_manage: bool) {
        if let Some(t) = title
            && !t.is_empty()
        {
            self.descriptor.title = t;
        }
        self.viewer_count = viewer_count;
        self.can_manage = can_manage;
    }

    /// Replaces the top paid donors list.
    pub fn set_donors(&mut self, donors: Vec<VideoStreamDonor>) {
        self.donors = top_video_stream_donors(&donors);
    }

    /// Returns the capped top paid donors list.
    #[must_use]
    pub fn donors(&self) -> &[VideoStreamDonor] {
        &self.donors
    }

    /// Appends a live chat comment to the stream overlay.
    pub fn push_comment(&mut self, comment: VideoStreamComment) {
        self.comments.push(comment);
        if self.comments.len() > 200 {
            let excess = self.comments.len() - 200;
            self.comments.drain(0..excess);
        }
    }

    /// Removes a comment by ID.
    pub fn remove_comment(&mut self, id: i64) -> bool {
        let before = self.comments.len();
        self.comments.retain(|c| c.id != id);
        self.comments.len() != before
    }

    /// Returns active live chat comments.
    #[must_use]
    pub fn comments(&self) -> &[VideoStreamComment] {
        &self.comments
    }

    /// Feeds a freshly decoded live RGBA8 video frame, clearing the loading shimmer
    /// (`_call->hasVideoWithFramesValue` -> `_loading = nullptr`).
    pub fn push_rgba_frame(&mut self, width: u32, height: u32, rgba: Vec<u8>) {
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || rgba.len() != expected {
            return;
        }
        self.has_video_frames = true;
        self.frame_size = (width, height);
        self.status = VideoStreamStatus::Live;
        self.pending_rgba = Some((width, height, rgba));
    }

    /// Marks the stream as finished and queues a close request (`Delegate::groupCallFinished`).
    pub fn mark_finished(&mut self) {
        self.status = VideoStreamStatus::Finished;
        self.close_requested = true;
    }

    /// Marks the stream as failed and queues a close request (`Delegate::groupCallFailed`).
    pub fn mark_failed(&mut self, reason: impl Into<String>) {
        self.status = VideoStreamStatus::Failed(reason.into());
        self.close_requested = true;
    }

    /// Returns `true` if a close was requested (e.g. call ended/failed).
    #[must_use]
    pub const fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// Renders the video stream viewport, loading shimmer, donors podium, live comments, and
    /// stream controls inside `rect`.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, world_now_ms: i64) -> Vec<VideoStreamAction> {
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();

        // Upload any freshly pushed live video frame to GPU texture.
        if let Some((w, h, rgba)) = self.pending_rgba.take() {
            let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            if let Some(tex) = &mut self.texture {
                tex.set(img, TextureOptions::LINEAR);
            } else {
                self.texture = Some(ctx.load_texture(
                    format!("tg_video_stream_{}", self.descriptor.call_id),
                    img,
                    TextureOptions::LINEAR,
                ));
            }
        }

        // Responsive stage layout: on narrow/portrait viewports reserve lower space for live chat
        // so comments never overlap the video frame or header pills.
        let is_narrow = rect.width() < 560.0 || rect.height() > rect.width() * 1.15;
        let pad_x = if is_narrow { 12.0 } else { 32.0 };
        let pad_top = if is_narrow { 16.0 } else { 68.0 };
        let comments_below_stage = is_narrow && self.comments_shown;
        let stage_bounds = if comments_below_stage {
            let chat_reserve = (rect.height() * 0.38).clamp(180.0, 320.0);
            Rect::from_min_max(
                Pos2::new(rect.left() + pad_x, rect.top() + pad_top),
                Pos2::new(
                    (rect.right() - pad_x).max(rect.left() + pad_x + 120.0),
                    (rect.bottom() - chat_reserve - 16.0).max(rect.top() + pad_top + 160.0),
                ),
            )
        } else {
            rect.shrink2(Vec2::new(pad_x, pad_top))
        };
        let stage_rect = {
            let fitted = fit_aspect_rect(
                stage_bounds,
                self.frame_size.0.max(1) as f32,
                self.frame_size.1.max(1) as f32,
            );
            if comments_below_stage {
                Rect::from_min_size(Pos2::new(fitted.left(), stage_bounds.top()), fitted.size())
            } else {
                fitted
            }
        };
        let stage_cr = CornerRadius::same(12);

        if let Some(tex) = &self.texture {
            painter.rect_filled(stage_rect, stage_cr, Color32::BLACK);
            painter.image(
                tex.id(),
                stage_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            // Animated `PathShiftGradient`-style shimmer while connecting (`VideoStream::Loading`).
            let reduce_motion = ui.style().animation_time <= 0.0;
            let phase = ((world_now_ms % 1800) as f32 / 1800.0).clamp(0.0, 1.0);
            let base_bg = Color32::from_rgb(0x16, 0x1D, 0x29);
            let shimmer_band = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 22);
            painter.rect_filled(stage_rect, stage_cr, base_bg);
            if self.is_loading() && !reduce_motion {
                let band_w = (stage_rect.width() / 3.0).max(60.0);
                let band_x =
                    stage_rect.left() - band_w + phase * (stage_rect.width() + band_w * 2.0);
                let band = Rect::from_min_size(
                    Pos2::new(band_x, stage_rect.top()),
                    Vec2::new(band_w, stage_rect.height()),
                )
                .intersect(stage_rect);
                if band.is_positive() {
                    painter.rect_filled(band, stage_cr, shimmer_band);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
            painter.rect_stroke(
                stage_rect,
                stage_cr,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 42)),
                StrokeKind::Inside,
            );
            let status_text = match &self.status {
                VideoStreamStatus::Connecting => {
                    if self.descriptor.rtmp_stream {
                        "Connecting to RTMP/HLS live stream\u{2026}"
                    } else {
                        "Connecting to live video stream\u{2026}"
                    }
                }
                VideoStreamStatus::Live => "Live video stream active",
                VideoStreamStatus::Finished => "Live stream ended",
                VideoStreamStatus::Failed(err) => err.as_str(),
            };
            let status_center_y = if is_narrow {
                (stage_rect.top() + 86.0 + stage_rect.bottom()) * 0.5
            } else {
                stage_rect.center().y
            };
            let status_galley = painter.layout(
                status_text.to_owned(),
                FontId::proportional(if is_narrow { 13.0 } else { 15.0 }),
                Color32::from_rgb(0xE8, 0xEE, 0xF5),
                (stage_rect.width() - 28.0).max(80.0),
            );
            painter.galley(
                Pos2::new(
                    stage_rect.center().x - status_galley.size().x * 0.5,
                    status_center_y - status_galley.size().y * 0.5,
                ),
                status_galley,
                Color32::from_rgb(0xE8, 0xEE, 0xF5),
            );
        }

        // Top scrim guarantees 4.5:1+ contrast for white overlay controls over bright video frames.
        let scrim_h: f32 = if is_narrow { 76.0 } else { 54.0 };
        let scrim_rect = Rect::from_min_size(
            stage_rect.min,
            Vec2::new(stage_rect.width(), scrim_h.min(stage_rect.height())),
        );
        painter.rect_filled(
            scrim_rect,
            CornerRadius {
                nw: 12,
                ne: 12,
                sw: 0,
                se: 0,
            },
            Color32::from_rgba_unmultiplied(8, 12, 18, 150),
        );

        // Top overlay header inside stage: LIVE / RTMP badge, title, viewer count, quality, comments toggle, audio, close.
        let inset_x = if is_narrow { 10.0 } else { 14.0 };
        let top_bar = Rect::from_min_size(
            Pos2::new(stage_rect.left() + inset_x, stage_rect.top() + 10.0),
            Vec2::new((stage_rect.width() - inset_x * 2.0).max(80.0), 26.0),
        );
        let badge_label = if self.descriptor.is_live_story {
            "LIVE STORY"
        } else if self.descriptor.rtmp_stream {
            "LIVE · RTMP"
        } else {
            "LIVE"
        };
        let badge_col = match self.status {
            VideoStreamStatus::Live | VideoStreamStatus::Connecting => {
                Color32::from_rgb(0xD9, 0x36, 0x36)
            }
            VideoStreamStatus::Finished | VideoStreamStatus::Failed(_) => {
                Color32::from_rgb(0x5A, 0x63, 0x70)
            }
        };
        let badge_galley = painter.layout_no_wrap(
            badge_label.to_owned(),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        let badge_rect = Rect::from_min_size(
            Pos2::new(top_bar.left(), top_bar.top() + 1.0),
            Vec2::new(badge_galley.size().x + 14.0, 24.0),
        );
        painter.rect_filled(badge_rect, CornerRadius::same(12), badge_col);
        painter.galley(
            Pos2::new(
                badge_rect.left() + 7.0,
                badge_rect.center().y - badge_galley.size().y * 0.5,
            ),
            badge_galley,
            Color32::WHITE,
        );

        // Right-aligned stream buttons first so title truncation never collides with controls.
        let btn_gap = if is_narrow { 4.0 } else { 6.0 };
        let mut btn_right = top_bar.right();

        let close_rect = Rect::from_min_size(
            Pos2::new(btn_right - 26.0, top_bar.top()),
            Vec2::splat(26.0),
        );
        btn_right = close_rect.left() - btn_gap;
        let close_resp = ui.interact(
            close_rect,
            Id::unique(("tg_vstream_close", self.descriptor.call_id)),
            Sense::click(),
        );
        close_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, "Close live stream")
        });
        if close_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            close_rect,
            CornerRadius::same(13),
            Color32::from_rgba_unmultiplied(0, 0, 0, if close_resp.hovered() { 205 } else { 145 }),
        );
        if close_resp.has_focus() {
            painter.rect_stroke(
                close_rect,
                CornerRadius::same(13),
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Outside,
            );
        }
        // Crisp vector 'X' (avoids missing glyph for U+2715 in bundled fonts).
        let xc = close_rect.center();
        let xr = 4.5;
        let x_stroke = Stroke::new(1.7, Color32::WHITE);
        painter.line_segment(
            [
                Pos2::new(xc.x - xr, xc.y - xr),
                Pos2::new(xc.x + xr, xc.y + xr),
            ],
            x_stroke,
        );
        painter.line_segment(
            [
                Pos2::new(xc.x + xr, xc.y - xr),
                Pos2::new(xc.x - xr, xc.y + xr),
            ],
            x_stroke,
        );
        if close_resp.clicked() {
            self.close_requested = true;
            actions.push(VideoStreamAction::Close);
        }

        let qual_label = self.quality.label();
        let qual_w = if is_narrow { 46.0 } else { 52.0 };
        let qual_rect = Rect::from_min_size(
            Pos2::new(btn_right - qual_w, top_bar.top()),
            Vec2::new(qual_w, 26.0),
        );
        btn_right = qual_rect.left() - btn_gap;
        let qual_resp = ui.interact(
            qual_rect,
            Id::unique(("tg_vstream_quality", self.descriptor.call_id)),
            Sense::click(),
        );
        qual_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("Stream quality {qual_label}"),
            )
        });
        if qual_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            qual_rect,
            CornerRadius::same(13),
            Color32::from_rgba_unmultiplied(0, 0, 0, if qual_resp.hovered() { 205 } else { 145 }),
        );
        if qual_resp.has_focus() {
            painter.rect_stroke(
                qual_rect,
                CornerRadius::same(13),
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Outside,
            );
        }
        painter.text(
            qual_rect.center(),
            Align2::CENTER_CENTER,
            qual_label,
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        if qual_resp.clicked() {
            self.quality = self.quality.cycle();
            actions.push(VideoStreamAction::RequestQuality(self.quality));
        }

        let comm_label = if self.comments_shown {
            if is_narrow { "Chat" } else { "Chat On" }
        } else {
            "Chat Off"
        };
        let comm_w = if is_narrow && self.comments_shown {
            46.0
        } else {
            64.0
        };
        let comm_rect = Rect::from_min_size(
            Pos2::new(btn_right - comm_w, top_bar.top()),
            Vec2::new(comm_w, 26.0),
        );
        btn_right = comm_rect.left() - btn_gap;
        let comm_resp = ui.interact(
            comm_rect,
            Id::unique(("tg_vstream_comments_toggle", self.descriptor.call_id)),
            Sense::click(),
        );
        comm_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                if self.comments_shown {
                    "Hide live comments"
                } else {
                    "Show live comments"
                },
            )
        });
        if comm_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            comm_rect,
            CornerRadius::same(13),
            Color32::from_rgba_unmultiplied(0, 0, 0, if comm_resp.hovered() { 205 } else { 145 }),
        );
        if comm_resp.has_focus() {
            painter.rect_stroke(
                comm_rect,
                CornerRadius::same(13),
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Outside,
            );
        }
        painter.text(
            comm_rect.center(),
            Align2::CENTER_CENTER,
            comm_label,
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        if comm_resp.clicked() {
            self.comments_shown = !self.comments_shown;
            actions.push(VideoStreamAction::ToggleComments(self.comments_shown));
        }

        let vol_label = if self.muted || self.volume <= 0.01 {
            "Muted"
        } else {
            "Audio"
        };
        let vol_w = if is_narrow { 48.0 } else { 52.0 };
        let vol_rect = Rect::from_min_size(
            Pos2::new(btn_right - vol_w, top_bar.top()),
            Vec2::new(vol_w, 26.0),
        );
        btn_right = vol_rect.left() - btn_gap;
        let vol_resp = ui.interact(
            vol_rect,
            Id::unique(("tg_vstream_mute", self.descriptor.call_id)),
            Sense::click(),
        );
        vol_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                if self.muted || self.volume <= 0.01 {
                    "Unmute stream audio"
                } else {
                    "Mute stream audio"
                },
            )
        });
        if vol_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            vol_rect,
            CornerRadius::same(13),
            Color32::from_rgba_unmultiplied(0, 0, 0, if vol_resp.hovered() { 205 } else { 145 }),
        );
        if vol_resp.has_focus() {
            painter.rect_stroke(
                vol_rect,
                CornerRadius::same(13),
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Outside,
            );
        }
        painter.text(
            vol_rect.center(),
            Align2::CENTER_CENTER,
            vol_label,
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        if vol_resp.clicked() {
            self.muted = !self.muted;
            actions.push(VideoStreamAction::SetVolume(self.volume()));
        }

        // Stream title + viewer count: inline when room permits, or on its own row below the
        // controls bar on narrow viewports so text never collides with pills.
        let viewer_suffix = if self.viewer_count > 0 {
            format!(" · {} watching", self.viewer_count)
        } else {
            String::new()
        };
        let full_title = format!("{}{}", self.descriptor.title, viewer_suffix);
        let inline_title_x = badge_rect.right() + 10.0;
        let inline_title_max_w = btn_right - inline_title_x - 6.0;
        let mut next_row_y = top_bar.bottom() + 6.0;
        if inline_title_max_w >= 110.0 {
            let clip_rect = Rect::from_min_max(
                Pos2::new(inline_title_x, top_bar.top()),
                Pos2::new(btn_right - 4.0, top_bar.bottom()),
            );
            let title_text = truncate_single_line(
                &painter,
                &full_title,
                FontId::proportional(13.5),
                inline_title_max_w,
            );
            painter.with_clip_rect(clip_rect).text(
                Pos2::new(inline_title_x, badge_rect.center().y),
                Align2::LEFT_CENTER,
                title_text,
                FontId::proportional(13.5),
                Color32::WHITE,
            );
        } else {
            let title_max_w = (stage_rect.width() - inset_x * 2.0).max(60.0);
            let title_text = truncate_single_line(
                &painter,
                &full_title,
                FontId::proportional(12.5),
                title_max_w,
            );
            let row_rect = Rect::from_min_size(
                Pos2::new(stage_rect.left() + inset_x, next_row_y),
                Vec2::new(title_max_w, 18.0),
            );
            painter.with_clip_rect(row_rect).text(
                Pos2::new(row_rect.left(), row_rect.center().y),
                Align2::LEFT_CENTER,
                title_text,
                FontId::proportional(12.5),
                Color32::WHITE,
            );
            next_row_y = row_rect.bottom() + 6.0;
        }

        // Top paid donors strip (`TopVideoStreamDonors`).
        if !self.donors.is_empty() {
            let mut dx = stage_rect.left() + inset_x;
            let dy = next_row_y;
            for (idx, donor) in self.donors.iter().enumerate() {
                let label = format!("#{} {} · {} Stars", idx + 1, donor.name, donor.stars);
                let g = painter.layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
                let star_slot = 13.0;
                let pill_w = g.size().x + 14.0 + star_slot;
                if dx + pill_w > stage_rect.right() - inset_x {
                    break;
                }
                let pill = Rect::from_min_size(Pos2::new(dx, dy), Vec2::new(pill_w, 22.0));
                let fill = if donor.is_my {
                    Color32::from_rgba_unmultiplied(0xC2, 0x7D, 0x00, 230)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 165)
                };
                painter.rect_filled(pill, CornerRadius::same(11), fill);
                paint_star_badge(
                    &painter,
                    Pos2::new(pill.left() + 10.0, pill.center().y),
                    4.5,
                    Color32::from_rgb(0xFF, 0xD5, 0x4F),
                );
                painter.galley(
                    Pos2::new(
                        pill.left() + 7.0 + star_slot,
                        pill.center().y - g.size().y * 0.5,
                    ),
                    g,
                    Color32::WHITE,
                );
                dx = pill.right() + 6.0;
            }
        }

        // Live chat comments overlay: lower-left inside stage on landscape, or cleanly docked
        // below the stage on narrow/portrait viewports so video and chat never collide.
        if self.comments_shown {
            let panel_rect = if comments_below_stage {
                Rect::from_min_max(
                    Pos2::new(stage_rect.left(), stage_rect.bottom() + 12.0),
                    Pos2::new(
                        stage_rect.right(),
                        (rect.bottom() - 12.0).max(stage_rect.bottom() + 140.0),
                    ),
                )
            } else {
                let panel_h = (stage_rect.height() * 0.42).clamp(110.0, 240.0);
                let panel_w = (stage_rect.width() - 28.0).clamp(180.0, 420.0);
                Rect::from_min_size(
                    Pos2::new(
                        stage_rect.left() + 14.0,
                        stage_rect.bottom() - panel_h - 14.0,
                    ),
                    Vec2::new(panel_w, panel_h),
                )
            };
            let panel_w = panel_rect.width();
            painter.rect_filled(
                panel_rect,
                CornerRadius::same(10),
                Color32::from_rgba_unmultiplied(
                    12,
                    16,
                    24,
                    if comments_below_stage { 225 } else { 170 },
                ),
            );
            painter.rect_stroke(
                panel_rect,
                CornerRadius::same(10),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 28)),
                StrokeKind::Inside,
            );
            let donor_ids: HashSet<i64> = top_donor_places(&self.donors).into_iter().collect();
            let list_bottom = panel_rect.bottom() - 36.0;
            let mut y = list_bottom - 6.0;
            for comment in self.comments.iter().rev().take(6) {
                if y < panel_rect.top() + 20.0 {
                    break;
                }
                let star_prefix = if comment.paid_stars > 0 {
                    format!("[{} Stars] ", comment.paid_stars)
                } else {
                    String::new()
                };
                let is_top_donor = donor_ids.contains(&comment.sender_peer_id);
                let text_left = panel_rect.left() + if is_top_donor { 22.0 } else { 10.0 };
                let line = format!("{star_prefix}{}: {}", comment.sender_name, comment.text);
                let g = painter.layout(
                    line,
                    FontId::proportional(12.0),
                    Color32::WHITE,
                    (panel_rect.right() - text_left - 10.0).max(80.0),
                );
                y -= g.size().y + 6.0;
                if y >= panel_rect.top() + 6.0 {
                    if is_top_donor {
                        paint_star_badge(
                            &painter,
                            Pos2::new(panel_rect.left() + 13.0, y + 7.0),
                            4.2,
                            Color32::from_rgb(0xFF, 0xD5, 0x4F),
                        );
                    }
                    painter.galley(Pos2::new(text_left, y), g, Color32::WHITE);
                }
            }

            // Inline comment composer at the bottom of the stream chat overlay.
            let input_rect = Rect::from_min_max(
                Pos2::new(panel_rect.left() + 8.0, list_bottom + 2.0),
                Pos2::new(panel_rect.right() - 8.0, panel_rect.bottom() - 6.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
                ui.horizontal(|ui| {
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.comment_draft)
                            .hint_text("Write a comment\u{2026}")
                            .desired_width((panel_w - 80.0).max(80.0)),
                    );
                    let send_clicked = ui.button(tg_lang::tr::lng_send_button()).clicked()
                        || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if send_clicked {
                        let trimmed = self.comment_draft.trim().to_owned();
                        if !trimmed.is_empty() {
                            self.comment_draft.clear();
                            actions.push(VideoStreamAction::SendComment(trimmed));
                        }
                    }
                });
            });
        }

        actions
    }
}

fn truncate_single_line(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    max_width: f32,
) -> String {
    tg_ui::elide::elide_end(painter, text, &font, max_width)
}

fn paint_star_badge(painter: &egui::Painter, center: Pos2, outer_r: f32, color: Color32) {
    let inner_r = outer_r * 0.45;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn fit_aspect_rect(bounds: Rect, content_w: f32, content_h: f32) -> Rect {
    let bw = bounds.width().max(120.0);
    let bh = bounds.height().max(90.0);
    let scale = (bw / content_w.max(1.0)).min(bh / content_h.max(1.0));
    let w = (content_w * scale).max(120.0);
    let h = (content_h * scale).max(90.0);
    Rect::from_center_size(bounds.center(), Vec2::new(w, h))
}

// =============================================================================
// 2. Video-Viewer Sponsored Message Overlay (`media_view_playback_sponsored.h/.cpp`)
// =============================================================================

/// Minimum delay before showing the first video ad (`kStartDelayMin = 1000ms`).
pub const SPONSORED_START_DELAY_MIN_MS: i64 = 1_000;

/// Minimum extra display window added to `durationMin` (`kDurationMin = 5000ms`).
pub const SPONSORED_DURATION_MIN_MS: i64 = 5_000;

/// Persistent per-video playback sponsored schedule state (`Data::SponsoredForVideoState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsoredForVideoState {
    /// Zero-based index into `SponsoredForVideo::list`.
    pub item_index: usize,
    /// Milliseconds remaining until showing (`> 0`) or elapsed while showing (`<= 0`).
    pub left_till_show_ms: i64,
    /// True before the first schedule initialization.
    pub initial: bool,
}

impl Default for SponsoredForVideoState {
    fn default() -> Self {
        Self {
            item_index: 0,
            left_till_show_ms: 0,
            initial: true,
        }
    }
}

/// Sponsored video ad bundle returned for a video item (`Data::SponsoredForVideo`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SponsoredForVideo {
    /// Ordered sponsored entries to rotate while the video plays.
    pub list: Vec<SponsoredMessageItem>,
    /// Delay before showing the first ad (ms).
    pub start_delay_ms: i64,
    /// Delay between consecutive ads after one closes or times out (ms).
    pub between_delay_ms: i64,
    /// Resumable schedule state.
    pub state: SponsoredForVideoState,
}

/// Events emitted by [`PlaybackSponsoredOverlay`] to the viewer / live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackSponsoredEvent {
    /// Ad became visible; caller should report `messages.viewSponsoredMessage(random_id)`.
    Viewed {
        /// Server `random_id`.
        random_id: Vec<u8>,
    },
    /// User clicked the ad body; caller should call `messages.clickSponsoredMessage` and open `url`.
    Clicked {
        /// Server `random_id`.
        random_id: Vec<u8>,
        /// Target URL.
        url: String,
    },
    /// User clicked the close button before countdown finished (or "Hide Ads") -> open Premium promo.
    PromotePremium,
    /// User clicked "About Telegram Ads" in the sponsored popup menu.
    OpenAbout {
        /// Server `random_id`.
        random_id: Vec<u8>,
    },
    /// User clicked "Report Ad" in the sponsored popup menu.
    Report {
        /// Server `random_id`.
        random_id: Vec<u8>,
    },
    /// Active ad was dismissed by user after countdown elapsed.
    Closed {
        /// Server `random_id`.
        random_id: Vec<u8>,
    },
}

/// Video-viewer sponsored overlay state machine & painter (`Media::View::PlaybackSponsored`).
#[derive(Debug, Clone, Default)]
pub struct PlaybackSponsoredOverlay {
    data: Option<SponsoredForVideo>,
    started: bool,
    paused: bool,
    paused_inside: bool,
    paused_outside: bool,
    start_ms: Option<i64>,
    shown_index: Option<usize>,
    allow_close_at_ms: i64,
    shown_started_at_ms: i64,
    paused_at_ms: Option<i64>,
    menu_open: bool,
    viewed_emitted: HashSet<Vec<u8>>,
}

impl PlaybackSponsoredOverlay {
    /// Creates an empty overlay with no sponsored data attached.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a [`SponsoredForVideo`] list (mirroring `PlaybackSponsored` constructor
    /// callback in `media_view_playback_sponsored.cpp:564-580`).
    pub fn set_data(
        &mut self,
        mut data: SponsoredForVideo,
        now_ms: i64,
    ) -> Vec<PlaybackSponsoredEvent> {
        if data.list.is_empty() {
            self.data = None;
            self.shown_index = None;
            return Vec::new();
        }
        if data.state.initial
            || data.state.item_index > data.list.len()
            || (data.state.item_index == data.list.len() && data.state.left_till_show_ms <= 0)
        {
            data.state.initial = false;
            data.state.item_index = 0;
            data.state.left_till_show_ms = data.start_delay_ms.max(SPONSORED_START_DELAY_MIN_MS);
        }
        self.data = Some(data);
        self.shown_index = None;
        self.menu_open = false;
        self.update(now_ms)
    }

    /// Starts the sponsored overlay playback timer (`PlaybackSponsored::start`).
    pub fn start(&mut self, now_ms: i64) -> Vec<PlaybackSponsoredEvent> {
        self.started = true;
        if !self.paused {
            self.start_ms = Some(now_ms);
            return self.update(now_ms);
        }
        Vec::new()
    }

    /// Sets external pause state (synced with video play/pause, `PlaybackSponsored::setPaused`).
    pub fn set_paused(&mut self, paused: bool, now_ms: i64) -> Vec<PlaybackSponsoredEvent> {
        if self.paused_outside == paused {
            return Vec::new();
        }
        self.paused_outside = paused;
        self.update_paused(now_ms)
    }

    fn set_paused_inside(&mut self, paused: bool, now_ms: i64) -> Vec<PlaybackSponsoredEvent> {
        if self.paused_inside == paused {
            return Vec::new();
        }
        self.paused_inside = paused;
        self.update_paused(now_ms)
    }

    fn update_paused(&mut self, now_ms: i64) -> Vec<PlaybackSponsoredEvent> {
        let mut events = Vec::new();
        let next_paused = self.paused_inside || self.paused_outside;
        if self.paused == next_paused {
            return events;
        }
        if self.started && next_paused {
            events.extend(self.update(now_ms));
        }
        self.paused = next_paused;
        if !self.started {
            return events;
        }
        if self.paused {
            self.start_ms = None;
            self.paused_at_ms = Some(now_ms);
        } else {
            if let Some(paused_at) = self.paused_at_ms.take() {
                let delta = (now_ms - paused_at).max(0);
                self.shown_started_at_ms += delta;
                self.allow_close_at_ms += delta;
            }
            self.start_ms = Some(now_ms);
            events.extend(self.update(now_ms));
        }
        events
    }

    /// Returns the currently visible [`SponsoredMessageItem`], if any.
    #[must_use]
    pub fn visible_message(&self) -> Option<&SponsoredMessageItem> {
        let idx = self.shown_index?;
        self.data.as_ref()?.list.get(idx)
    }

    /// Returns `true` when the active ad's close countdown has completed and clicking `×`
    /// closes the ad instead of promoting Premium (`Close::_allowClose`).
    #[must_use]
    pub fn can_close_at(&self, now_ms: i64) -> bool {
        let effective_now = self.paused_at_ms.unwrap_or(now_ms);
        effective_now >= self.allow_close_at_ms
    }

    /// Returns remaining seconds until close is allowed (`0` once closeable).
    #[must_use]
    pub fn seconds_until_close(&self, now_ms: i64) -> i64 {
        let effective_now = self.paused_at_ms.unwrap_or(now_ms);
        let ms_till = (self.allow_close_at_ms - effective_now).max(0);
        (ms_till + 999) / 1000
    }

    /// Returns a snapshot of the current schedule state (`PlaybackSponsored::computeState`).
    #[must_use]
    pub fn current_state(&self, now_ms: i64) -> Option<SponsoredForVideoState> {
        let data = self.data.as_ref()?;
        let mut st = data.state.clone();
        if let Some(start) = self.start_ms {
            st.left_till_show_ms -= (now_ms - start).max(0);
        }
        Some(st)
    }

    /// Advances the sponsored display/hide state machine to `now_ms` (`PlaybackSponsored::update`).
    pub fn update(&mut self, now_ms: i64) -> Vec<PlaybackSponsoredEvent> {
        let mut events = Vec::new();
        let Some(start) = self.start_ms else {
            return events;
        };
        let Some(ref mut data) = self.data else {
            return events;
        };

        let elapsed = (now_ms - start).max(0);
        let mut computed = data.state.clone();
        computed.left_till_show_ms -= elapsed;

        let msg = data.list.get(computed.item_index).cloned();
        let duration_ms = msg.as_ref().map_or(0i64, |m| {
            let min_d = m.min_display_duration_ms as i64;
            let max_d = m.max_display_duration_ms as i64;
            (min_d + SPONSORED_DURATION_MIN_MS).max(max_d)
        });

        if data.state.left_till_show_ms > 0 && computed.left_till_show_ms <= 0 {
            data.state.left_till_show_ms = 0;
            if let Some(ref m) = msg
                && duration_ms > 0
            {
                let min_d = m.min_display_duration_ms as i64;
                self.shown_index = Some(computed.item_index);
                self.shown_started_at_ms = now_ms;
                self.allow_close_at_ms = now_ms + min_d;
                self.start_ms = Some(now_ms);
                if self.viewed_emitted.insert(m.random_id.clone()) {
                    events.push(PlaybackSponsoredEvent::Viewed {
                        random_id: m.random_id.clone(),
                    });
                }
            } else {
                self.finish();
            }
        } else if data.state.left_till_show_ms <= 0 && computed.left_till_show_ms <= -duration_ms {
            if duration_ms > 0 {
                self.hide_current(now_ms);
            } else {
                self.finish();
            }
        } else {
            if computed.left_till_show_ms <= 0
                && let Some(ref m) = msg
                && duration_ms > 0
                && self.shown_index.is_none()
            {
                let min_d = m.min_display_duration_ms as i64;
                self.shown_index = Some(computed.item_index);
                self.shown_started_at_ms = now_ms;
                self.allow_close_at_ms = now_ms + computed.left_till_show_ms + min_d;
                if self.viewed_emitted.insert(m.random_id.clone()) {
                    events.push(PlaybackSponsoredEvent::Viewed {
                        random_id: m.random_id.clone(),
                    });
                }
            }
            data.state = computed;
            self.start_ms = Some(now_ms);
        }

        events
    }

    /// Manually dismisses the currently visible ad (or advances to the next ad after `between_delay_ms`)
    /// (`PlaybackSponsored::hide`).
    pub fn hide_current(&mut self, now_ms: i64) {
        self.shown_index = None;
        self.menu_open = false;
        self.paused_inside = false;
        if let Some(ref mut data) = self.data {
            data.state.item_index = data.state.item_index.saturating_add(1);
            data.state.left_till_show_ms = data.between_delay_ms.max(SPONSORED_START_DELAY_MIN_MS);
            self.start_ms = if self.paused { None } else { Some(now_ms) };
        }
    }

    fn finish(&mut self) {
        self.shown_index = None;
        self.menu_open = false;
        self.start_ms = None;
        self.data = None;
    }

    /// Handles a click on the close/countdown button, returning the resulting event.
    pub fn click_close_button(&mut self, now_ms: i64) -> Option<PlaybackSponsoredEvent> {
        let msg = self.visible_message()?.clone();
        if self.can_close_at(now_ms) {
            self.hide_current(now_ms);
            Some(PlaybackSponsoredEvent::Closed {
                random_id: msg.random_id,
            })
        } else {
            Some(PlaybackSponsoredEvent::PromotePremium)
        }
    }

    /// Paints the floating video sponsored message card directly above `controls_rect`
    /// (`PlaybackSponsored::Message::paintEvent` + `Close::paintEvent`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        controls_rect: Rect,
        now_ms: i64,
    ) -> Vec<PlaybackSponsoredEvent> {
        let mut events = self.update(now_ms);
        let Some(msg) = self.visible_message().cloned() else {
            return events;
        };

        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let card_w = controls_rect.width().clamp(300.0, 580.0);
        let card_h = 68.0;
        let card_rect = Rect::from_min_size(
            Pos2::new(
                controls_rect.center().x - card_w * 0.5,
                controls_rect.top() - 12.0 - card_h,
            ),
            Vec2::new(card_w, card_h),
        );

        let bg = Color32::from_rgba_unmultiplied(0x18, 0x1E, 0x28, 232);
        let link_fg = Color32::from_rgb(0x6A, 0xB3, 0xF3);
        let fg = Color32::WHITE;
        let sub_fg = Color32::from_rgb(0xC6, 0xD0, 0xDC);

        painter.rect_filled(card_rect, CornerRadius::same(10), bg);
        painter.rect_stroke(
            card_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 26)),
            StrokeKind::Inside,
        );

        // Close / countdown button on right (`Close::paintEvent`).
        let close_rect = Rect::from_center_size(
            Pos2::new(card_rect.right() - 24.0, card_rect.center().y),
            Vec2::splat(34.0),
        );
        let close_resp = ui.interact(
            close_rect,
            Id::unique(("tg_mv_sponsored_close", msg.random_id.as_slice())),
            Sense::click(),
        );
        if close_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let secs_left = self.seconds_until_close(now_ms);
        if secs_left > 0 {
            let effective_now = self.paused_at_ms.unwrap_or(now_ms);
            let total = (self.allow_close_at_ms - self.shown_started_at_ms).max(1) as f32;
            let remain = (self.allow_close_at_ms - effective_now).max(0) as f32;
            let frac = (remain / total).clamp(0.0, 1.0);
            let radius = 12.0;
            let steps = ((20.0 * frac).ceil() as usize).max(2);
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let a = -std::f32::consts::FRAC_PI_2 + (i as f32 / 20.0) * std::f32::consts::TAU;
                pts.push(Pos2::new(
                    close_rect.center().x + a.cos() * radius,
                    close_rect.center().y + a.sin() * radius,
                ));
            }
            painter.add(Shape::line(pts, Stroke::new(1.8, link_fg)));
            painter.text(
                close_rect.center(),
                Align2::CENTER_CENTER,
                secs_left.to_string(),
                FontId::proportional(11.0),
                link_fg,
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            tg_app_core::ui_icons::paint_close_cross(&painter, close_rect.center(), 13.0, link_fg);
        }

        // Left circular sponsor monogram avatar.
        let thumb_center = Pos2::new(card_rect.left() + 30.0, card_rect.center().y);
        painter.circle_filled(thumb_center, 18.0, Color32::from_rgb(0x3B, 0x82, 0xC4));
        let initial = msg
            .title
            .chars()
            .next()
            .unwrap_or('S')
            .to_uppercase()
            .to_string();
        painter.text(
            thumb_center,
            Align2::CENTER_CENTER,
            initial,
            FontId::proportional(15.0),
            Color32::WHITE,
        );

        let text_left = card_rect.left() + 58.0;
        let title_galley =
            painter.layout_no_wrap(msg.title.clone(), FontId::proportional(13.5), fg);
        let max_title_w = (close_rect.left() - text_left - 84.0).max(60.0);
        let title_w = title_galley.size().x.min(max_title_w);
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_left, card_rect.top() + 10.0),
                Vec2::new(title_w, 20.0),
            ))
            .galley(
                Pos2::new(text_left, card_rect.top() + 10.0),
                title_galley,
                fg,
            );

        // "Sponsored" / "Recommended" pill button (`_about`).
        let about_rect = Rect::from_min_size(
            Pos2::new(text_left + title_w + 8.0, card_rect.top() + 9.0),
            Vec2::new(72.0, 20.0),
        );
        let about_resp = ui.interact(
            about_rect,
            Id::unique(("tg_mv_sponsored_about", msg.random_id.as_slice())),
            Sense::click(),
        );
        if about_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            about_rect,
            CornerRadius::same(10),
            Color32::from_rgba_unmultiplied(
                link_fg.r(),
                link_fg.g(),
                link_fg.b(),
                if about_resp.hovered() { 45 } else { 28 },
            ),
        );
        painter.text(
            about_rect.center(),
            Align2::CENTER_CENTER,
            msg.badge_label(),
            FontId::proportional(10.5),
            link_fg,
        );

        // Sponsored message body text.
        let body_max_w = (close_rect.left() - text_left - 8.0).max(80.0);
        let body_galley = painter.layout(
            msg.text.text.clone(),
            FontId::proportional(12.0),
            sub_fg,
            body_max_w,
        );
        painter
            .with_clip_rect(Rect::from_min_max(
                Pos2::new(text_left, card_rect.top() + 31.0),
                Pos2::new(close_rect.left() - 6.0, card_rect.bottom() - 6.0),
            ))
            .galley(
                Pos2::new(text_left, card_rect.top() + 31.0),
                body_galley,
                sub_fg,
            );

        // Card body click area (excluding About & Close buttons).
        let body_resp = ui.interact(
            card_rect,
            Id::unique(("tg_mv_sponsored_body", msg.random_id.as_slice())),
            Sense::click(),
        );

        if close_resp.clicked() {
            if let Some(ev) = self.click_close_button(now_ms) {
                events.push(ev);
            }
        } else if about_resp.clicked() {
            self.menu_open = !self.menu_open;
            let open = self.menu_open;
            events.extend(self.set_paused_inside(open, now_ms));
        } else if body_resp.clicked() {
            events.push(PlaybackSponsoredEvent::Clicked {
                random_id: msg.random_id.clone(),
                url: msg.url.clone(),
            });
        }

        // Popup menu opened from `"Sponsored"` button (`Menu::FillSponsored` in `media_view_playback_sponsored.cpp:530-550`).
        if self.menu_open {
            let menu_rect = Rect::from_min_size(
                Pos2::new(about_rect.left(), card_rect.top() - 96.0),
                Vec2::new(172.0, 90.0),
            );
            painter.rect_filled(
                menu_rect,
                CornerRadius::same(8),
                Color32::from_rgb(0x22, 0x2A, 0x36),
            );
            painter.rect_stroke(
                menu_rect,
                CornerRadius::same(8),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 32)),
                StrokeKind::Inside,
            );
            let rows = [
                ("About Telegram Ads", 0usize),
                ("Hide Ads (Premium)", 1usize),
                ("Report Ad", 2usize),
            ];
            for (label, idx) in rows {
                let r = Rect::from_min_size(
                    Pos2::new(
                        menu_rect.left() + 4.0,
                        menu_rect.top() + 4.0 + idx as f32 * 27.0,
                    ),
                    Vec2::new(menu_rect.width() - 8.0, 26.0),
                );
                let resp = ui.interact(
                    r,
                    Id::unique(("tg_mv_sponsored_menu_row", idx)),
                    Sense::click(),
                );
                if resp.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                    painter.rect_filled(
                        r,
                        CornerRadius::same(5),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                    );
                }
                painter.text(
                    Pos2::new(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(12.0),
                    if idx == 2 {
                        Color32::from_rgb(0xEF, 0x6B, 0x6B)
                    } else {
                        Color32::WHITE
                    },
                );
                if resp.clicked() {
                    self.menu_open = false;
                    events.extend(self.set_paused_inside(false, now_ms));
                    match idx {
                        0 => events.push(PlaybackSponsoredEvent::OpenAbout {
                            random_id: msg.random_id.clone(),
                        }),
                        1 => events.push(PlaybackSponsoredEvent::PromotePremium),
                        2 => {
                            self.hide_current(now_ms);
                            events.push(PlaybackSponsoredEvent::Report {
                                random_id: msg.random_id.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        events
    }
}

// =============================================================================
// 3. Theme-File Preview inside MediaOverlayWidget (`_themePreview` / `showThemePreview`)
// =============================================================================

/// Returns `true` if `file_name` or `mime_type` identifies a `.tdesktop-theme` or
/// `.tdesktop-palette` file (`DocumentData::isTheme()`).
pub use tg_app_core::core_types::misc::is_theme_file;

/// Action emitted from the theme-file preview card inside [`crate::media::overlay::MediaOverlayWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemePreviewAction {
    /// User clicked "Apply Theme" (`_themeApply`).
    Apply {
        /// Viewer media item ID.
        item_id: MediaItemId,
        /// Display name of the theme file.
        theme_name: String,
        /// Original raw `.tdesktop-theme` / `.tdesktop-palette` bytes.
        raw_bytes: Vec<u8>,
    },
    /// User clicked "Share" (`_themeShare`).
    Share(MediaItemId),
    /// User clicked "Cancel" or outside the theme preview frame (`_themeCancel`).
    Cancel,
}

/// Decoded `.tdesktop-theme` / `.tdesktop-palette` preview state rendered inside
/// [`crate::media::overlay::MediaOverlayWidget`].
pub struct ThemeFilePreview {
    /// Media item identifier.
    pub item_id: MediaItemId,
    /// Display title (filename stem or cloud theme title).
    pub theme_name: String,
    /// Optional cloud users count (`_themeCloudData.usersCount`).
    pub users_count: u32,
    /// Original raw bytes passed back when the user clicks "Apply Theme".
    pub raw_bytes: Vec<u8>,
    /// Parsed `tg-theme` palette + optional wallpaper background.
    pub instance: ThemeInstance,
    /// Cached GPU texture for the embedded theme wallpaper, if present.
    bg_texture: Option<TextureHandle>,
    /// Last painted card rectangle (for outside-click dismissal).
    last_card_rect: Rect,
}

impl std::fmt::Debug for ThemeFilePreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThemeFilePreview")
            .field("item_id", &self.item_id)
            .field("theme_name", &self.theme_name)
            .field("users_count", &self.users_count)
            .field("raw_bytes_len", &self.raw_bytes.len())
            .field("has_wallpaper", &self.instance.background.is_some())
            .finish()
    }
}

impl ThemeFilePreview {
    /// Parses `.tdesktop-theme` / `.tdesktop-palette` `bytes` through `tg_theme::load_theme_from_bytes`.
    pub fn from_bytes(
        item_id: MediaItemId,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<Self, String> {
        let instance = load_theme_from_bytes(&bytes, &Colorizer::default(), None, true)
            .map_err(|e| format!("Failed to parse theme file: {e}"))?;
        let theme_name = file_name
            .strip_suffix(".tdesktop-theme")
            .or_else(|| file_name.strip_suffix(".tdesktop-palette"))
            .unwrap_or(file_name)
            .to_owned();
        Ok(Self {
            item_id,
            theme_name: if theme_name.is_empty() {
                "Custom Theme".to_owned()
            } else {
                theme_name
            },
            users_count: 0,
            raw_bytes: bytes,
            instance,
            bg_texture: None,
            last_card_rect: Rect::NOTHING,
        })
    }

    /// Returns the last painted preview card bounds (`_themePreviewRect`).
    #[must_use]
    pub const fn last_card_rect(&self) -> Rect {
        self.last_card_rect
    }

    /// Resolves a palette token by name into an `egui::Color32`.
    #[must_use]
    pub fn palette_color(&self, key: &str, fallback: Color32) -> Color32 {
        self.instance
            .palette
            .color_by_name(key)
            .map_or(fallback, theme_color_to_egui)
    }

    /// Renders the live theme preview card (`media_view_overlay_widget.cpp:6785-6845`) centered
    /// inside `viewport_rect`.
    pub fn show(&mut self, ui: &mut Ui, viewport_rect: Rect) -> Option<ThemePreviewAction> {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();

        let card_w = (viewport_rect.width() - 96.0).clamp(440.0, 680.0);
        let card_h = (viewport_rect.height() - 140.0).clamp(360.0, 500.0);
        let card_rect = Rect::from_center_size(viewport_rect.center(), Vec2::new(card_w, card_h));
        self.last_card_rect = card_rect;

        // Upload embedded theme wallpaper once if present.
        if self.bg_texture.is_none()
            && let Some(ref bg) = self.instance.background
        {
            let img = ColorImage::from_rgba_unmultiplied(
                [bg.width() as usize, bg.height() as usize],
                bg.as_raw(),
            );
            self.bg_texture = Some(ctx.load_texture(
                format!("tg_theme_preview_bg_{}", self.item_id.0),
                img,
                TextureOptions::LINEAR,
            ));
        }

        let window_bg = self.palette_color("windowBg", Color32::from_rgb(0xFF, 0xFF, 0xFF));
        let window_fg = self.palette_color("windowFg", Color32::from_rgb(0x11, 0x11, 0x11));
        let sub_fg = self.palette_color("windowSubTextFg", Color32::from_rgb(0x70, 0x75, 0x79));
        let active_bg = self.palette_color("activeButtonBg", Color32::from_rgb(0x40, 0xA7, 0xE3));
        let active_fg = self.palette_color("activeButtonFg", Color32::WHITE);
        let dialogs_bg = self.palette_color("dialogsBg", window_bg);
        let dialogs_active = self.palette_color("dialogsBgActive", active_bg);
        let dialogs_name_active = self.palette_color("dialogsNameFgActive", Color32::WHITE);
        let msg_in_bg = self.palette_color("msgInBg", Color32::from_rgb(0xF1, 0xF1, 0xF1));
        let msg_out_bg = self.palette_color("msgOutBg", Color32::from_rgb(0xEF, 0xFD, 0xDE));
        let msg_in_fg = self.palette_color("historyTextInFg", window_fg);
        let msg_out_fg = self.palette_color("historyTextOutFg", window_fg);
        let canvas_bg = self.palette_color("windowBgOver", Color32::from_rgb(0xD8, 0xE6, 0xF2));

        let cr = CornerRadius::same(12);
        painter.rect_filled(card_rect, cr, window_bg);
        painter.rect_stroke(
            card_rect,
            cr,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 55)),
            StrokeKind::Inside,
        );

        // Header bar.
        let header_h = 46.0;
        let header_rect = Rect::from_min_size(card_rect.min, Vec2::new(card_w, header_h));
        painter.text(
            Pos2::new(header_rect.left() + 18.0, header_rect.center().y),
            Align2::LEFT_CENTER,
            format!("Theme Preview · {}", self.theme_name),
            FontId::proportional(15.0),
            window_fg,
        );
        if self.users_count > 0 {
            painter.text(
                Pos2::new(header_rect.right() - 18.0, header_rect.center().y),
                Align2::RIGHT_CENTER,
                format!("{} users", self.users_count),
                FontId::proportional(12.0),
                sub_fg,
            );
        }

        // Two-column mini Telegram preview area.
        let footer_h = 54.0;
        let preview_rect = Rect::from_min_max(
            Pos2::new(card_rect.left() + 14.0, header_rect.bottom() + 2.0),
            Pos2::new(
                card_rect.right() - 14.0,
                card_rect.bottom() - footer_h - 6.0,
            ),
        );
        let preview_cr = CornerRadius::same(8);
        painter.rect_filled(preview_rect, preview_cr, canvas_bg);
        if let Some(tex) = &self.bg_texture {
            painter.image(
                tex.id(),
                preview_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Left dialogs column preview.
        let side_w = (preview_rect.width() * 0.34).clamp(130.0, 210.0);
        let side_rect =
            Rect::from_min_size(preview_rect.min, Vec2::new(side_w, preview_rect.height()));
        painter.rect_filled(
            side_rect,
            CornerRadius {
                nw: 8,
                ne: 0,
                sw: 8,
                se: 0,
            },
            dialogs_bg,
        );
        let sample_chats = [
            ("Saved Messages", "Theme preview ready", true),
            ("Design Studio", "Palette tokens applied", false),
            ("Release Notes", "Day & Night contrast ok", false),
        ];
        for (i, (title, sub, selected)) in sample_chats.iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(
                    side_rect.left() + 6.0,
                    side_rect.top() + 8.0 + i as f32 * 52.0,
                ),
                Vec2::new(side_w - 12.0, 46.0),
            );
            if *selected {
                painter.rect_filled(row, CornerRadius::same(8), dialogs_active);
            }
            painter.circle_filled(
                Pos2::new(row.left() + 20.0, row.center().y),
                14.0,
                if *selected {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 64)
                } else {
                    active_bg
                },
            );
            painter.text(
                Pos2::new(row.left() + 42.0, row.top() + 8.0),
                Align2::LEFT_TOP,
                *title,
                FontId::proportional(12.0),
                if *selected {
                    dialogs_name_active
                } else {
                    window_fg
                },
            );
            painter.text(
                Pos2::new(row.left() + 42.0, row.top() + 25.0),
                Align2::LEFT_TOP,
                *sub,
                FontId::proportional(10.5),
                if *selected {
                    dialogs_name_active
                } else {
                    sub_fg
                },
            );
        }

        // Right chat canvas sample bubbles.
        let chat_left = side_rect.right() + 14.0;
        let chat_right = preview_rect.right() - 14.0;
        let in_bubble = Rect::from_min_size(
            Pos2::new(chat_left, preview_rect.top() + 22.0),
            Vec2::new((chat_right - chat_left).min(240.0), 54.0),
        );
        painter.rect_filled(in_bubble, CornerRadius::same(12), msg_in_bg);
        painter.text(
            Pos2::new(in_bubble.left() + 12.0, in_bubble.top() + 10.0),
            Align2::LEFT_TOP,
            "How does this color palette look?",
            FontId::proportional(12.5),
            msg_in_fg,
        );
        painter.text(
            Pos2::new(in_bubble.right() - 10.0, in_bubble.bottom() - 8.0),
            Align2::RIGHT_BOTTOM,
            "12:04",
            FontId::proportional(10.0),
            sub_fg,
        );

        let out_w = (chat_right - chat_left).min(250.0);
        let out_bubble = Rect::from_min_size(
            Pos2::new(chat_right - out_w, in_bubble.bottom() + 14.0),
            Vec2::new(out_w, 54.0),
        );
        painter.rect_filled(out_bubble, CornerRadius::same(12), msg_out_bg);
        painter.text(
            Pos2::new(out_bubble.left() + 12.0, out_bubble.top() + 10.0),
            Align2::LEFT_TOP,
            "Crisp bubbles and readable contrast!",
            FontId::proportional(12.5),
            msg_out_fg,
        );
        painter.text(
            Pos2::new(out_bubble.right() - 24.0, out_bubble.bottom() - 8.0),
            Align2::RIGHT_BOTTOM,
            "12:05",
            FontId::proportional(10.0),
            sub_fg,
        );
        tg_app_core::ui_icons::paint_double_check(
            &painter,
            Pos2::new(out_bubble.right() - 10.0, out_bubble.bottom() - 13.0),
            9.0,
            sub_fg,
        );

        // Footer buttons: Share (left), Cancel + Apply Theme (right) matching `_themeShare`/`_themeCancel`/`_themeApply`.
        let mut out_action = None;
        let footer_y = card_rect.bottom() - footer_h + 10.0;

        let share_rect = Rect::from_min_size(
            Pos2::new(card_rect.left() + 16.0, footer_y),
            Vec2::new(82.0, 34.0),
        );
        let share_resp = ui.interact(
            share_rect,
            Id::unique(("tg_theme_preview_share", self.item_id.0)),
            Sense::click(),
        );
        if share_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            share_rect,
            CornerRadius::same(8),
            if share_resp.hovered() {
                Color32::from_rgba_unmultiplied(active_bg.r(), active_bg.g(), active_bg.b(), 32)
            } else {
                Color32::TRANSPARENT
            },
        );
        painter.text(
            share_rect.center(),
            Align2::CENTER_CENTER,
            "Share",
            FontId::proportional(13.0),
            active_bg,
        );
        if share_resp.clicked() {
            out_action = Some(ThemePreviewAction::Share(self.item_id));
        }

        let apply_rect = Rect::from_min_size(
            Pos2::new(card_rect.right() - 134.0, footer_y),
            Vec2::new(118.0, 34.0),
        );
        let apply_resp = ui.interact(
            apply_rect,
            Id::unique(("tg_theme_preview_apply", self.item_id.0)),
            Sense::click(),
        );
        if apply_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(apply_rect, CornerRadius::same(8), active_bg);
        painter.text(
            apply_rect.center(),
            Align2::CENTER_CENTER,
            "Apply Theme",
            FontId::proportional(13.0),
            active_fg,
        );
        if apply_resp.clicked() {
            out_action = Some(ThemePreviewAction::Apply {
                item_id: self.item_id,
                theme_name: self.theme_name.clone(),
                raw_bytes: self.raw_bytes.clone(),
            });
        }

        let cancel_rect = Rect::from_min_size(
            Pos2::new(apply_rect.left() - 90.0, footer_y),
            Vec2::new(82.0, 34.0),
        );
        let cancel_resp = ui.interact(
            cancel_rect,
            Id::unique(("tg_theme_preview_cancel", self.item_id.0)),
            Sense::click(),
        );
        if cancel_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        painter.rect_filled(
            cancel_rect,
            CornerRadius::same(8),
            if cancel_resp.hovered() {
                Color32::from_rgba_unmultiplied(active_bg.r(), active_bg.g(), active_bg.b(), 32)
            } else {
                Color32::TRANSPARENT
            },
        );
        painter.text(
            cancel_rect.center(),
            Align2::CENTER_CENTER,
            "Cancel",
            FontId::proportional(13.0),
            active_bg,
        );
        if cancel_resp.clicked() {
            out_action = Some(ThemePreviewAction::Cancel);
        }

        out_action
    }
}

fn theme_color_to_egui(c: ThemeColor) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}
