//! Floating circular round-video mini-player widget backed by `tg-audio::FloatController`
//! and `tg-video::ClipReader`.
//!
//! Port origin: `Telegram/SourceFiles/media/player/media_player_float.h` and
//! `Telegram/SourceFiles/media/player/media_player_float.cpp`.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{
    Area, Color32, ColorImage, Id, Order, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions,
    Vec2,
};
use tg_audio::{
    DragOutcome, FloatColumn, FloatController, FloatCorner, FullMsgId, Point, Rect as FloatRect,
    SectionArea, Size as FloatSize,
};
use tg_ui::tg_style::st;
use tg_video::{ClipMode, ClipReader, FrameData, Size as VideoSize};

use crate::media::style::{
    MEDIA_PLAYER_FLOAT_MARGIN, MEDIA_PLAYER_FLOAT_SIZE, MediaColors,
    paint_centered_style_icon_alpha, with_opacity,
};

/// Total outer diameter of the floating round-video widget (`2 * margin + float_size = 152px`).
pub const FLOAT_WIDGET_OUTER_SIZE: i32 =
    (2.0 * MEDIA_PLAYER_FLOAT_MARGIN + MEDIA_PLAYER_FLOAT_SIZE) as i32;

/// Actions emitted by [`RoundVideoFloat::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundFloatEvent {
    /// User dragged the floating player past 50% outside the window and dismissed it.
    Dismissed(FullMsgId),
    /// User double-clicked the floating player to jump to the original round video message.
    JumpToMessage(FullMsgId),
}

/// Floating circular round-video player (`Media::Player::Float` + `FloatController`).
pub struct RoundVideoFloat {
    controller: FloatController,
    reader: Option<ClipReader>,
    texture: Option<TextureHandle>,
    paused: bool,
    active_item: Option<FullMsgId>,
    clock_start_ms: Option<i64>,
    paused_at_ms: i64,
    drag_origin_pos: Point,
    drag_accum: Vec2,
}

impl RoundVideoFloat {
    /// Creates a new floating round-video mini-player controller for `parent_size`.
    #[must_use]
    pub fn new(parent_width: i32, parent_height: i32) -> Self {
        let controller = FloatController::new(
            FloatSize::new(parent_width.max(1), parent_height.max(1)),
            FloatSize::new(FLOAT_WIDGET_OUTER_SIZE, FLOAT_WIDGET_OUTER_SIZE),
        );
        Self {
            controller,
            reader: None,
            texture: None,
            paused: false,
            active_item: None,
            clock_start_ms: None,
            paused_at_ms: 0,
            drag_origin_pos: Point::default(),
            drag_accum: Vec2::ZERO,
        }
    }

    /// Updates available layout column sections (`FloatController::set_layout`).
    pub fn set_layout(
        &mut self,
        parent_width: i32,
        parent_height: i32,
        sections: Vec<SectionArea>,
    ) {
        self.controller.set_layout(
            FloatSize::new(parent_width.max(1), parent_height.max(1)),
            sections,
        );
    }

    /// Returns a shared reference to the underlying [`FloatController`].
    #[must_use]
    pub const fn controller(&self) -> &FloatController {
        &self.controller
    }

    /// Returns the preferred column/corner persisted across drags.
    #[must_use]
    pub const fn preferred_anchor(&self) -> (FloatColumn, FloatCorner) {
        (
            self.controller.preferred_column(),
            self.controller.preferred_corner(),
        )
    }

