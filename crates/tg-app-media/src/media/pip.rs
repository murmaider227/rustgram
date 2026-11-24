//! Picture-in-Picture (PiP) floating video window with aspect-ratio resizing and magnetic edge snapping.
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view_pip.h`,
//! `Telegram/SourceFiles/media/view/media_view_pip.cpp`, and `media_view.style`.

use egui::{
    Align2, Area, Color32, ColorImage, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::tg_style::st;
use tg_video::{FfmpegStreamDecoder, FrameData, FramePopOutcome, PlayerStage, StreamingPlayer};

use crate::media::backend::MediaItem;
use crate::media::style::{
    MEDIAVIEW_PIP_CONTROLS_FG, MEDIAVIEW_PIP_PLAYBACK_ACTIVE, MEDIAVIEW_PIP_PLAYBACK_INACTIVE,
    MediaColors, VolumeIconLevel, format_duration_secs, format_remaining_secs,
    paint_centered_style_icon,
};

/// Default PiP long-side size (`st::pipDefaultSize = 320px`).
pub const PIP_DEFAULT_SIZE: i32 = st::pip_default_size.0;
/// Minimum PiP side size (`st::pipMinimalSize = 120px`).
pub const PIP_MINIMAL_SIZE: i32 = st::pip_minimal_size.0;
/// Screen edge margin (`st::pipBorderSkip = 20px`).
pub const PIP_BORDER_SKIP: i32 = st::pip_border_skip.0;
/// Magnetic edge-snapping distance (`st::pipBorderSnapArea = 16px`).
pub const PIP_BORDER_SNAP_AREA: i32 = st::pip_border_snap_area.0;
/// Resize handle hit area thickness (`st::pipResizeArea = 10px`).
pub const PIP_RESIZE_AREA: f32 = st::pip_resize_area.0 as f32;

/// Integer 2D rectangle used for deterministic PiP geometry math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PipRect {
    /// Left coordinate.
    pub x: i32,
    /// Top coordinate.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl PipRect {
    /// Creates a new [`PipRect`].
    #[must_use]
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Converts to `egui::Rect`.
    #[must_use]
    pub fn to_egui(self) -> Rect {
        Rect::from_min_size(
            Pos2::new(self.x as f32, self.y as f32),
            Vec2::new(self.w as f32, self.h as f32),
        )
    }

    /// Translates by `(dx, dy)`.
    #[must_use]
    pub const fn translated(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            w: self.w,
            h: self.h,
        }
    }
}

/// Active resize/move handle part (`RectPart` in `media_view_pip.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PipEdge {
    /// Moving the whole window.
    #[default]
    Center,
    /// Left edge resize.
    Left,
    /// Top edge resize.
    Top,
    /// Right edge resize.
    Right,
    /// Bottom edge resize.
    Bottom,
    /// Top-left corner resize.
    TopLeft,
    /// Top-right corner resize.
    TopRight,
    /// Bottom-left corner resize.
    BottomLeft,
    /// Bottom-right corner resize.
    BottomRight,
}

impl PipEdge {
    /// Returns `true` for the four corner resize handles.
    #[must_use]
    pub const fn is_corner(self) -> bool {
        matches!(
            self,
            Self::TopLeft | Self::TopRight | Self::BottomLeft | Self::BottomRight
        )
    }
}

/// Screen edge attachment flags persisted across sessions (`PipPanel::Position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PipAttachments {
    /// Attached to left edge.
    pub left: bool,
    /// Attached to top edge.
    pub top: bool,
    /// Attached to right edge.
    pub right: bool,
    /// Attached to bottom edge.
    pub bottom: bool,
}

/// Maximum allowed PiP dimensions for a screen (`MaxAllowedSizeForScreen` in `media_view_pip.cpp:62-67`).
#[must_use]
pub const fn max_allowed_size_for_screen(screen_w: i32, screen_h: i32) -> (i32, i32) {
    let skip = 3 * PIP_BORDER_SKIP;
    let w = screen_w - skip;
    let h = screen_h - skip;
    (
        if w > PIP_MINIMAL_SIZE {
            w
        } else {
            PIP_MINIMAL_SIZE
        },
        if h > PIP_MINIMAL_SIZE {
            h
        } else {
            PIP_MINIMAL_SIZE
        },
    )
}

