//! Round-video circular seek ring with drag-to-seek, rim shadow, and wrap-around ghost arc.
//!
//! Port origin: `Telegram/SourceFiles/history/view/media/history_view_video_message_seek.h`
//! & `.cpp`.

use std::f64::consts::{FRAC_PI_2, TAU};

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};
use tg_ui::tg_style::st;

/// Ring reveal animation duration (`kShowDuration = 220ms`).
pub const SEEK_SHOW_DURATION_SECS: f64 = 0.220;
/// Ring hide animation duration (`kHideDuration = 150ms`).
pub const SEEK_HIDE_DURATION_SECS: f64 = 0.150;
/// Grab dot enlarge animation duration (`kGrabDuration = 150ms`).
pub const SEEK_GRAB_DURATION_SECS: f64 = 0.150;
/// Ghost wrap-around crossfade duration (`kGhostDuration = 150ms`).
pub const SEEK_GHOST_DURATION_SECS: f64 = 0.150;
/// Background ring track opacity (`kTrackOpacity = 0.2`).
const TRACK_OPACITY: f64 = 0.2;
/// Progress delta threshold that triggers ghost fade across 12 o'clock (`kGhostJump = 0.9`).
const GHOST_JUMP_THRESHOLD: f64 = 0.9;
/// Base radial line width when collapsed (`st::radialLine`).
const NORMAL_LINE_PX: f64 = 3.0;
/// Progress arc opacity (`st::historyVideoMessageProgressOpacity`).
const PROGRESS_OPACITY: f64 = 0.85;

/// Simple time-driven scalar transition matching `Ui::Animations::Simple`.
#[derive(Debug, Clone, Copy, Default)]
struct SimpleTransition {
    from: f64,
    to: f64,
    start_secs: f64,
    duration_secs: f64,
    ease_out_back: bool,
    active: bool,
}

impl SimpleTransition {
    fn start(
        &mut self,
        now_secs: f64,
        from: f64,
        to: f64,
        duration_secs: f64,
        ease_out_back: bool,
    ) {
        self.from = from;
        self.to = to;
        self.start_secs = now_secs;
        self.duration_secs = duration_secs.max(0.001);
        self.ease_out_back = ease_out_back;
        self.active = true;
    }

    fn value(&mut self, now_secs: f64, fallback: f64) -> f64 {
        if !self.active {
            return fallback;
        }
        let raw = ((now_secs - self.start_secs) / self.duration_secs).clamp(0.0, 1.0);
        if raw >= 1.0 {
            self.active = false;
            return self.to;
        }
        let eased = if self.ease_out_back {
            // Standard easeOutBack (`s = 1.70158`).
            let s = 1.70158_f64;
            let t = raw - 1.0;
            1.0 + t * t * ((s + 1.0) * t + s)
        } else {
            raw
        };
        self.from + (self.to - self.from) * eased
    }

    const fn is_animating(&self) -> bool {
        self.active
    }
}

/// Per-round-video circular seek ring state (`HistoryView::VideoMessageSeek`).
#[derive(Debug, Clone, Default)]
pub struct VideoMessageSeek {
    shown_anim: SimpleTransition,
    grab_anim: SimpleTransition,
    ghost_anim: SimpleTransition,
    progress: f64,
    ghost_progress: f64,
    shown: bool,
    grabbed: bool,
}

impl VideoMessageSeek {
    /// Creates a fresh seek ring controller at progress `0.0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current seek progress in `0.0..=1.0`.
    #[must_use]
    pub const fn progress(&self) -> f64 {
        self.progress
    }

    /// True if the seek ring is currently grabbed/dragging.
    #[must_use]
    pub const fn is_grabbed(&self) -> bool {
        self.grabbed
    }

    /// Updates playback progress (non-drag).
    pub fn set_progress(&mut self, progress: f64) {
        self.progress = progress.clamp(0.0, 1.0);
    }