    /// Attaches a round video message from encoded bytes (e.g. MP4) and starts circular playback.
    pub fn open_round_video_bytes(
        &mut self,
        item: FullMsgId,
        bytes: Vec<u8>,
        item_visible_in_history: bool,
    ) -> anyhow::Result<()> {
        let mut reader = ClipReader::from_bytes(bytes, ClipMode::Normal)
            .map_err(|e| anyhow::anyhow!("open round video clip failed: {e}"))?;
        let d = MEDIA_PLAYER_FLOAT_SIZE as u32;
        reader.set_resize(Some(VideoSize::new(d, d)));
        reader.set_round_circle(true);
        self.reader = Some(reader);
        self.texture = None;
        self.active_item = Some(item);
        self.paused = false;
        self.clock_start_ms = None;
        self.paused_at_ms = 0;
        self.controller.set_current_round_video(Some(item));
        self.controller
            .update_visibility(true, item_visible_in_history);
        Ok(())
    }

    /// Updates whether the original chat bubble is currently visible in the history viewport.
    pub fn set_history_item_visible(&mut self, visible_in_history: bool) {
        let ready = self.reader.is_some();
        self.controller.update_visibility(ready, visible_in_history);
    }

    /// Detaches and stops the active round video float.
    pub fn detach(&mut self) {
        self.reader = None;
        self.texture = None;
        self.active_item = None;
        self.controller.detach();
    }

    /// Toggles pause/resume on the active round video.
    pub fn toggle_pause(&mut self, now_ms: i64) {
        if self.paused {
            self.paused = false;
            self.clock_start_ms = Some(now_ms - self.paused_at_ms);
        } else {
            self.paused = true;
            if let Some(start) = self.clock_start_ms {
                self.paused_at_ms = (now_ms - start).max(0);
            }
        }
    }

