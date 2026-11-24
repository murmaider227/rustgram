//! 1:1 call window UI: background, userpic/video canvas, status timer, emoji fingerprint
//! verification row with tooltip, signal bars, animated control buttons, device menu, and
//! post-call rating card.
//!
//! Port of:
//! - `Telegram/SourceFiles/calls/calls_panel.cpp`
//! - `Telegram/SourceFiles/calls/calls_panel_background.cpp`
//! - `Telegram/SourceFiles/calls/calls_emoji_fingerprint.cpp`
//! - `Telegram/SourceFiles/calls/calls_signal_bars.cpp`
//! - `Telegram/SourceFiles/calls/calls_rate_call.cpp`
//! - `Telegram/SourceFiles/ui/controls/call_button.cpp`

use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};
use tg_calls::{CallState, CallType, RemoteAudioState, RemoteBatteryState, VideoState};
use tg_lang::tr;
use tg_ui::fonts::ensure_tdesktop_fonts;

use crate::calls::backend::{ActiveCallSnapshot, CallsBackend};
use crate::calls::format::{call_status_text, emoji_fingerprint_tooltip};
use crate::calls::screen_picker::ScreenSharePickerDialog;
use crate::calls::strings;
use crate::calls::theme::{
    CALL_BUTTON_BG_SIZE, CALL_BUTTON_HEIGHT, CALL_BUTTON_OUTER_RADIUS, CALL_BUTTON_WIDTH,
    CALL_PANEL_ANIM_DURATION_MS, CALL_WINDOW_HEIGHT, CALL_WINDOW_MIN_HEIGHT, CALL_WINDOW_MIN_WIDTH,
    CALL_WINDOW_WIDTH, CallPalette, lerp_color, paint_avatar, paint_camera_icon,
    paint_chevron_down_icon, paint_close_icon, paint_fingerprint_emoji_glyph, paint_mic_icon,
    paint_minimize_icon, paint_phone_icon, paint_pin_icon, paint_radial_backdrop,
    paint_screen_share_icon, paint_signal_bars, paint_star, with_alpha,
};

/// Which corner device popup menu is open inside the 1:1 call window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMenuKind {
    /// Speakers & Microphone selector attached to Mute button.
    Audio,
    /// Camera selector attached to Camera button.
    Camera,
}

/// Interactive state and animation tracks for the 1:1 call window (`Calls::Panel`).
#[derive(Debug)]
pub struct CallPanelState {
    /// Whether the 1:1 call window is currently visible (hiding keeps the green top bar active).
    pub visible: bool,
    /// Whether pinned on top of other panels.
    pub pinned_on_top: bool,
    /// Animated Answer -> Hangup transition (`0.0` = Answer green upright, `1.0` = Hangup red 135°).
    pub hangup_progress: f32,
    /// Animated Mute cross-line progress (`0.0` = unmuted, `1.0` = muted).
    pub mute_cross_progress: f32,
    /// Last frame timestamp (ms) used to advance smooth button animations.
    pub last_frame_ms: u64,
    /// Open audio/camera device menu, if any.
    pub device_menu: Option<DeviceMenuKind>,
    /// Whether the emoji fingerprint tooltip is explicitly forced open (e.g. for screenshot/click).
    pub fingerprint_tooltip_pinned: bool,
    /// Currently hovered star index (`1..=5`) in the post-call rating card.
    pub rating_hover: u8,
    /// Selected star rating (`0..=5`) in the post-call rating card.
    pub rating_selected: u8,
    /// Optional comment text shown when rating <= 3 stars (`calls_rate_call.cpp`).
    pub rating_comment: String,
    /// Animation progress (`0.0..=1.0`) for the post-call rating card entrance.
    pub rating_show_progress: f32,
    /// Modal dialog for picking a monitor or window to share during a 1:1 call.
    pub screen_picker: ScreenSharePickerDialog,
}

impl Default for CallPanelState {
    fn default() -> Self {
        Self {
            visible: true,
            pinned_on_top: false,
            hangup_progress: 0.0,
            mute_cross_progress: 0.0,
            last_frame_ms: 0,
            device_menu: None,
            fingerprint_tooltip_pinned: false,
            rating_hover: 0,
            rating_selected: 0,
            rating_comment: String::new(),
            rating_show_progress: 0.0,
            screen_picker: ScreenSharePickerDialog::default(),
        }
    }
}

impl CallPanelState {
    /// Resets transient panel animations when a fresh call starts.
    pub fn reset_for_call(&mut self, call: &ActiveCallSnapshot, now_ms: u64) {
        self.visible = true;
        self.device_menu = None;
        self.fingerprint_tooltip_pinned = false;
        self.rating_hover = 0;
        self.rating_selected = 0;
        self.rating_comment.clear();
        self.rating_show_progress = 0.0;
        self.last_frame_ms = now_ms;
        self.hangup_progress = if call.call_type == CallType::Outgoing
            && call.state != CallState::WaitingUserConfirmation
            && call.state != CallState::Busy
        {
            1.0
        } else {
            0.0
        };
        self.mute_cross_progress = if call.muted { 1.0 } else { 0.0 };
    }