    /// Updates dragged seek progress, triggering a wrap-around ghost crossfade when crossing
    /// 12 o'clock (`|delta| > 0.9`, matching `VideoMessageSeek::setDraggedProgress`).
    pub fn set_dragged_progress(&mut self, progress: f64, now_secs: f64) {
        let clamped = progress.clamp(0.0, 1.0);
        if (self.progress - clamped).abs() <= f64::EPSILON {
            return;
        }
        if (clamped - self.progress).abs() > GHOST_JUMP_THRESHOLD {
            self.ghost_progress = self.progress;
            self.ghost_anim
                .start(now_secs, 1.0, 0.0, SEEK_GHOST_DURATION_SECS, false);
        }
        self.progress = clamped;
    }

    /// Sets grabbed/dragging state and triggers the dot enlargement transition.
    pub fn set_grabbed(&mut self, grabbed: bool, now_secs: f64) {
        if self.grabbed == grabbed {
            return;
        }
        let cur = self
            .grab_anim
            .value(now_secs, if self.grabbed { 1.0 } else { 0.0 });
        self.grabbed = grabbed;
        self.grab_anim.start(
            now_secs,
            cur,
            if grabbed { 1.0 } else { 0.0 },
            SEEK_GRAB_DURATION_SECS,
            false,
        );
    }

    /// Returns `true` if `point` lies inside the outer circular grab band of `rthumb`
    /// (`VideoMessageSeek::grabPoint`, band = `st::historyVideoMessageSeekGrabBand` = 40px).
    #[must_use]
    pub fn grab_point(rthumb: Rect, point: Pos2) -> bool {
        let radius = f64::from(rthumb.width().min(rthumb.height())) * 0.5;
        let band = f64::from(st::history_video_message_seek_grab_band.0 as f32).min(radius * 0.65);
        if radius <= 1.0 || radius <= band * 0.5 {
            return false;
        }
        let center = rthumb.center();
        let dx = f64::from(point.x - center.x);
        let dy = f64::from(point.y - center.y);
        let distance = (dx * dx + dy * dy).sqrt();
        distance >= (radius - band) && distance <= radius + 4.0
    }

    /// Computes clockwise seek progress in `[0.0, 1.0]` from 12 o'clock for a pointer `point`
    /// relative to `rthumb.center()`.
    #[must_use]
    pub fn progress_from_point(rthumb: Rect, point: Pos2) -> f64 {
        let center = rthumb.center();
        let dx = f64::from(point.x - center.x);
        let dy = f64::from(point.y - center.y);
        // 12 o'clock is (0, -1), increasing clockwise toward (1, 0) = 0.25, (0, 1) = 0.5, (-1, 0) = 0.75.
        let angle = dx.atan2(-dy);
        let normalized = if angle < 0.0 { angle + TAU } else { angle };
        (normalized / TAU).clamp(0.0, 1.0)
    }