/// Magnetically snaps `inner` to screen margins within [`PIP_BORDER_SNAP_AREA`] (`ClampToEdges`).
#[must_use]
pub fn clamp_to_edges(screen: PipRect, inner: PipRect) -> (i32, i32) {
    let skip = PIP_BORDER_SKIP;
    let area = PIP_BORDER_SNAP_AREA;
    let sleft = screen.x + skip;
    let stop = screen.y + skip;
    let sright = screen.x + screen.w - skip;
    let sbottom = screen.y + screen.h - skip;

    let ileft = inner.x;
    let itop = inner.y;
    let iright = inner.x + inner.w;
    let ibottom = inner.y + inner.h;

    let mut shift_x = 0;
    let mut shift_y = 0;
    if iright + shift_x >= sright - area && iright + shift_x < sright + area {
        shift_x += sright - iright;
    }
    if ileft + shift_x >= sleft - area && ileft + shift_x < sleft + area {
        shift_x += sleft - ileft;
    }
    if ibottom + shift_y >= sbottom - area && ibottom + shift_y < sbottom + area {
        shift_y += sbottom - ibottom;
    }
    if itop + shift_y >= stop - area && itop + shift_y < stop + area {
        shift_y += stop - itop;
    }

    let clamped_x = (inner.x + shift_x).clamp(
        screen.x + 4,
        (screen.x + screen.w - inner.w - 4).max(screen.x + 4),
    );
    let clamped_y = (inner.y + shift_y).clamp(
        screen.y + 4,
        (screen.y + screen.h - inner.h - 4).max(screen.y + 4),
    );
    (clamped_x, clamped_y)
}

/// Scales `aspect` to fit or cover `target` while preserving aspect ratio.
fn scale_keeping_aspect(
    aspect_w: i32,
    aspect_h: i32,
    target_w: i32,
    target_h: i32,
    expanding: bool,
) -> (i32, i32) {
    let aw = i64::from(aspect_w.max(1));
    let ah = i64::from(aspect_h.max(1));
    let tw = i64::from(target_w.max(1));
    let th = i64::from(target_h.max(1));
    let w_by_h = (aw * th) / ah;
    let choose_height = if expanding {
        w_by_h >= tw
    } else {
        w_by_h <= tw
    };
    if choose_height {
        ((w_by_h as i32).max(1), target_h.max(1))
    } else {
        let h_by_w = (ah * tw) / aw;
        (target_w.max(1), (h_by_w as i32).max(1))
    }
}

/// Applies an unconstrained drag delta `(dx, dy)` to `original` clamped to `[min_size, max_size]`
/// (`Transformed` in `media_view_pip.cpp:97-145`).
#[must_use]
pub fn transformed_rect(
    original: PipRect,
    min_size: (i32, i32),
    max_size: (i32, i32),
    dx: i32,
    dy: i32,
    by: PipEdge,
) -> PipRect {
    let x1 = original.w - min_size.0;
    let x2 = max_size.0 - original.w;
    let y1 = original.h - min_size.1;
    let y2 = max_size.1 - original.h;
    let mut r = original;
    match by {
        PipEdge::Center => return r.translated(dx, dy),
        PipEdge::TopLeft => {
            let cy = dy.clamp(-y2, y1);
            let cx = dx.clamp(-x2, x1);
            r.y += cy;
            r.h -= cy;
            r.x += cx;
            r.w -= cx;
        }
        PipEdge::TopRight => {
            let cy = dy.clamp(-y2, y1);
            let cx = dx.clamp(-x1, x2);
            r.y += cy;
            r.h -= cy;
            r.w += cx;
        }
        PipEdge::BottomRight => {
            r.h += dy.clamp(-y1, y2);
            r.w += dx.clamp(-x1, x2);
        }
        PipEdge::BottomLeft => {
            let cx = dx.clamp(-x2, x1);
            r.h += dy.clamp(-y1, y2);
            r.x += cx;
            r.w -= cx;
        }
        PipEdge::Left => {
            let cx = dx.clamp(-x2, x1);
            r.x += cx;
            r.w -= cx;
        }
        PipEdge::Top => {
            let cy = dy.clamp(-y2, y1);
            r.y += cy;
            r.h -= cy;
        }
        PipEdge::Right => {
            r.w += dx.clamp(-x1, x2);
        }
        PipEdge::Bottom => {
            r.h += dy.clamp(-y1, y2);
        }
    }
    r
}

