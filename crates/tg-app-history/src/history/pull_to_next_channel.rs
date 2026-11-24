//! Pull-to-next-unread-channel overscroll indicator & footer hint for broadcast channels.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/history_view_pull_to_next_channel.h`
//! - `Telegram/SourceFiles/history/history_view_pull_to_next_channel.cpp`
//! - `Telegram/SourceFiles/chat_helpers/chat_helpers.style` (`historyPullNext*`)

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_app_core::stand_in::types::PeerId;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;

use crate::history::theme::HistoryColors;

/// Exact tdesktop animation durations (`history_view_pull_to_next_channel.cpp:50-55`).
const RELEASE_SHOW_DURATION_SECS: f64 = 0.250;
const RELEASE_HIDE_DURATION_SECS: f64 = 0.250;
const PANEL_DURATION_SECS: f64 = 0.320;
const BOUNCE_DURATION_SECS: f64 = 0.400;

/// Footer bar height for the broadcast channel pull-to-next-channel hint strip.
pub const PULL_NEXT_FOOTER_HEIGHT: f32 = 46.0;

/// Summary of the next unread broadcast channel candidate (`FindNextUnreadChannel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextUnreadChannelHint {
    /// Destination broadcast channel peer ID.
    pub peer_id: PeerId,
    /// Display title of the next unread broadcast channel.
    pub title: String,
    /// Unread message count shown inside the floating unread badge.
    pub unread_count: u32,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Exact tdesktop cubic bounce curve (`BounceProgress` in `history_view_pull_to_next_channel.cpp:219-233`).
#[must_use]
pub fn bounce_progress(t: f64) -> f64 {
    let ease = |x: f64| -> f64 {
        if x < 0.5 {
            4.0 * x * x * x
        } else {
            let u = -2.0 * x + 2.0;
            1.0 - u * u * u / 2.0
        }
    };
    let t = t.clamp(0.0, 1.0);
    if t < 0.45 {
        ease(t / 0.45)
    } else if t < 0.75 {
        1.0 - ease((t - 0.45) / 0.3) * 1.5
    } else {
        -0.5 + ease((t - 0.75) / 0.25) * 0.5
    }
}

/// Exact tdesktop `easeOutQuint` curve (`anim::easeOutQuint`).
#[must_use]
fn ease_out_quint(t: f64) -> f64 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u * u * u
}

/// Exact tdesktop `sineInOut` curve (`anim::sineInOut`).
#[must_use]
fn sine_in_out(t: f64) -> f64 {
    0.5 * (1.0 - (std::f64::consts::PI * t.clamp(0.0, 1.0)).cos())
}

/// Pull threshold matching `PullThreshold()` (`st::historyPullNextThreshold * 2`,
/// `history_view_pull_to_next_channel.cpp:57-63`).
#[must_use]
pub fn pull_threshold_px() -> f32 {
    (st::history_pull_next_threshold.0 as f32 * 2.0).max(96.0)
}

/// Stateful pull-to-next-unread-channel controller (`HistoryView::PullToNextChannel`).
#[derive(Debug, Default)]
pub struct PullToNextChannelWidget {
    /// Current overscroll pull distance in logical pixels (`0.0` when idle).
    pull_px: f32,
    /// True once `pull_px >= pull_threshold_px()`.
    ready: bool,
    /// Animation progress `0.0..=1.0` for transitioning from arrow capsule to ready avatar pill.
    release_progress: f64,
    release_anim_start: Option<(f64, f64, f64)>,
    /// Bounce timer start timestamp when `ready` first flips true.
    bounce_start: Option<f64>,
    /// Hint panel visibility animation `0.0..=1.0`.
    panel_progress: f64,
    panel_anim_start: Option<(f64, f64, f64)>,
    /// Last time overscroll wheel/drag input was received (for smooth spring decay on release).
    last_pull_time: Option<f64>,
}