    /// Paints the circular seek ring, outer rim vignette shadow, track, progress arc, ghost arc,
    /// and interactive grab dot (`VideoMessageSeek::paint`). Returns `true` if another frame
    /// repaint is needed for an in-flight transition.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &Painter,
        rthumb: Rect,
        shown: bool,
        seeking: bool,
        playback: f64,
        in_ttl_viewer: bool,
        now_secs: f64,
        reduced_motion: bool,
    ) -> bool {
        if self.shown != shown {
            let cur = self
                .shown_anim
                .value(now_secs, if self.shown { 1.0 } else { 0.0 });
            self.shown = shown;
            if reduced_motion {
                self.shown_anim.active = false;
            } else {
                self.shown_anim.start(
                    now_secs,
                    cur,
                    if shown { 1.0 } else { 0.0 },
                    if shown {
                        SEEK_SHOW_DURATION_SECS
                    } else {
                        SEEK_HIDE_DURATION_SECS
                    },
                    shown,
                );
            }
        }

        let amount = if reduced_motion {
            if shown { 1.0 } else { 0.0 }
        } else {
            self.shown_anim
                .value(now_secs, if shown { 1.0 } else { 0.0 })
        };
        let value = if seeking || (playback < 0.0 && amount > 0.0) {
            self.progress
        } else if playback < 0.0 {
            0.0
        } else {
            playback.clamp(0.0, 1.0)
        };

        if value <= 0.0 && amount <= 0.0 {
            return false;
        }

        let opacity = amount.clamp(0.0, 1.0);
        let center = rthumb.center();
        let outer_radius = rthumb.width().min(rthumb.height()) * 0.5;

        // 1. Soft inner rim vignette shadow (`paintShadow` / `kShadowInnerStop = 0.7`, `shadowOpacity = 0.16`),
        //    rendered as concentric inner bands strictly inside `outer_radius` so it never spills past the circle.
        if opacity > 0.0 {
            let base_alpha =
                f64::from(st::history_video_message_seek_shadow_opacity as f32) * opacity * 255.0;
            let bands = [
                (outer_radius - 3.5, 6.0_f32, 0.95_f64),
                (outer_radius - 9.5, 6.5_f32, 0.58_f64),
                (outer_radius - 16.0, 6.5_f32, 0.25_f64),
            ];
            for (r, w, weight) in bands {
                if r <= 4.0 {
                    continue;
                }
                let a = (base_alpha * weight).round().clamp(0.0, 255.0) as u8;
                if a == 0 {
                    continue;
                }
                let segs = 48usize;
                let mut pts = Vec::with_capacity(segs + 1);
                for i in 0..=segs {
                    let ang = (i as f64 / segs as f64) * TAU;
                    pts.push(Pos2::new(
                        center.x + r * ang.cos() as f32,
                        center.y + r * ang.sin() as f32,
                    ));
                }
                painter.add(Shape::line(
                    pts,
                    Stroke::new(w, Color32::from_rgba_unmultiplied(0, 0, 0, a)),
                ));
            }
        }

        // 2. Inset & stroke width interpolation (`normalInset = 1.5 * radialLine`, `seekInset = 18px`).
        let seek_line = f64::from(st::history_video_message_seek_line.0 as f32);
        let stroke_w = (NORMAL_LINE_PX + (seek_line - NORMAL_LINE_PX) * opacity) as f32;
        let normal_inset = 1.5 * NORMAL_LINE_PX;
        let seek_inset = f64::from(st::history_video_message_seek_inset.0 as f32);
        let step_inside = (normal_inset + (seek_inset - normal_inset) * amount).max(2.0) as f32;
        let arc_radius = (outer_radius - step_inside).max(8.0);

        // 3. Full circular track at `kTrackOpacity * opacity`.
        if opacity > 0.0 {
            let track_alpha = (TRACK_OPACITY * opacity * 255.0).round() as u8;
            painter.circle_stroke(
                center,
                arc_radius,
                Stroke::new(
                    stroke_w,
                    Color32::from_rgba_unmultiplied(255, 255, 255, track_alpha),
                ),
            );
        }

        // 4. Progress arc (+ wrap-around ghost crossfade arc).
        let paint_progress_arc = |prog: f64, alpha_scale: f64| {
            let sweep = if in_ttl_viewer {
                -(1.0 - prog.clamp(0.0, 1.0)) * TAU
            } else {
                prog.clamp(0.0, 1.0) * TAU
            };
            if sweep.abs() < 0.002 {
                return;
            }
            let alpha = (PROGRESS_OPACITY * alpha_scale.clamp(0.0, 1.0) * 255.0).round() as u8;
            paint_circular_arc(
                painter,
                center,
                arc_radius,
                -FRAC_PI_2,
                sweep,
                stroke_w,
                Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
            );
        };

        let ghost = if reduced_motion {
            0.0
        } else {
            self.ghost_anim.value(now_secs, 0.0)
        };
        if ghost > 0.0 {
            paint_progress_arc(value, 1.0 - ghost);
            paint_progress_arc(self.ghost_progress, ghost);
        } else {
            paint_progress_arc(value, 1.0);
        }

        // 5. Interactive grab thumb dot (`dotSizeMin = 6`, `dotSize = 16`, `dotSizeGrabbed = 22`).
        if opacity > 0.0 {
            let grabbed_val = if reduced_motion {
                if seeking || self.grabbed { 1.0 } else { 0.0 }
            } else {
                self.grab_anim
                    .value(now_secs, if seeking || self.grabbed { 1.0 } else { 0.0 })
            };
            let min_sz = f64::from(st::history_video_message_seek_dot_size_min.0 as f32);
            let full_sz = f64::from(st::history_video_message_seek_dot_size.0 as f32);
            let grab_sz = f64::from(st::history_video_message_seek_dot_size_grabbed.0 as f32);
            let dot_size =
                (min_sz + (full_sz - min_sz) * amount + (grab_sz - full_sz) * grabbed_val).max(2.0)
                    as f32;
            let angle = -FRAC_PI_2 + value * TAU;
            let dot_center = Pos2::new(
                center.x + arc_radius * angle.cos() as f32,
                center.y + arc_radius * angle.sin() as f32,
            );
            let dot_alpha = (opacity * 255.0).round() as u8;
            painter.circle_filled(
                dot_center,
                dot_size * 0.5,
                Color32::from_rgba_unmultiplied(255, 255, 255, dot_alpha),
            );
        }

        !reduced_motion
            && (self.shown_anim.is_animating()
                || self.grab_anim.is_animating()
                || self.ghost_anim.is_animating())
    }
}