/// Constrains a resized PiP rectangle to preserve `aspect_ratio` (`Constrained` in `media_view_pip.cpp:147-242`).
#[must_use]
pub fn constrained_rect(
    original: PipRect,
    min_size: (i32, i32),
    max_size: (i32, i32),
    aspect_ratio: (i32, i32),
    by: PipEdge,
    attached: PipAttachments,
) -> PipRect {
    if by == PipEdge::Center {
        return original;
    }
    if original.w <= 0 && original.h <= 0 {
        return PipRect::new(
            original.x,
            original.y,
            aspect_ratio.0.max(1),
            aspect_ratio.1.max(1),
        );
    }
    let width_larger = i64::from(original.w) * i64::from(aspect_ratio.1.max(1))
        > i64::from(original.h) * i64::from(aspect_ratio.0.max(1));
    let keep_aspect_fit = by.is_corner()
        || (matches!(by, PipEdge::Top | PipEdge::Bottom) && width_larger)
        || (matches!(by, PipEdge::Left | PipEdge::Right) && !width_larger);
    let (dw, dh) = scale_keeping_aspect(
        aspect_ratio.0,
        aspect_ratio.1,
        original.w,
        original.h,
        !keep_aspect_fit,
    );
    let nw = dw.clamp(min_size.0, max_size.0);
    let nh = dh.clamp(min_size.1, max_size.1);

    match by {
        PipEdge::TopLeft => PipRect::new(
            original.x + original.w - nw,
            original.y + original.h - nh,
            nw,
            nh,
        ),
        PipEdge::TopRight => PipRect::new(original.x, original.y + original.h - nh, nw, nh),
        PipEdge::BottomRight => PipRect::new(original.x, original.y, nw, nh),
        PipEdge::BottomLeft => PipRect::new(original.x + original.w - nw, original.y, nw, nh),
        PipEdge::Left => {
            let dy = if attached.top {
                0
            } else if attached.bottom {
                original.h - nh
            } else {
                (original.h - nh) / 2
            };
            PipRect::new(original.x + original.w - nw, original.y + dy, nw, nh)
        }
        PipEdge::Top => {
            let dx = if attached.left {
                0
            } else if attached.right {
                original.w - nw
            } else {
                (original.w - nw) / 2
            };
            PipRect::new(original.x + dx, original.y, nw, nh)
        }
        PipEdge::Right => {
            let dy = if attached.top {
                0
            } else if attached.bottom {
                original.h - nh
            } else {
                (original.h - nh) / 2
            };
            PipRect::new(original.x, original.y + dy, nw, nh)
        }
        PipEdge::Bottom => {
            let dx = if attached.left {
                0
            } else if attached.right {
                original.w - nw
            } else {
                (original.w - nw) / 2
            };
            PipRect::new(original.x + dx, original.y + original.h - nh, nw, nh)
        }
        PipEdge::Center => original,
    }
}