    /// Advances smooth tdesktop button animations (`callPanelDuration = 150ms`).
    pub fn advance_animations(&mut self, call: &ActiveCallSnapshot, now_ms: u64) {
        let dt_ms = if self.last_frame_ms == 0 {
            16
        } else {
            now_ms.saturating_sub(self.last_frame_ms).min(100)
        };
        self.last_frame_ms = now_ms;
        let step = (dt_ms as f32) / (CALL_PANEL_ANIM_DURATION_MS as f32);

        let is_incoming_waiting = call.call_type == CallType::Incoming
            && matches!(call.state, CallState::Starting | CallState::WaitingIncoming);
        let hangup_target = if is_incoming_waiting
            || matches!(
                call.state,
                CallState::Busy | CallState::WaitingUserConfirmation
            ) {
            0.0
        } else {
            1.0
        };
        self.hangup_progress = move_towards(self.hangup_progress, hangup_target, step);

        let mute_target = if call.muted { 1.0 } else { 0.0 };
        self.mute_cross_progress = move_towards(self.mute_cross_progress, mute_target, step);

        if call.rating_requested {
            self.rating_show_progress =
                move_towards(self.rating_show_progress, 1.0, (dt_ms as f32) / 250.0);
        } else {
            self.rating_show_progress = 0.0;
        }
    }