impl PullToNextChannelWidget {
    /// Creates an idle pull-to-next-channel controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets pull state immediately (e.g. when switching channels or scrolling away from bottom).
    pub fn reset(&mut self) {
        self.pull_px = 0.0;
        self.ready = false;
        self.release_progress = 0.0;
        self.release_anim_start = None;
        self.bounce_start = None;
        self.panel_progress = 0.0;
        self.panel_anim_start = None;
        self.last_pull_time = None;
    }

    /// Sets a deterministic overscroll distance (used by tests & visual snapshots).
    pub fn set_pull_preview(&mut self, pull_px: f32, now_secs: f64) {
        self.pull_px = pull_px.max(0.0);
        self.last_pull_time = Some(now_secs);
        let ready = self.pull_px >= pull_threshold_px();
        self.ready = ready;
        self.release_progress = if ready { 1.0 } else { 0.0 };
        self.release_anim_start = None;
        self.panel_progress = if pull_px > 0.0 { 1.0 } else { 0.0 };
        self.panel_anim_start = None;
        self.bounce_start = None;
    }

    /// Current pull offset in logical pixels.
    #[must_use]
    pub const fn pull_px(&self) -> f32 {
        self.pull_px
    }

    /// Returns `true` if the user has pulled past the release threshold.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.ready
    }

    /// Feeds downward overscroll wheel delta (`delta_y < 0.0` in egui means scrolling down past
    /// bottom) while the broadcast channel is pinned at bottom.
    pub fn feed_bottom_overscroll(
        &mut self,
        downward_overscroll_px: f32,
        now_secs: f64,
        reduced_motion: bool,
    ) {
        if downward_overscroll_px <= 0.0 {
            return;
        }
        let max_pull = (st::history_pull_next_expand.0 as f32).max(120.0) * 1.35;
        self.pull_px = (self.pull_px + downward_overscroll_px * 0.65).min(max_pull);
        self.last_pull_time = Some(now_secs);
        self.sync_ready(now_secs, reduced_motion);
    }

    fn sync_ready(&mut self, now_secs: f64, reduced_motion: bool) {
        let next_ready = self.pull_px >= pull_threshold_px();
        if self.ready != next_ready {
            self.ready = next_ready;
            let to = if next_ready { 1.0 } else { 0.0 };
            if reduced_motion {
                self.release_progress = to;
                self.release_anim_start = None;
                self.bounce_start = None;
            } else {
                self.release_anim_start = Some((now_secs, self.release_progress, to));
                if next_ready {
                    self.bounce_start = Some(now_secs);
                }
            }
        }
        let target_panel = if self.pull_px > 2.0 { 1.0 } else { 0.0 };
        let current_target = self
            .panel_anim_start
            .map_or(self.panel_progress, |(_, _, to)| to);
        if (current_target - target_panel).abs() > 0.01 {
            if reduced_motion {
                self.panel_progress = target_panel;
                self.panel_anim_start = None;
            } else {
                self.panel_anim_start = Some((now_secs, self.panel_progress, target_panel));
            }
        }
    }

    fn step_animations(&mut self, now_secs: f64, reduced_motion: bool) -> (f64, f64, f64, bool) {
        let mut animating = false;

        if let Some((start, from, to)) = self.release_anim_start {
            let dur = if to > from {
                RELEASE_SHOW_DURATION_SECS
            } else {
                RELEASE_HIDE_DURATION_SECS
            };
            let t = ((now_secs - start) / dur).clamp(0.0, 1.0);
            let eased = if to > from {
                ease_out_quint(t)
            } else {
                sine_in_out(t)
            };
            self.release_progress = from + (to - from) * eased;
            if t >= 1.0 {
                self.release_progress = to;
                self.release_anim_start = None;
            } else {
                animating = true;
            }
        }

        if let Some((start, from, to)) = self.panel_anim_start {
            let t = ((now_secs - start) / PANEL_DURATION_SECS).clamp(0.0, 1.0);
            self.panel_progress = from + (to - from) * ease_out_quint(t);
            if t >= 1.0 {
                self.panel_progress = to;
                self.panel_anim_start = None;
            } else {
                animating = true;
            }
        }

        let bounce_offset = if reduced_motion {
            0.0
        } else if let Some(start) = self.bounce_start {
            let t = ((now_secs - start) / BOUNCE_DURATION_SECS).clamp(0.0, 1.0);
            if t >= 1.0 {
                self.bounce_start = None;
                0.0
            } else {
                animating = true;
                -bounce_progress(t) * f64::from(st::history_pull_next_bounce.0.max(4))
            }
        } else {
            0.0
        };

        (
            self.release_progress,
            self.panel_progress,
            bounce_offset,
            animating,
        )
    }

    /// Paints the broadcast-channel Pull-to-Next-Unread-Channel indicator over the bottom of
    /// `list_rect` AND the interactive footer bar inside `footer_rect`.
    ///
    /// Returns `Some(next_peer_id)` when the user completes a pull-and-release gesture OR clicks
    /// the footer button to jump to the next unread channel.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        footer_rect: Rect,
        at_bottom: bool,
        next_channel: Option<&NextUnreadChannelHint>,
        colors: &HistoryColors,
        reduced_motion: bool,
    ) -> Option<PeerId> {
        let now_secs = ui.ctx().input(|i| i.time);
        let mut jump_to = None;

        if !at_bottom {
            self.reset();
            return None;
        }

        // Capture wheel overscroll while hovered over the channel history at bottom.
        let pointer_in_list = ui
            .ctx()
            .input(|i| i.pointer.hover_pos().is_some_and(|p| list_rect.contains(p)));
        if pointer_in_list {
            let scroll_y = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll_y < -0.5 {
                self.feed_bottom_overscroll(-scroll_y, now_secs, reduced_motion);
            } else if scroll_y > 0.5 && self.pull_px > 0.0 {
                self.pull_px = (self.pull_px - scroll_y).max(0.0);
                self.sync_ready(now_secs, reduced_motion);
            }
        }

        // Release detection: once wheel/pull stops for 220ms, commit jump if ready, otherwise decay.
        if self.pull_px > 0.0
            && let Some(last) = self.last_pull_time
            && now_secs - last > 0.22
        {
            if self.ready
                && let Some(next) = next_channel
            {
                jump_to = Some(next.peer_id);
                self.reset();
                return jump_to;
            }
            if reduced_motion {
                self.pull_px = 0.0;
            } else {
                self.pull_px = (self.pull_px * 0.72 - 2.0).max(0.0);
                ui.ctx().request_repaint();
            }
            self.sync_ready(now_secs, reduced_motion);
        }

        let (release, _panel, bounce_offset, animating) =
            self.step_animations(now_secs, reduced_motion);
        if animating {
            ui.ctx().request_repaint();
        }

        // 1. Floating Pull Indicator capsule / avatar + title pill above the bottom edge of `list_rect`.
        if self.pull_px > 1.0 {
            self.paint_indicator(
                ui,
                list_rect,
                next_channel,
                release as f32,
                bounce_offset as f32,
                colors,
            );
        }

        // 2. Interactive bottom footer strip inside `footer_rect` (accessible via click + keyboard
        //    and displays the exact tdesktop pull/release hint strings).
        if footer_rect.is_positive()
            && self.paint_footer_hint(
                ui,
                footer_rect,
                next_channel,
                release as f32,
                colors,
                reduced_motion,
            )
            && let Some(next) = next_channel
        {
            jump_to = Some(next.peer_id);
            self.reset();
        }

        jump_to
    }

    fn paint_indicator(
        &self,
        ui: &mut Ui,
        list_rect: Rect,
        next_channel: Option<&NextUnreadChannelHint>,
        release: f32,
        bounce_offset: f32,
        colors: &HistoryColors,
    ) {
        let card = (st::history_pull_next_expand.0 as f32).max(108.0);
        let skip = (st::history_pull_next_skip.0 as f32).max(8.0);
        let padding = (st::history_pull_next_padding.0 as f32).max(4.0);
        let avatar_dia = (st::history_pull_next_avatar.0 as f32).max(40.0);
        let arrow_size = (st::history_pull_next_arrow_size.0 as f32).max(24.0);
        let arrow_stroke = (st::history_pull_next_arrow_stroke.0 as f32).max(2.0);
        let name_radius = (st::history_pull_next_name_radius.0 as f32).max(15.0);
        let rise = (st::history_pull_next_rise.0 as f32).max(35.0);

        let offset = self.pull_px;
        let progress = (offset / card).clamp(0.0, 1.0);
        let alpha = progress;
        let cx = list_rect.center().x;
        let bottom = list_rect.bottom();
        let bg = scale_alpha(colors.service_bg, alpha);
        let fg = scale_alpha(colors.service_fg, alpha);

        let circle_r = avatar_dia * 0.5;
        let width_r = (circle_r * (offset - skip) / (card - skip).max(1.0)).clamp(0.0, circle_r);
        let width_r2 = (circle_r * progress)
            .min(offset * 0.5 - skip * progress)
            .max(0.0);
        let inset = (st::history_pull_next_inset.0 as f32).max(16.0);
        let size = (width_r2 * 2.0 - inset).max(0.0) * (1.0 - release) + avatar_dia * release;

        // Stretching vertical capsule + upward pull arrow while `release < 1.0`.
        if release < 0.99 && width_r > 1.0 {
            let cap_top = bottom - offset;
            let cap_bot = bottom + (-skip * (1.0 - release) + (-offset + avatar_dia) * release);
            let mut rect = Rect::from_min_max(
                Pos2::new(cx - width_r, cap_top),
                Pos2::new(cx + width_r, cap_bot),
            );
            if release > 0.0 {
                rect = rect.shrink(inset * release * 0.5);
            }
            if rect.is_positive() {
                let cr = CornerRadius::same((rect.width() * 0.5).min(255.0).round() as u8);
                ui.painter().rect_filled(
                    rect,
                    cr,
                    scale_alpha(colors.service_bg, alpha * (1.0 - release)),
                );

                let arrow_cy =
                    bottom + (-offset + arrow_size + skip * (1.0 - progress) - rise * release);
                paint_up_arrow(
                    ui.painter(),
                    Pos2::new(cx, arrow_cy),
                    arrow_size * progress * 0.45,
                    arrow_stroke * progress.max(0.5),
                    scale_alpha(colors.service_fg, alpha * (1.0 - release)),
                );
            }
        }

        // Next channel title pill (or "You have no unread channels") when `release > 0`.
        let label = next_channel
            .map(|c| c.title.clone())
            .unwrap_or_else(tr::lng_pull_no_unread_channels);
        if release > 0.01 && !label.is_empty() {
            let avail = (list_rect.width() - 4.0 * skip - 24.0).max(80.0);
            let galley = ui.painter().layout(
                label,
                FontId::new(13.0, family_semibold()),
                scale_alpha(colors.service_fg, alpha * release),
                avail,
            );
            let name_w = galley.size().x;
            let name_h = galley.size().y;
            let name_top_offset = (st::history_pull_next_name_top.0 as f32).max(20.0);
            let y = bottom + (name_top_offset * (1.0 - release) - rise * release) + bounce_offset;
            let pill = Rect::from_min_size(
                Pos2::new(cx - name_w * 0.5 - skip, y - padding),
                Vec2::new(name_w + 2.0 * skip, name_h + padding * 2.0),
            );
            ui.painter().rect_filled(
                pill,
                CornerRadius::same(name_radius.min(255.0).round() as u8),
                scale_alpha(colors.service_bg, alpha * release),
            );
            ui.painter().galley(
                Pos2::new(cx - name_w * 0.5, y),
                galley,
                scale_alpha(colors.service_fg, alpha * release),
            );
        }

        // Channel avatar + unread badge (or checkmark circle when no unread channels remain).
        if size >= 4.0 {
            let top = bottom
                + ((-skip - skip * progress - size) * (1.0 - release)
                    + (-offset.min(card) + padding) * release)
                + bounce_offset;
            let av_rect = Rect::from_min_size(Pos2::new(cx - size * 0.5, top), Vec2::splat(size));
            if let Some(next) = next_channel {
                let ui_colors = colors.ui_colors();
                // Subtle surface halo ring so the floating avatar separates cleanly over message text.
                ui.painter().circle_stroke(
                    av_rect.center(),
                    size * 0.5 + 1.5,
                    Stroke::new(2.5, scale_alpha(colors.bar_bg, alpha)),
                );
                EmptyUserpic::for_color_index(next.color_index, &next.title, &ui_colors)
                    .paint_circle(ui.painter(), av_rect);
                if release > 0.05 && next.unread_count > 0 {
                    let badge_h = (st::history_pull_next_badge.0 as f32).max(20.0) * release;
                    let text = if next.unread_count > 999 {
                        "999+".to_owned()
                    } else {
                        next.unread_count.to_string()
                    };
                    let badge_galley = ui.painter().layout_no_wrap(
                        text,
                        FontId::new(11.5 * release.max(0.7), family_semibold()),
                        Color32::WHITE,
                    );
                    let badge_w = (badge_galley.size().x + 10.0).max(badge_h);
                    let badge_rect = Rect::from_min_size(
                        Pos2::new(
                            av_rect.right() - badge_w + badge_h * 0.3,
                            av_rect.top() - badge_h * 0.25,
                        ),
                        Vec2::new(badge_w, badge_h),
                    );
                    ui.painter().rect_filled(
                        badge_rect,
                        CornerRadius::same((badge_h * 0.5).round() as u8),
                        colors.accent,
                    );
                    ui.painter().galley(
                        Pos2::new(
                            badge_rect.center().x - badge_galley.size().x * 0.5,
                            badge_rect.center().y - badge_galley.size().y * 0.5,
                        ),
                        badge_galley,
                        Color32::WHITE,
                    );
                }
            } else {
                ui.painter().circle_filled(av_rect.center(), size * 0.5, bg);
                paint_check_mark(ui.painter(), av_rect.center(), size * 0.26, fg);
            }
        }
    }

    fn paint_footer_hint(
        &self,
        ui: &mut Ui,
        rect: Rect,
        next_channel: Option<&NextUnreadChannelHint>,
        release: f32,
        colors: &HistoryColors,
        reduced_motion: bool,
    ) -> bool {
        let id = Id::unique("tg_hist_pull_next_channel_footer");
        let sense = if next_channel.is_some() {
            Sense::click()
        } else {
            Sense::hover()
        };
        let resp = ui.interact(rect, id, sense);

        let bg = if next_channel.is_some() && resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            colors.bar_bg_over
        } else {
            colors.bar_bg
        };
        ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.top()),
                Pos2::new(rect.right(), rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        let Some(next) = next_channel else {
            let empty_text = tr::lng_pull_no_unread_channels();
            resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, empty_text.clone()));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                empty_text,
                FontId::new(13.0, family_regular()),
                colors.sub_text_fg,
            );
            return false;
        };

        let a11y = format!(
            "{}: {} ({} unread)",
            tr::lng_pull_next_channel(),
            next.title,
            next.unread_count
        );
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, a11y.clone()));

        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(2.0),
                CornerRadius::same(6),
                Stroke::new(2.0, colors.accent),
                StrokeKind::Inside,
            );
        }

        let slide = if reduced_motion {
            0.0
        } else {
            (st::history_pull_next_hint_slide.0 as f32).max(10.0)
        };
        // Compact badge on the right when space permits (keep footer strictly single-line at 390px).
        let compact = rect.width() < 460.0;
        let show_arrow = release <= 0.5;
        let hint_text = if release > 0.5 {
            if compact {
                tr::lng_release_next_channel()
            } else {
                format!("{} · {}", tr::lng_release_next_channel(), next.title)
            }
        } else if compact {
            next.title.clone()
        } else {
            format!("{} · {}", tr::lng_pull_next_channel(), next.title)
        };

        let badge_text = if next.unread_count > 999 {
            "999+".to_owned()
        } else {
            next.unread_count.to_string()
        };
        let badge_galley = ui.painter().layout_no_wrap(
            badge_text,
            FontId::new(11.5, family_semibold()),
            Color32::WHITE,
        );
        let badge_w = (badge_galley.size().x + 12.0).max(22.0);
        let badge_h = 20.0;
        let arrow_slot = if show_arrow { 16.0 } else { 0.0 };
        let max_text_w = (rect.width() - badge_w - arrow_slot - 44.0).max(100.0);

        let text_col = if release > 0.5 {
            colors.active_text_fg
        } else {
            colors.sub_text_fg
        };
        let mut job = egui::text::LayoutJob::single_section(
            hint_text,
            egui::TextFormat {
                font_id: FontId::new(13.0, family_regular()),
                color: text_col,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width: max_text_w,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        let total_w = arrow_slot + galley.size().x + 10.0 + badge_w;
        let start_x = (rect.center().x - total_w * 0.5).max(rect.left() + 12.0);
        let shift_y = if release > 0.5 {
            slide * (1.0 - release)
        } else {
            -slide * release
        };
        if show_arrow {
            tg_app_core::ui_icons::paint_arrow(
                ui.painter(),
                Pos2::new(start_x + 5.0, rect.center().y + shift_y),
                11.0,
                tg_app_core::ui_icons::ArrowDir::Up,
                text_col,
            );
        }
        ui.painter().galley(
            Pos2::new(
                start_x + arrow_slot,
                rect.center().y - galley.size().y * 0.5 + shift_y,
            ),
            galley,
            text_col,
        );

        let badge_rect = Rect::from_min_size(
            Pos2::new(
                (start_x + total_w - badge_w).min(rect.right() - badge_w - 12.0),
                rect.center().y - badge_h * 0.5,
            ),
            Vec2::new(badge_w, badge_h),
        );
        ui.painter()
            .rect_filled(badge_rect, CornerRadius::same(10), colors.accent);
        ui.painter().galley(
            Pos2::new(
                badge_rect.center().x - badge_galley.size().x * 0.5,
                badge_rect.center().y - badge_galley.size().y * 0.5,
            ),
            badge_galley,
            Color32::WHITE,
        );

        resp.clicked()
    }
}