    /// Returns `true` when the floating round video is currently visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.controller.current().is_some_and(|c| c.visible)
    }

    /// Finishes an active drag programmatically (exposed for tests).
    pub fn finish_drag(&mut self) -> Option<DragOutcome> {
        let outcome = self.controller.finish_drag()?;
        if outcome.closed {
            self.reader = None;
            self.texture = None;
            self.active_item = None;
        }
        Some(outcome)
    }

    /// Advances decoding and renders the circular floating mini-player inside `parent_rect`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        parent_rect: Rect,
        now_ms: i64,
    ) -> Option<RoundFloatEvent> {
        let cur = self.controller.current()?.clone();
        if !cur.visible || cur.opacity <= 0.01 {
            return None;
        }

        // Advance frame via `ClipReader`.
        if let Some(reader) = &mut self.reader {
            let start = *self.clock_start_ms.get_or_insert(now_ms);
            let target_ms = if self.paused {
                self.paused_at_ms
            } else {
                (now_ms - start).max(0)
            };
            if let Ok(frame) = reader.read_frames_till(target_ms)
                && let FrameData::Rgba(bytes) = &frame.data
            {
                let img = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    bytes,
                );
                if let Some(tex) = &mut self.texture {
                    tex.set(img, TextureOptions::LINEAR);
                } else {
                    self.texture =
                        Some(ctx.load_texture("tg_round_video_float", img, TextureOptions::LINEAR));
                }
            }
            if !self.paused {
                ctx.request_repaint_after(std::time::Duration::from_millis(20));
            }
        }

        let duration_ms = self
            .reader
            .as_ref()
            .map_or(0, ClipReader::duration_ms)
            .max(1);
        let progress = self.reader.as_ref().map_or(0.0, |r| {
            (r.frame_real_time_ms() as f32 / duration_ms as f32).clamp(0.0, 1.0)
        });

        let opacity = cur.opacity as f32;
        let area_pos = Pos2::new(
            parent_rect.min.x + cur.position.x as f32,
            parent_rect.min.y + cur.position.y as f32,
        );
        let mut event = None;

        Area::new(Id::unique("tg_media_round_float"))
            .order(Order::Foreground)
            .fixed_pos(area_pos)
            .show(ctx, |ui| {
                let outer = Vec2::splat(FLOAT_WIDGET_OUTER_SIZE as f32);
                let (rect, response) = ui.allocate_exact_size(outer, Sense::click_and_drag());
                let inner = rect.shrink(MEDIA_PLAYER_FLOAT_MARGIN);
                let center = inner.center();
                let radius = inner.width() * 0.5;
                let painter = ui.painter();

                // Soft circular drop shadow (`Float::prepareShadow`).
                painter.circle_filled(
                    center + Vec2::new(0.0, 2.0),
                    radius + 3.0,
                    with_opacity(Color32::from_rgba_unmultiplied(0, 0, 0, 72), opacity),
                );
                painter.circle_filled(center, radius, with_opacity(Color32::BLACK, opacity));

                // Circular masked video texture.
                if let Some(tex) = &self.texture {
                    painter.image(
                        tex.id(),
                        inner,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        with_opacity(Color32::WHITE, opacity),
                    );
                }

                let colors = MediaColors::from_ctx(ctx);
                let ring_fg = colors.color_or(
                    "historyVideoMessageProgressFg",
                    Color32::from_rgba_unmultiplied(255, 255, 255, 215),
                );
                // Radial playback progress arc (`historyVideoMessageProgressFg`).
                paint_radial_progress_ring(
                    painter,
                    center,
                    radius - 3.0,
                    progress,
                    with_opacity(ring_fg, opacity),
                );

                // Paused badge in center (`st::pipPlayIconOver`).
                if self.paused {
                    painter.circle_filled(
                        center,
                        20.0,
                        with_opacity(Color32::from_rgba_unmultiplied(0, 0, 0, 130), opacity),
                    );
                    paint_centered_style_icon_alpha(
                        ctx,
                        painter,
                        &st::pip_play_icon_over,
                        Rect::from_center_size(center, Vec2::splat(32.0)),
                        &colors,
                        opacity,
                    );
                }

                // Interactions: double-click jumps to message, single click toggles pause, drag snaps/dismisses.
                if response.double_clicked()
                    && let Some(item) = self.active_item
                {
                    event = Some(RoundFloatEvent::JumpToMessage(item));
                } else if response.clicked() {
                    self.toggle_pause(now_ms);
                }

                if response.drag_started() {
                    self.drag_origin_pos = cur.position;
                    self.drag_accum = Vec2::ZERO;
                }
                if response.dragged() {
                    self.drag_accum += response.drag_delta();
                    let next_pos = Point::new(
                        self.drag_origin_pos.x + self.drag_accum.x.round() as i32,
                        self.drag_origin_pos.y + self.drag_accum.y.round() as i32,
                    );
                    self.controller.drag_to(next_pos);
                }
                if response.drag_stopped()
                    && let Some(outcome) = self.finish_drag()
                    && outcome.closed
                    && let Some(closed_id) = outcome.closed_item
                {
                    event = Some(RoundFloatEvent::Dismissed(closed_id));
                }
            });

        event
    }
}

/// Helper building a single-section layout covering `width × height` below an optional `top_inset`.
#[must_use]
pub fn single_section_layout(width: i32, height: i32) -> Vec<SectionArea> {
    single_section_layout_with_top_inset(width, height, 0)
}

/// Helper building a single-section layout covering `width × height` offset below `top_inset` (e.g. top audio player bar).
#[must_use]
pub fn single_section_layout_with_top_inset(
    width: i32,
    height: i32,
    top_inset: i32,
) -> Vec<SectionArea> {
    let top = top_inset.max(0);
    vec![SectionArea {
        column: FloatColumn::Second,
        available_rect: FloatRect::new(0, top, width.max(1), (height - top).max(1)),
    }]
}

fn paint_radial_progress_ring(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    progress: f32,
    color: Color32,
) {
    let clamped = progress.clamp(0.0, 1.0);
    if clamped <= 0.002 {
        return;
    }
    let steps = ((clamped * 48.0).ceil() as usize).max(4);
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let ang = -FRAC_PI_2 + t * clamped * TAU;
            Pos2::new(center.x + radius * ang.cos(), center.y + radius * ang.sin())
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(2.5, color)));
}