    /// Renders the 1:1 call floating window in `ctx`.
    pub fn show_window(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        ring_peak: f32,
        now_ms: u64,
    ) {
        let Some(call) = backend.active_call().cloned() else {
            return;
        };
        if !self.visible {
            return;
        }
        self.advance_animations(&call, now_ms);

        let mut keep_open = self.visible;
        ensure_tdesktop_fonts(ctx);
        egui::Window::new(format!(
            "{} — {}",
            strings::CALL_WINDOW_PREFIX,
            call.peer.name
        ))
        .id(egui::Id::unique("rustgram_1to1_call_window"))
        .title_bar(false)
        .resizable(true)
        .default_size(Vec2::new(CALL_WINDOW_WIDTH, CALL_WINDOW_HEIGHT))
        .min_size(Vec2::new(CALL_WINDOW_MIN_WIDTH, CALL_WINDOW_MIN_HEIGHT))
        .open(&mut keep_open)
        .frame(egui::Frame::NONE.corner_radius(CornerRadius::same(12)))
        .show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            self.paint_panel_contents(ui, rect, &call, backend, palette, ring_peak, now_ms);
        });

        self.visible = keep_open;

        if let Some(source) =
            self.screen_picker
                .show(ctx, "call_screen_share_picker", palette, || {
                    backend.screen_share_sources()
                })
        {
            backend.start_call_screen_share_with_source(&source);
        }
    }

    /// Renders the full 1:1 call surface directly into `rect` (used by both the floating window
    /// and headless screenshot capture).
    #[allow(clippy::too_many_arguments)]
    pub fn paint_panel_contents(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        call: &ActiveCallSnapshot,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        ring_peak: f32,
        now_ms: u64,
    ) {
        let painter = ui.painter_at(rect);
        let showing_incoming_video =
            call.state == CallState::Established && call.remote_video_state == VideoState::Active;

        // 1. Background: radial gradient derived from peer accent (`calls_panel_background.cpp`)
        //    or incoming video canvas when remote video is active.
        let body_center = Pos2::new(rect.center().x, rect.top() + rect.height() * 0.36);
        if showing_incoming_video {
            paint_incoming_video_backdrop(&painter, rect, call, palette, now_ms);
        } else {
            let inner_tint = lerp_color(call.peer.avatar_color, palette.call_bg, 0.52);
            paint_radial_backdrop(&painter, rect, body_center, inner_tint, palette.call_bg);
        }

        // Subtle outer border for window crispness.
        painter.rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)),
            egui::StrokeKind::Inside,
        );

        // 2. Top chrome: Pin-on-top + minimize + close buttons on top-right.
        let top_btn_y = rect.top() + 20.0;
        let close_rect =
            Rect::from_center_size(Pos2::new(rect.right() - 24.0, top_btn_y), Vec2::splat(26.0));
        let min_rect =
            Rect::from_center_size(Pos2::new(rect.right() - 56.0, top_btn_y), Vec2::splat(26.0));
        let pin_rect =
            Rect::from_center_size(Pos2::new(rect.right() - 88.0, top_btn_y), Vec2::splat(26.0));

        if paint_top_icon_button(ui, &painter, close_rect, false, palette, |p, c, fg| {
            paint_close_icon(p, c, 14.0, fg);
        }) {
            if matches!(
                call.state,
                CallState::WaitingIncoming
                    | CallState::Starting
                    | CallState::Busy
                    | CallState::WaitingUserConfirmation
            ) {
                backend.hangup_call(now_ms);
            } else if call.rating_requested {
                backend.finish_call_rating();
            } else {
                // Closing window during an active call minimizes to the green top bar (`calls_panel.cpp:1752`).
                self.visible = false;
            }
        }
        if paint_top_icon_button(ui, &painter, min_rect, false, palette, |p, c, fg| {
            paint_minimize_icon(p, c, 14.0, fg);
        }) {
            self.visible = false;
        }
        if paint_top_icon_button(
            ui,
            &painter,
            pin_rect,
            self.pinned_on_top,
            palette,
            |p, c, fg| {
                paint_pin_icon(p, c, 15.0, fg, self.pinned_on_top);
            },
        ) {
            self.pinned_on_top = !self.pinned_on_top;
        }

        // 3. Emoji fingerprint + Signal bars pill (`calls_emoji_fingerprint.cpp:223-298`).
        if let Some(ref emojis) = call.emoji_fingerprint
            && !call.rating_requested
        {
            self.paint_fingerprint_and_signal_bars(ui, &painter, rect, call, emojis, palette);
        }

        // 4. Center body: userpic + peer name + call state / duration timer.
        if !showing_incoming_video {
            let compact = call.rating_requested || call.camera_state == VideoState::Active;
            let photo_radius = if compact { 48.0 } else { 76.0 };
            let photo_center = Pos2::new(
                rect.center().x,
                if compact {
                    rect.top() + 104.0
                } else {
                    rect.top() + rect.height() * 0.34
                },
            );

            // Soft speaking/ringing halo behind avatar.
            let halo_amp = if call.state == CallState::WaitingIncoming {
                ring_peak
            } else if call.state == CallState::Established {
                call.remote_audio_level
            } else {
                0.0
            };
            if halo_amp > 0.02 {
                let halo_r = photo_radius + 10.0 + halo_amp * 18.0;
                painter.circle_filled(
                    photo_center,
                    halo_r,
                    with_alpha(call.peer.avatar_color, 0.22 * halo_amp),
                );
                painter.circle_filled(
                    photo_center,
                    photo_radius + 5.0 + halo_amp * 8.0,
                    with_alpha(Color32::WHITE, 0.14 * halo_amp),
                );
            }

            paint_avatar(
                &painter,
                photo_center,
                photo_radius,
                call.peer.avatar_color,
                &call.peer.initials,
                photo_radius * 0.70,
            );

            // Remote muted badge on userpic (`calls_userpic.cpp`).
            if call.remote_audio_state == RemoteAudioState::Muted && !call.rating_requested {
                let badge = Pos2::new(
                    photo_center.x + photo_radius * 0.68,
                    photo_center.y + photo_radius * 0.68,
                );
                painter.circle_filled(badge, 18.0, palette.call_bg);
                painter.circle_filled(badge, 15.0, palette.call_hangup_bg);
                paint_mic_icon(&painter, badge, 15.0, Color32::WHITE, 1.0, Color32::WHITE);
            }

            let name_y = photo_center.y + photo_radius + 26.0;
            let title_text = if call.rating_requested {
                tr::lng_call_ended_title()
            } else {
                call.peer.name.clone()
            };
            painter.text(
                Pos2::new(rect.center().x, name_y),
                Align2::CENTER_CENTER,
                title_text,
                FontId::proportional(22.0),
                palette.call_name_fg,
            );

            let status_text = call_status_text(
                call.state,
                call.duration_ms,
                call.is_conference_invite,
                if call.rating_requested {
                    call.ended_duration_secs
                } else {
                    None
                },
            );
            painter.text(
                Pos2::new(rect.center().x, name_y + 28.0),
                Align2::CENTER_CENTER,
                status_text,
                FontId::proportional(14.5),
                if call.rating_requested {
                    palette.call_name_fg
                } else {
                    palette.call_status_fg
                },
            );
        }

        // 5. Outgoing video preview bubble (bottom-right PiP when camera/screen active).
        if !call.rating_requested
            && (call.camera_state == VideoState::Active || call.screen_share_active)
        {
            let pip_size = Vec2::new(160.0, 108.0);
            let pip_rect = Rect::from_min_size(
                Pos2::new(
                    rect.right() - pip_size.x - 16.0,
                    rect.bottom() - 114.0 - pip_size.y,
                ),
                pip_size,
            );
            painter.rect_filled(
                pip_rect,
                CornerRadius::same(10),
                Color32::from_rgb(0x22, 0x2B, 0x36),
            );
            painter.rect_stroke(
                pip_rect,
                CornerRadius::same(10),
                Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 65)),
                egui::StrokeKind::Inside,
            );
            let pip_label = if call.screen_share_active {
                strings::SHARING_SCREEN
            } else {
                strings::YOUR_CAMERA
            };
            painter.text(
                pip_rect.center(),
                Align2::CENTER_CENTER,
                pip_label,
                FontId::proportional(12.0),
                palette.call_name_fg,
            );
        }

        // 6. Remote mute / low battery status pill above controls (`calls_panel.cpp:904-1009`).
        if !call.rating_requested && call.state == CallState::Established {
            let notice = if call.remote_audio_state == RemoteAudioState::Muted {
                Some(tr::lng_call_microphone_off(&call.peer.short_name))
            } else if call.remote_battery_state == RemoteBatteryState::Low {
                Some(tr::lng_call_battery_level_low(&call.peer.short_name))
            } else {
                None
            };
            if let Some(msg) = notice {
                let pill_w = (msg.len() as f32 * 7.2 + 28.0).min(rect.width() - 32.0);
                let pill = Rect::from_center_size(
                    Pos2::new(rect.center().x, rect.bottom() - 108.0),
                    Vec2::new(pill_w, 28.0),
                );
                painter.rect_filled(
                    pill,
                    CornerRadius::same(14),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 128),
                );
                painter.text(
                    pill.center(),
                    Align2::CENTER_CENTER,
                    msg,
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        }

        // 7. Either Post-Call Rating Card (`calls_rate_call.cpp`) or Bottom Control Buttons.
        if call.rating_requested {
            self.paint_rate_call_card(ui, &painter, rect, call, backend, palette);
        } else {
            self.paint_bottom_controls(
                ui, &painter, rect, call, backend, palette, ring_peak, now_ms,
            );
        }

        // 8. Optional audio/camera device selection popup menu (`calls_device_menu.cpp`).
        if let Some(kind) = self.device_menu {
            self.show_device_popup(ui, rect, kind, backend, palette);
        }
    }

    fn paint_fingerprint_and_signal_bars(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        call: &ActiveCallSnapshot,
        emojis: &[String; 4],
        palette: &CallPalette,
    ) {
        let pill_h = 30.0;
        let emoji_pill_w = 114.0;
        let bars_pill_w = 36.0;
        let gap = 3.0;
        let total_w = emoji_pill_w + gap + bars_pill_w;
        let pill_left = rect.center().x - total_w * 0.5;
        let pill_top = rect.top() + 14.0;

        let emoji_rect = Rect::from_min_size(
            Pos2::new(pill_left, pill_top),
            Vec2::new(emoji_pill_w, pill_h),
        );
        let bars_rect = Rect::from_min_size(
            Pos2::new(emoji_rect.right() + gap, pill_top),
            Vec2::new(bars_pill_w, pill_h),
        );
        let full_rect = emoji_rect.union(bars_rect);

        // Two-part joined pill matching `calls_emoji_fingerprint.cpp:275-287`.
        painter.rect_filled(
            emoji_rect,
            CornerRadius {
                nw: 15,
                sw: 15,
                ne: 4,
                se: 4,
            },
            palette.call_bg_button,
        );
        painter.rect_filled(
            bars_rect,
            CornerRadius {
                nw: 4,
                sw: 4,
                ne: 15,
                se: 15,
            },
            palette.call_bg_button,
        );

        // Render the 4 crisp verification fingerprint glyphs across the left segment.
        let slot_w = (emoji_pill_w - 14.0) / 4.0;
        for (i, emoji) in emojis.iter().enumerate() {
            let cx = emoji_rect.left() + 7.0 + slot_w * (i as f32 + 0.5);
            paint_fingerprint_emoji_glyph(
                painter,
                Pos2::new(cx, emoji_rect.center().y),
                8.5,
                emoji,
            );
        }

        // Render 4 signal bars inside the right segment.
        let bars_origin = Pos2::new(bars_rect.left() + 10.0, bars_rect.center().y - 5.0);
        paint_signal_bars(
            painter,
            bars_origin,
            call.signal_bars,
            2.5,
            2.0,
            4.0,
            10.0,
            palette.call_name_fg,
        );

        let resp = ui.allocate_rect(full_rect, Sense::click());
        if resp.clicked() {
            self.fingerprint_tooltip_pinned = !self.fingerprint_tooltip_pinned;
        }
        if resp.hovered() || self.fingerprint_tooltip_pinned {
            let tip = emoji_fingerprint_tooltip(&call.peer.name);
            let tip_w = ((tip.len() as f32) * 5.8 + 44.0).clamp(320.0, rect.width() - 32.0);
            let tip_rect = Rect::from_min_size(
                Pos2::new(rect.center().x - tip_w * 0.5, full_rect.bottom() + 8.0),
                Vec2::new(tip_w, 48.0),
            );
            painter.rect_filled(
                tip_rect,
                CornerRadius::same(8),
                Color32::from_rgba_unmultiplied(16, 20, 24, 236),
            );
            painter.rect_stroke(
                tip_rect,
                CornerRadius::same(8),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 32)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                Pos2::new(tip_rect.center().x, tip_rect.top() + 14.0),
                Align2::CENTER_CENTER,
                tr::lng_confcall_e2e_badge(),
                FontId::proportional(11.5),
                palette.call_answer_bg,
            );
            painter.text(
                Pos2::new(tip_rect.center().x, tip_rect.top() + 32.0),
                Align2::CENTER_CENTER,
                tip,
                FontId::proportional(10.5),
                palette.call_name_fg,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_bottom_controls(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        call: &ActiveCallSnapshot,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        ring_peak: f32,
        now_ms: u64,
    ) {
        let is_incoming_waiting = call.call_type == CallType::Incoming
            && matches!(call.state, CallState::Starting | CallState::WaitingIncoming);
        let is_busy = call.state == CallState::Busy;
        let is_waiting_user = call.state == CallState::WaitingUserConfirmation;

        let controls_y = rect.bottom() - CALL_BUTTON_HEIGHT - 12.0;

        if is_incoming_waiting {
            // Incoming ringing layout: Decline (red) + Answer (green with outer audio-peak pulse).
            let spacing = 92.0;
            let decline_center = Pos2::new(rect.center().x - spacing * 0.5, controls_y + 34.0);
            let answer_center = Pos2::new(rect.center().x + spacing * 0.5, controls_y + 34.0);

            let decline_lbl = tr::lng_call_decline();
            if paint_call_circle_button(
                ui,
                painter,
                decline_center,
                palette.call_hangup_bg,
                0.0,
                palette.call_hangup_bg,
                &decline_lbl,
                palette,
                |p, c| paint_phone_icon(p, c, 22.0, 135.0, palette.call_icon_fg),
            ) {
                backend.hangup_call(now_ms);
            }

            let accept_lbl = tr::lng_call_accept();
            if paint_call_circle_button(
                ui,
                painter,
                answer_center,
                palette.call_answer_bg,
                ring_peak,
                palette.call_answer_outer,
                &accept_lbl,
                palette,
                |p, c| paint_phone_icon(p, c, 22.0, 0.0, palette.call_icon_fg),
            ) {
                backend.answer_call(now_ms);
            }
            return;
        }

        if is_busy || is_waiting_user {
            let spacing = 84.0;
            let left_center = Pos2::new(rect.center().x - spacing * 0.5, controls_y + 34.0);
            let right_center = Pos2::new(rect.center().x + spacing * 0.5, controls_y + 34.0);

            let cancel_lbl = tr::lng_call_cancel();
            if paint_call_circle_button(
                ui,
                painter,
                left_center,
                palette.call_icon_bg_active,
                0.0,
                palette.call_icon_bg_active,
                &cancel_lbl,
                palette,
                |p, c| paint_close_icon(p, c, 20.0, palette.call_icon_fg_active),
            ) {
                backend.hangup_call(now_ms);
            }

            let primary_label = if is_busy {
                tr::lng_call_redial()
            } else {
                tr::lng_call_start()
            };
            if paint_call_circle_button(
                ui,
                painter,
                right_center,
                palette.call_answer_bg,
                0.0,
                palette.call_answer_outer,
                &primary_label,
                palette,
                |p, c| paint_phone_icon(p, c, 22.0, 0.0, palette.call_icon_fg),
            ) {
                if is_busy {
                    backend.redial_call(now_ms);
                } else {
                    backend.confirm_outgoing_call(false, now_ms);
                }
            }
            return;
        }

        // Standard active / outgoing call row: Screen Share · Camera · End Call · Mute.
        let count = 4usize;
        let total_w = (count as f32) * CALL_BUTTON_WIDTH + ((count - 1) as f32) * 14.0;
        let start_x = rect.center().x - total_w * 0.5 + CALL_BUTTON_WIDTH * 0.5;
        let cy = controls_y + 34.0;
        let step_x = CALL_BUTTON_WIDTH + 14.0;

        // 1. Screen Share button
        let screen_center = Pos2::new(start_x, cy);
        let (screen_bg, screen_fg) = if call.screen_share_active {
            (palette.call_icon_bg_active, palette.call_icon_fg_active)
        } else {
            (palette.call_icon_bg, palette.call_icon_fg)
        };
        let screen_lbl = tr::lng_call_screencast();
        if paint_call_circle_button(
            ui,
            painter,
            screen_center,
            screen_bg,
            0.0,
            palette.call_icon_bg,
            &screen_lbl,
            palette,
            |p, c| paint_screen_share_icon(p, c, 20.0, screen_fg),
        ) {
            if call.screen_share_active {
                backend.toggle_call_screen_share();
            } else {
                self.screen_picker
                    .open_with_sources(backend.screen_share_sources());
            }
        }

        // 2. Camera button + corner device selector chevron
        let cam_center = Pos2::new(start_x + step_x, cy);
        let cam_active = call.camera_state == VideoState::Active;
        let (cam_bg, cam_fg) = if cam_active {
            (palette.call_icon_bg_active, palette.call_icon_fg_active)
        } else {
            (palette.call_icon_bg, palette.call_icon_fg)
        };
        let cam_lbl = if cam_active {
            tr::lng_call_stop_video()
        } else {
            tr::lng_call_start_video()
        };
        if paint_call_circle_button(
            ui,
            painter,
            cam_center,
            cam_bg,
            0.0,
            palette.call_icon_bg,
            &cam_lbl,
            palette,
            |p, c| paint_camera_icon(p, c, 21.0, cam_fg, !cam_active),
        ) {
            backend.toggle_call_camera();
        }
        if paint_corner_chevron(ui, painter, cam_center, palette) {
            self.device_menu = if self.device_menu == Some(DeviceMenuKind::Camera) {
                None
            } else {
                Some(DeviceMenuKind::Camera)
            };
        }

        // 3. Hangup button (animated angle & color from Answer -> Hangup via `hangup_progress`)
        let hangup_center = Pos2::new(start_x + step_x * 2.0, cy);
        let hangup_bg = lerp_color(
            palette.call_answer_bg,
            palette.call_hangup_bg,
            self.hangup_progress,
        );
        let hangup_angle = 135.0 * self.hangup_progress;
        let end_lbl = tr::lng_call_end_call();
        if paint_call_circle_button(
            ui,
            painter,
            hangup_center,
            hangup_bg,
            0.0,
            palette.call_hangup_bg,
            &end_lbl,
            palette,
            |p, c| paint_phone_icon(p, c, 22.0, hangup_angle, palette.call_icon_fg),
        ) {
            backend.hangup_call(now_ms);
        }

        // 4. Mute button (animated cross-line) + corner audio device selector chevron
        let mute_center = Pos2::new(start_x + step_x * 3.0, cy);
        let mute_bg = lerp_color(
            palette.call_icon_bg,
            palette.call_icon_bg_active,
            self.mute_cross_progress,
        );
        let mute_fg = lerp_color(
            palette.call_icon_fg,
            palette.call_icon_fg_active,
            self.mute_cross_progress,
        );
        let mute_lbl = if call.muted {
            tr::lng_call_unmute_audio()
        } else {
            tr::lng_call_mute_audio()
        };
        if paint_call_circle_button(
            ui,
            painter,
            mute_center,
            mute_bg,
            0.0,
            palette.call_icon_bg,
            &mute_lbl,
            palette,
            |p, c| paint_mic_icon(p, c, 20.0, mute_fg, self.mute_cross_progress, mute_fg),
        ) {
            backend.set_call_muted(!call.muted);
        }
        if paint_corner_chevron(ui, painter, mute_center, palette) {
            self.device_menu = if self.device_menu == Some(DeviceMenuKind::Audio) {
                None
            } else {
                Some(DeviceMenuKind::Audio)
            };
        }
    }

    fn paint_rate_call_card(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        call: &ActiveCallSnapshot,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
    ) {
        let card_w = 300.0_f32.min(rect.width() - 32.0);
        let card_h = if self.rating_selected > 0 && self.rating_selected <= 3 {
            210.0
        } else {
            152.0
        };
        let card_rect = Rect::from_min_size(
            Pos2::new(rect.center().x - card_w * 0.5, rect.top() + 236.0),
            Vec2::new(card_w, card_h),
        );

        painter.rect_filled(
            card_rect,
            CornerRadius::same(24),
            Color32::from_rgba_unmultiplied(0, 0, 0, 56),
        );
        painter.text(
            Pos2::new(card_rect.center().x, card_rect.top() + 26.0),
            Align2::CENTER_CENTER,
            tr::lng_call_rate_title(),
            FontId::proportional(15.5),
            palette.call_name_fg,
        );
        painter.text(
            Pos2::new(card_rect.center().x, card_rect.top() + 50.0),
            Align2::CENTER_CENTER,
            tr::lng_call_rate_description(),
            FontId::proportional(13.0),
            palette.call_status_fg,
        );

        // 5 interactive vector stars (`calls_rate_call.cpp:192-208`).
        let star_size = 36.0;
        let star_skip = 6.0;
        let full_stars_w = 5.0 * star_size + 4.0 * star_skip;
        let stars_left = card_rect.center().x - full_stars_w * 0.5;
        let stars_y = card_rect.top() + 102.0;

        self.rating_hover = 0;
        for i in 0..5u8 {
            let cx = stars_left + (i as f32) * (star_size + star_skip) + star_size * 0.5;
            let star_rect = Rect::from_center_size(Pos2::new(cx, stars_y), Vec2::splat(star_size));
            let resp = ui.allocate_rect(star_rect, Sense::click());
            if resp.hovered() {
                self.rating_hover = i + 1;
            }
            if resp.clicked() {
                self.rating_selected = i + 1;
                if self.rating_selected >= 4 {
                    backend.submit_call_rating(self.rating_selected, String::new());
                }
            }
        }
        let active_stars = if self.rating_hover > 0 {
            self.rating_hover
        } else {
            self.rating_selected
        };
        for i in 0..5u8 {
            let cx = stars_left + (i as f32) * (star_size + star_skip) + star_size * 0.5;
            let filled = (i + 1) <= active_stars;
            let color = if filled {
                Color32::from_rgb(0xFF, 0xD1, 0x5C)
            } else {
                palette.call_name_fg
            };
            paint_star(painter, Pos2::new(cx, stars_y), 15.0, color, filled);
        }

        // Optional feedback input when user rated 1-3 stars.
        if self.rating_selected > 0 && self.rating_selected <= 3 {
            let input_rect = Rect::from_min_size(
                Pos2::new(card_rect.left() + 18.0, card_rect.top() + 136.0),
                Vec2::new(card_rect.width() - 36.0, 56.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.rating_comment)
                        .hint_text(tr::lng_call_rate_comment()),
                );
            });
        }

        // Bottom Close / Submit button (`callRateCloseWidth = 358`, `callRateCloseHeight = 52`).
        let btn_w = 320.0_f32.min(rect.width() - 48.0);
        let btn_rect = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.bottom() - 48.0),
            Vec2::new(btn_w, 46.0),
        );
        let btn_resp = ui.allocate_rect(btn_rect, Sense::click());
        let btn_bg = if btn_resp.hovered() {
            Color32::WHITE
        } else {
            palette.call_icon_bg_active
        };
        painter.rect_filled(btn_rect, CornerRadius::same(10), btn_bg);
        let label = if self.rating_selected > 0 && self.rating_selected <= 3 {
            strings::SEND_FEEDBACK.to_owned()
        } else if call.submitted_rating.is_some() {
            tr::lng_box_done()
        } else {
            tr::lng_close()
        };
        painter.text(
            btn_rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(15.5),
            palette.call_icon_fg_active,
        );
        if btn_resp.clicked() {
            if self.rating_selected > 0 && call.submitted_rating.is_none() {
                backend.submit_call_rating(self.rating_selected, self.rating_comment.clone());
            }
            backend.finish_call_rating();
        }
    }

    fn show_device_popup(
        &mut self,
        ui: &mut Ui,
        panel_rect: Rect,
        kind: DeviceMenuKind,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
    ) {
        let popup_rect = Rect::from_min_size(
            Pos2::new(panel_rect.center().x - 130.0, panel_rect.bottom() - 270.0),
            Vec2::new(260.0, 168.0),
        );
        let painter = ui.painter_at(popup_rect);
        painter.rect_filled(popup_rect, CornerRadius::same(10), palette.group_menu_bg);
        painter.rect_stroke(
            popup_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 36)),
            egui::StrokeKind::Inside,
        );

        let settings = backend.audio_settings().clone();
        let outputs: Vec<(String, String)> = backend
            .output_devices()
            .iter()
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();
        let inputs: Vec<(String, String)> = backend
            .input_devices()
            .iter()
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();
        let cameras: Vec<(String, String)> = backend
            .camera_devices()
            .iter()
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(popup_rect.shrink(10.0)),
            |ui| {
                ui.horizontal(|ui| {
                    let heading = match kind {
                        DeviceMenuKind::Audio => strings::AUDIO_DEVICES,
                        DeviceMenuKind::Camera => strings::CAMERA_DEVICE,
                    };
                    ui.label(
                        RichText::new(heading)
                            .strong()
                            .color(palette.group_members_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (close_r, close_resp) =
                            ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                        let fg = if close_resp.hovered() {
                            palette.group_members_fg
                        } else {
                            palette.group_member_sub_fg
                        };
                        paint_close_icon(ui.painter(), close_r.center(), 12.0, fg);
                        if close_resp.clicked() {
                            self.device_menu = None;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| match kind {
                    DeviceMenuKind::Audio => {
                        ui.label(
                            RichText::new(tr::lng_group_call_speakers())
                                .size(11.0)
                                .color(palette.group_active_fg),
                        );
                        for (id, name) in &outputs {
                            let sel = *id == settings.output_device_id;
                            if ui.selectable_label(sel, name).clicked() {
                                backend.set_audio_devices(
                                    settings.input_device_id.clone(),
                                    id.clone(),
                                );
                                self.device_menu = None;
                            }
                        }
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(tr::lng_group_call_microphone())
                                .size(11.0)
                                .color(palette.group_active_fg),
                        );
                        for (id, name) in &inputs {
                            let sel = *id == settings.input_device_id;
                            if ui.selectable_label(sel, name).clicked() {
                                backend.set_audio_devices(
                                    id.clone(),
                                    settings.output_device_id.clone(),
                                );
                                self.device_menu = None;
                            }
                        }
                    }
                    DeviceMenuKind::Camera => {
                        for (id, name) in &cameras {
                            let sel = *id == settings.camera_device_id;
                            if ui.selectable_label(sel, name).clicked() {
                                backend.set_camera_device(id.clone());
                                self.device_menu = None;
                            }
                        }
                    }
                });
            },
        );
    }
}