/// Computes the default bottom-right PiP placement inside `screen` for an aspect ratio.
#[must_use]
pub fn default_pip_rect(screen: PipRect, aspect_w: i32, aspect_h: i32) -> PipRect {
    let (w, h) = scale_keeping_aspect(
        aspect_w,
        aspect_h,
        PIP_DEFAULT_SIZE,
        PIP_DEFAULT_SIZE,
        false,
    );
    let w = w.max(PIP_MINIMAL_SIZE);
    let h = h.max(PIP_MINIMAL_SIZE);
    let x = (screen.x + screen.w - w - PIP_BORDER_SKIP).max(screen.x + PIP_BORDER_SKIP);
    let y = (screen.y + screen.h - h - PIP_BORDER_SKIP).max(screen.y + PIP_BORDER_SKIP);
    PipRect::new(x, y, w, h)
}

/// Playback state carried back when enlarging a PiP window into the fullscreen viewer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PipResumeState {
    /// Current playback position in milliseconds.
    pub position_ms: i64,
    /// Whether playback was paused.
    pub paused: bool,
    /// Current volume (`0.0..=1.0`).
    pub volume: f64,
    /// Current playback speed (`0.5..=2.5`).
    pub speed: f64,
}

/// Events emitted by [`PipWindow::show`].
#[derive(Debug, Clone)]
pub enum PipEvent {
    /// User closed the PiP window.
    Closed,
    /// User requested returning the video to the fullscreen media viewer at `resume.position_ms`.
    EnlargeToViewer {
        /// The video item being played.
        item: Box<MediaItem>,
        /// Playback position, paused status, volume, and speed to restore.
        resume: PipResumeState,
    },
}

/// Floating Picture-in-Picture video player (`Media::View::Pip`).
pub struct PipWindow {
    item: MediaItem,
    player: StreamingPlayer<FfmpegStreamDecoder>,
    texture: Option<TextureHandle>,
    rect: PipRect,
    aspect_ratio: (i32, i32),
    attached: PipAttachments,
    volume: f64,
    last_positive_volume: f64,
    drag_edge: Option<PipEdge>,
    drag_start_rect: PipRect,
    drag_accum: Vec2,
    seeking: bool,
    last_world_ms: i64,
}

impl std::fmt::Debug for PipWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipWindow")
            .field("item_id", &self.item.id)
            .field("rect", &self.rect)
            .field("volume", &self.volume)
            .finish()
    }
}

impl PipWindow {
    /// Creates a new floating PiP window taking ownership of an active [`StreamingPlayer`].
    #[must_use]
    pub fn new(
        item: MediaItem,
        player: StreamingPlayer<FfmpegStreamDecoder>,
        screen_rect: PipRect,
        volume: f64,
    ) -> Self {
        let aspect_w = item.width.max(1) as i32;
        let aspect_h = item.height.max(1) as i32;
        let rect = default_pip_rect(screen_rect, aspect_w, aspect_h);
        let vol = volume.clamp(0.0, 1.0);
        Self {
            item,
            player,
            texture: None,
            rect,
            aspect_ratio: (aspect_w, aspect_h),
            attached: PipAttachments {
                right: true,
                bottom: true,
                ..PipAttachments::default()
            },
            volume: vol,
            last_positive_volume: if vol > 0.0 { vol } else { 1.0 },
            drag_edge: None,
            drag_start_rect: rect,
            drag_accum: Vec2::ZERO,
            seeking: false,
            last_world_ms: 0,
        }
    }

    /// Returns the active [`MediaItem`].
    #[must_use]
    pub fn item(&self) -> &MediaItem {
        &self.item
    }

    /// Returns the current PiP window rectangle.
    #[must_use]
    pub const fn rect(&self) -> PipRect {
        self.rect
    }

    /// Returns the current volume in `[0.0, 1.0]`.
    #[must_use]
    pub const fn volume(&self) -> f64 {
        self.volume
    }