fn paint_circular_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_rad: f64,
    sweep_rad: f64,
    stroke_width: f32,
    color: Color32,
) {
    let steps = ((sweep_rad.abs() * 24.0).ceil() as usize).clamp(8, 72);
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = start_rad + sweep_rad * (i as f64 / steps as f64);
        pts.push(Pos2::new(
            center.x + radius * a.cos() as f32,
            center.y + radius * a.sin() as f32,
        ));
    }
    painter.add(Shape::line(pts, Stroke::new(stroke_width, color)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_from_point_maps_clock_quadrants_accurately() {
        let rect = Rect::from_center_size(Pos2::new(100.0, 100.0), egui::Vec2::splat(200.0));
        // 12 o'clock -> 0.0
        assert!(VideoMessageSeek::progress_from_point(rect, Pos2::new(100.0, 20.0)) < 0.01);
        // 3 o'clock -> 0.25
        assert!(
            (VideoMessageSeek::progress_from_point(rect, Pos2::new(180.0, 100.0)) - 0.25).abs()
                < 0.01
        );
        // 6 o'clock -> 0.50
        assert!(
            (VideoMessageSeek::progress_from_point(rect, Pos2::new(100.0, 180.0)) - 0.50).abs()
                < 0.01
        );
        // 9 o'clock -> 0.75
        assert!(
            (VideoMessageSeek::progress_from_point(rect, Pos2::new(20.0, 100.0)) - 0.75).abs()
                < 0.01
        );
    }

    #[test]
    fn grab_point_matches_outer_band_only() {
        let rect = Rect::from_center_size(Pos2::new(120.0, 120.0), egui::Vec2::splat(220.0));
        // Center is NOT in the grab band (radius 110, band 40 -> ring is 70..=110)
        assert!(!VideoMessageSeek::grab_point(rect, Pos2::new(120.0, 120.0)));
        // Point at distance 90 from center IS in the grab band
        assert!(VideoMessageSeek::grab_point(rect, Pos2::new(210.0, 120.0)));
        // Far outside is NOT in the grab band
        assert!(!VideoMessageSeek::grab_point(rect, Pos2::new(260.0, 120.0)));
    }

    #[test]
    fn wrap_around_drag_triggers_ghost_animation() {
        let mut seek = VideoMessageSeek::new();
        seek.set_progress(0.96);
        seek.set_dragged_progress(0.02, 1.0);
        assert!((seek.progress() - 0.02).abs() < 1e-6);
        assert!(seek.ghost_anim.is_animating());
    }
}