fn paint_top_icon_button(
    ui: &mut Ui,
    painter: &Painter,
    rect: Rect,
    active: bool,
    palette: &CallPalette,
    paint_icon: impl FnOnce(&Painter, Pos2, Color32),
) -> bool {
    let resp = ui.allocate_rect(rect, Sense::click());
    if resp.hovered() || active {
        painter.circle_filled(
            rect.center(),
            rect.width() * 0.5,
            if active {
                palette.call_icon_bg_active
            } else {
                palette.call_icon_bg
            },
        );
    }
    let fg = if active {
        palette.call_icon_fg_active
    } else {
        palette.call_name_fg
    };
    paint_icon(painter, rect.center(), fg);
    resp.clicked()
}

#[allow(clippy::too_many_arguments)]
fn paint_call_circle_button(
    ui: &mut Ui,
    painter: &Painter,
    center: Pos2,
    bg: Color32,
    outer_pulse: f32,
    outer_bg: Color32,
    label: &str,
    palette: &CallPalette,
    paint_icon: impl FnOnce(&Painter, Pos2),
) -> bool {
    let r = CALL_BUTTON_BG_SIZE * 0.5;
    let hit_rect = Rect::from_center_size(
        Pos2::new(center.x, center.y + 8.0),
        Vec2::new(CALL_BUTTON_WIDTH, CALL_BUTTON_HEIGHT),
    );
    let resp = ui.allocate_rect(hit_rect, Sense::click());

    if outer_pulse > 0.01 {
        let outer_r = r + outer_pulse.clamp(0.0, 1.0) * CALL_BUTTON_OUTER_RADIUS;
        painter.circle_filled(center, outer_r, outer_bg);
    }

    let fill = if resp.hovered() {
        lerp_color(bg, Color32::WHITE, 0.14)
    } else {
        bg
    };
    painter.circle_filled(center, r, fill);
    paint_icon(painter, center);

    painter.text(
        Pos2::new(center.x, center.y + r + 14.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.0),
        palette.call_name_fg,
    );
    resp.clicked()
}