    /// Advances video decoding at `world_now_ms` and updates the GPU texture.
    pub fn step(&mut self, ctx: &egui::Context, world_now_ms: i64) {
        self.last_world_ms = world_now_ms;
        if let Ok(FramePopOutcome::Ready { frame, .. }) = self.player.step_video(world_now_ms) {
            self.upload_frame(ctx, &frame);
        } else if self.texture.is_none()
            && let Some(frame) = self.player.current_video_frame().cloned()
        {
            self.upload_frame(ctx, &frame);
        }
        if !self.player.is_paused() && self.player.stage() == PlayerStage::Started {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn upload_frame(&mut self, ctx: &egui::Context, frame: &tg_video::VideoFrame) {
        let FrameData::Rgba(bytes) = &frame.data else {
            return;
        };
        let image = ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            bytes,
        );
        if let Some(tex) = &mut self.texture {
            tex.set(image, TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture(
                format!("tg_pip_video_{}", self.item.id.0),
                image,
                TextureOptions::LINEAR,
            ));
        }
    }

    /// Renders the PiP floating window as an always-on-top `egui::Area` and returns any event.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        screen_rect: Rect,
        world_now_ms: i64,
    ) -> Option<PipEvent> {
        self.step(ctx, world_now_ms);
        ensure_tdesktop_fonts(ctx);
        let colors = MediaColors::from_ctx(ctx);
        let pip_controls_fg = colors.color_or("mediaviewPipControlsFg", MEDIAVIEW_PIP_CONTROLS_FG);
        let pip_playback_active =
            colors.color_or("mediaviewPipPlaybackActive", MEDIAVIEW_PIP_PLAYBACK_ACTIVE);
        let pip_playback_inactive = colors.color_or(
            "mediaviewPipPlaybackInactive",
            MEDIAVIEW_PIP_PLAYBACK_INACTIVE,
        );
        let screen_pip = PipRect::new(
            screen_rect.min.x.round() as i32,
            screen_rect.min.y.round() as i32,
            screen_rect.width().round() as i32,
            screen_rect.height().round() as i32,
        );
        let mut event = None;

        Area::new(Id::unique(("tg_media_pip", self.item.id.0)))
            .order(Order::Foreground)
            .fixed_pos(Pos2::new(self.rect.x as f32, self.rect.y as f32))
            .show(ctx, |ui| {
                let size = Vec2::new(self.rect.w as f32, self.rect.h as f32);
                let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
                let painter = ui.painter();

                // Soft outer drop shadow.
                painter.rect_filled(
                    rect.expand(4.0).translate(Vec2::new(0.0, 2.0)),
                    CornerRadius::same(10),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 96),
                );
                // Video surface background.
                painter.rect_filled(rect, CornerRadius::same(8), Color32::BLACK);

                if let Some(tex) = &self.texture {
                    painter.image(
                        tex.id(),
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                let hovered = ui.rect_contains_pointer(rect);
                let paused = self.player.is_paused();

                // Hit-test buttons (`Pip::setupButtons` geometry).
                let close_rect = Rect::from_min_size(
                    Pos2::new(rect.max.x - 30.0, rect.min.y + 4.0),
                    Vec2::new(26.0, 26.0),
                );
                let enlarge_rect = Rect::from_min_size(
                    Pos2::new(close_rect.min.x - 28.0, rect.min.y + 4.0),
                    Vec2::new(26.0, 26.0),
                );
                let vol_rect = Rect::from_min_size(
                    Pos2::new(rect.min.x + 4.0, rect.min.y + 4.0),
                    Vec2::new(26.0, 26.0),
                );
                let play_rect = Rect::from_center_size(rect.center(), Vec2::new(42.0, 42.0));
                let seek_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + 8.0, rect.max.y - 16.0),
                    Pos2::new(rect.max.x - 8.0, rect.max.y - 4.0),
                );

                // Hover scrim and controls.
                if hovered || paused || self.seeking {
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(8),
                        Color32::from_rgba_unmultiplied(0, 0, 0, 72),
                    );

                    // Top controls: Volume, Enlarge, Close (`st::pip*Icon`).
                    let close_resp = ui
                        .interact(close_rect, Id::unique("pip_close"), Sense::click())
                        .on_hover_text(tg_lang::tr::lng_close());
                    let enlarge_resp = ui
                        .interact(enlarge_rect, Id::unique("pip_enlarge"), Sense::click())
                        .on_hover_text(tg_lang::tr::lng_shortcuts_media_fullscreen());
                    let vol_resp = ui
                        .interact(vol_rect, Id::unique("pip_vol"), Sense::click())
                        .on_hover_text(tg_lang::tr::lng_ringtones_box_volume());

                    let close_icon = if close_resp.hovered() {
                        &st::pip_close_icon_over
                    } else {
                        &st::pip_close_icon
                    };
                    let enlarge_icon = if enlarge_resp.hovered() {
                        &st::pip_enlarge_icon_over
                    } else {
                        &st::pip_enlarge_icon
                    };
                    let vol_icon = match (
                        VolumeIconLevel::from_volume(self.volume),
                        vol_resp.hovered(),
                    ) {
                        (VolumeIconLevel::Off, true) => &st::pip_volume_icon0_over,
                        (VolumeIconLevel::Off, false) => &st::pip_volume_icon0,
                        (VolumeIconLevel::Small, true) => &st::pip_volume_icon1_over,
                        (VolumeIconLevel::Small, false) => &st::pip_volume_icon1,
                        (VolumeIconLevel::Full, true) => &st::pip_volume_icon2_over,
                        (VolumeIconLevel::Full, false) => &st::pip_volume_icon2,
                    };
                    paint_centered_style_icon(ctx, painter, close_icon, close_rect, &colors);
                    paint_centered_style_icon(ctx, painter, enlarge_icon, enlarge_rect, &colors);
                    paint_centered_style_icon(ctx, painter, vol_icon, vol_rect, &colors);

                    if close_resp.clicked() {
                        event = Some(PipEvent::Closed);
                    } else if enlarge_resp.clicked() {
                        event = Some(PipEvent::EnlargeToViewer {
                            item: Box::new(self.item.clone()),
                            resume: PipResumeState {
                                position_ms: self.player.position_ms(world_now_ms),
                                paused: self.player.is_paused(),
                                volume: self.volume,
                                speed: self.player.speed(),
                            },
                        });
                    } else if vol_resp.clicked() {
                        if self.volume > 0.0 {
                            self.last_positive_volume = self.volume;
                            self.volume = 0.0;
                        } else {
                            self.volume = self.last_positive_volume.clamp(0.1, 1.0);
                        }
                        let _ = self.player.set_volume(self.volume);
                    }

                    // Center play/pause badge (`st::pipPlayIconOver` / `st::pipPauseIconOver`).
                    painter.circle_filled(
                        play_rect.center(),
                        20.0,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                    );
                    let center_icon = if paused {
                        &st::pip_play_icon_over
                    } else {
                        &st::pip_pause_icon_over
                    };
                    paint_centered_style_icon(ctx, painter, center_icon, play_rect, &colors);

                    // Bottom seek bar + time labels.
                    let dur_ms = self
                        .player
                        .total_duration_ms()
                        .max(self.item.duration_ms)
                        .max(1);
                    let pos_ms = self.player.position_ms(world_now_ms).clamp(0, dur_ms);
                    let progress = (pos_ms as f32 / dur_ms as f32).clamp(0.0, 1.0);
                    let seek_resp =
                        ui.interact(seek_rect, Id::unique("pip_seek"), Sense::click_and_drag());
                    if (seek_resp.clicked() || seek_resp.dragged())
                        && let Some(ptr) = seek_resp.interact_pointer_pos()
                    {
                        self.seeking = true;
                        let frac = ((ptr.x - seek_rect.min.x) / seek_rect.width()).clamp(0.0, 1.0);
                        let target = (f64::from(frac) * dur_ms as f64).round() as i64;
                        let _ = self.player.seek(target, Some(world_now_ms));
                    } else {
                        self.seeking = false;
                    }

                    let bar = Rect::from_min_max(
                        Pos2::new(seek_rect.min.x, seek_rect.center().y - 1.5),
                        Pos2::new(seek_rect.max.x, seek_rect.center().y + 1.5),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), pip_playback_inactive);
                    let filled = Rect::from_min_max(
                        bar.min,
                        Pos2::new(bar.min.x + bar.width() * progress, bar.max.y),
                    );
                    painter.rect_filled(filled, CornerRadius::same(2), pip_playback_active);

                    let elapsed = format_duration_secs(pos_ms / 1000);
                    let remaining = format_remaining_secs((dur_ms - pos_ms).max(0) / 1000);
                    let font = font_id_from_style(st::pip_playback_font);
                    painter.text(
                        Pos2::new(seek_rect.min.x, seek_rect.min.y - 8.0),
                        Align2::LEFT_CENTER,
                        elapsed,
                        font.clone(),
                        pip_controls_fg,
                    );
                    painter.text(
                        Pos2::new(seek_rect.max.x, seek_rect.min.y - 8.0),
                        Align2::RIGHT_CENTER,
                        remaining,
                        font,
                        pip_controls_fg,
                    );
                }