fn paint_up_arrow(painter: &egui::Painter, center: Pos2, radius: f32, stroke_w: f32, fg: Color32) {
    if radius <= 1.0 {
        return;
    }
    let stroke = Stroke::new(stroke_w, fg);
    painter.line_segment(
        [
            Pos2::new(center.x, center.y - radius),
            Pos2::new(center.x, center.y + radius),
        ],
        stroke,
    );
    painter.add(Shape::line(
        vec![
            Pos2::new(center.x - radius * 0.72, center.y - radius * 0.22),
            Pos2::new(center.x, center.y - radius),
            Pos2::new(center.x + radius * 0.72, center.y - radius * 0.22),
        ],
        stroke,
    ));
}

fn paint_check_mark(painter: &egui::Painter, center: Pos2, s: f32, fg: Color32) {
    if s <= 1.0 {
        return;
    }
    painter.add(Shape::line(
        vec![
            Pos2::new(center.x - s, center.y),
            Pos2::new(center.x - s / 3.0, center.y + s * 0.7),
            Pos2::new(center.x + s, center.y - s * 0.7),
        ],
        Stroke::new(2.0, fg),
    ));
}

fn scale_alpha(color: Color32, factor: f32) -> Color32 {
    let a = (f32::from(color.a()) * factor.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounce_progress_matches_tdesktop_endpoints_and_overshoot() {
        assert!((bounce_progress(0.0) - 0.0).abs() < 1e-6);
        assert!((bounce_progress(0.45) - 1.0).abs() < 1e-6);
        assert!((bounce_progress(0.75) - (-0.5)).abs() < 1e-6);
        assert!((bounce_progress(1.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn pull_reaches_ready_past_threshold() {
        let mut pull = PullToNextChannelWidget::new();
        assert!(!pull.is_ready());
        let thresh = pull_threshold_px();
        pull.feed_bottom_overscroll(thresh * 2.0, 1.0, false);
        assert!(pull.is_ready());
        pull.reset();
        assert!(!pull.is_ready());
        assert_eq!(pull.pull_px(), 0.0);
    }
}