fn paint_corner_chevron(
    ui: &mut Ui,
    painter: &Painter,
    button_center: Pos2,
    palette: &CallPalette,
) -> bool {
    let badge_center = Pos2::new(button_center.x + 17.0, button_center.y - 17.0);
    let rect = Rect::from_center_size(badge_center, Vec2::splat(20.0));
    let resp = ui.allocate_rect(rect, Sense::click());
    painter.circle_filled(badge_center, 10.0, palette.call_bg);
    painter.circle_filled(
        badge_center,
        8.5,
        if resp.hovered() {
            palette.call_icon_bg_active
        } else {
            palette.call_icon_bg
        },
    );
    let fg = if resp.hovered() {
        palette.call_icon_fg_active
    } else {
        palette.call_icon_fg
    };
    paint_chevron_down_icon(painter, badge_center, 11.0, fg);
    resp.clicked()
}

fn paint_incoming_video_backdrop(
    painter: &Painter,
    rect: Rect,
    call: &ActiveCallSnapshot,
    palette: &CallPalette,
    now_ms: u64,
) {
    paint_radial_backdrop(
        painter,
        rect,
        rect.center(),
        lerp_color(
            call.peer.avatar_color,
            Color32::from_rgb(0x14, 0x1E, 0x2B),
            0.45,
        ),
        Color32::from_rgb(0x0E, 0x13, 0x1A),
    );
    let phase = (now_ms as f32) * 0.002;
    let ring_r = 92.0 + phase.sin() * 6.0;
    painter.circle_stroke(
        Pos2::new(rect.center().x, rect.center().y - 18.0),
        ring_r,
        Stroke::new(2.0, with_alpha(Color32::WHITE, 0.18)),
    );
    paint_avatar(
        painter,
        Pos2::new(rect.center().x, rect.center().y - 18.0),
        62.0,
        call.peer.avatar_color,
        &call.peer.initials,
        40.0,
    );
    painter.text(
        Pos2::new(rect.center().x, rect.center().y + 68.0),
        Align2::CENTER_CENTER,
        format!("{} · {}", call.peer.name, strings::HD_VIDEO_SUFFIX),
        FontId::proportional(15.0),
        palette.call_name_fg,
    );
}

fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    if (target - current).abs() <= max_delta {
        target
    } else if target > current {
        current + max_delta
    } else {
        current - max_delta
    }
}