                // Drag move / corner resize / click play-pause / double-click enlarge.
                if response.double_clicked() && event.is_none() {
                    event = Some(PipEvent::EnlargeToViewer {
                        item: Box::new(self.item.clone()),
                        resume: PipResumeState {
                            position_ms: self.player.position_ms(world_now_ms),
                            paused: self.player.is_paused(),
                            volume: self.volume,
                            speed: self.player.speed(),
                        },
                    });
                } else if response.clicked() && event.is_none() {
                    if self.player.is_paused() {
                        self.player.resume(world_now_ms);
                    } else {
                        self.player.pause(world_now_ms);
                    }
                }

                if response.drag_started()
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    self.drag_edge = Some(hit_test_edge(rect, pos));
                    self.drag_start_rect = self.rect;
                    self.drag_accum = Vec2::ZERO;
                }
                if response.dragged()
                    && let Some(edge) = self.drag_edge
                {
                    self.drag_accum += response.drag_delta();
                    let min_sz = (PIP_MINIMAL_SIZE, PIP_MINIMAL_SIZE);
                    let max_sz = max_allowed_size_for_screen(screen_pip.w, screen_pip.h);
                    let raw = transformed_rect(
                        self.drag_start_rect,
                        min_sz,
                        max_sz,
                        self.drag_accum.x.round() as i32,
                        self.drag_accum.y.round() as i32,
                        edge,
                    );
                    self.rect = constrained_rect(
                        raw,
                        min_sz,
                        max_sz,
                        self.aspect_ratio,
                        edge,
                        self.attached,
                    );
                }
                if response.drag_stopped() {
                    let (nx, ny) = clamp_to_edges(screen_pip, self.rect);
                    self.rect.x = nx;
                    self.rect.y = ny;
                    self.drag_edge = None;
                }

                painter.rect_stroke(
                    rect,
                    CornerRadius::same(8),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
                    egui::StrokeKind::Inside,
                );
            });

        event
    }
}

fn hit_test_edge(rect: Rect, pos: Pos2) -> PipEdge {
    let left = pos.x - rect.min.x <= PIP_RESIZE_AREA;
    let right = rect.max.x - pos.x <= PIP_RESIZE_AREA;
    let top = pos.y - rect.min.y <= PIP_RESIZE_AREA;
    let bottom = rect.max.y - pos.y <= PIP_RESIZE_AREA;
    match (left, top, right, bottom) {
        (true, true, _, _) => PipEdge::TopLeft,
        (_, true, true, _) => PipEdge::TopRight,
        (true, _, _, true) => PipEdge::BottomLeft,
        (_, _, true, true) => PipEdge::BottomRight,
        (true, _, _, _) => PipEdge::Left,
        (_, true, _, _) => PipEdge::Top,
        (_, _, true, _) => PipEdge::Right,
        (_, _, _, true) => PipEdge::Bottom,
        _ => PipEdge::Center,
    }
}
