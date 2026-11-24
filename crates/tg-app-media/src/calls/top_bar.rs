//! Active call top bar rendered over the main window with mute toggle, hangup, and return-to-call.
//!
//! Port of `Telegram/SourceFiles/calls/calls_top_bar.cpp`.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_calls::{CallState, InstanceState, MuteState};
use tg_lang::tr;
use tg_ui::fonts::ensure_tdesktop_fonts;

use crate::calls::backend::{ActiveCallSnapshot, GroupCallSnapshot};
use crate::calls::format::format_duration_text;
use crate::calls::theme::{
    CALL_BAR_HEIGHT, CallPalette, paint_avatar, paint_horizontal_gradient, paint_mic_icon,
    paint_phone_icon, paint_signal_bars, with_alpha,
};

/// Top bar color/animation state (`Calls::BarState` in `calls_top_bar.cpp:44-49`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopBarState {
    /// Connecting / disconnected group call transport.
    Connecting,
    /// Unmuted active call (green bar for 1:1, green-teal gradient for group call).
    Active,
    /// Self-muted call (gray bar for 1:1, blue-cyan gradient for group call).
    Muted,
    /// Force-muted or raised hand in group call (magenta-violet-blue gradient).
    ForceMuted,
}

/// Computes [`TopBarState`] for a group call (`BarStateFromMuteState`, `calls_top_bar.cpp:61-74`).
#[must_use]
pub const fn group_bar_state(mute: MuteState, instance: InstanceState) -> TopBarState {
    if matches!(instance, InstanceState::Disconnected) {
        TopBarState::Connecting
    } else if matches!(mute, MuteState::ForceMuted | MuteState::RaisedHand) {
        TopBarState::ForceMuted
    } else if matches!(mute, MuteState::Muted) {
        TopBarState::Muted
    } else {
        TopBarState::Active
    }
}

/// User actions emitted by [`CallsTopBar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopBarAction {
    /// Restore and focus the active 1:1 or group call window.
    ReturnToCall,
    /// Toggle local microphone mute.
    ToggleMute,
    /// End 1:1 call or leave group call.
    Hangup,
}

/// Persistent visual state for the active call top bar (`Calls::TopBar`).
#[derive(Debug, Default)]
pub struct CallsTopBar {
    /// Animated diagonal mute slash (`0.0` = active, `1.0` = muted).
    pub mute_cross_progress: f32,
}

impl CallsTopBar {
    /// Renders the 38px active call top bar at the top of `ui` when either a 1:1 call or group
    /// call is active (non-rating).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        active_call: Option<&ActiveCallSnapshot>,
        active_group: Option<&GroupCallSnapshot>,
        palette: &CallPalette,
        now_ms: u64,
    ) -> Option<TopBarAction> {
        let show_1to1 =
            active_call.filter(|c| !c.rating_requested && !c.state.is_finishing_or_finished());
        if show_1to1.is_none() && active_group.is_none() {
            return None;
        }

        ensure_tdesktop_fonts(ui.ctx());
        let mut action = None;
        egui::Panel::top("rustgram_calls_top_bar")
            .exact_size(CALL_BAR_HEIGHT)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                if let Some(call) = show_1to1 {
                    action = self.paint_1to1_bar(ui, rect, call, palette);
                } else if let Some(group) = active_group {
                    action = self.paint_group_bar(ui, rect, group, palette, now_ms);
                }
            });
        action
    }

    /// Paints the 1:1 active call green/gray bar directly into `rect`.
    pub fn paint_1to1_bar(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        call: &ActiveCallSnapshot,
        palette: &CallPalette,
    ) -> Option<TopBarAction> {
        let target_cross = if call.muted { 1.0 } else { 0.0 };
        self.mute_cross_progress += (target_cross - self.mute_cross_progress) * 0.35;

        let painter = ui.painter_at(rect);
        let bg = if call.muted {
            palette.call_bar_muted_bg
        } else {
            palette.call_bar_active_bg
        };
        painter.rect_filled(rect, 0.0, bg);

        ensure_tdesktop_fonts(ui.ctx());
        let mut action = None;

        // Left: Mute toggle button (`callBarMuteToggle = 41x38`).
        let mute_rect = Rect::from_min_size(rect.min, Vec2::new(41.0, CALL_BAR_HEIGHT));
        let mute_resp = ui.allocate_rect(mute_rect, Sense::click());
        if mute_resp.hovered() {
            painter.rect_filled(mute_rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 32));
        }
        paint_mic_icon(
            &painter,
            mute_rect.center(),
            18.0,
            palette.call_bar_fg,
            self.mute_cross_progress,
            palette.call_bar_fg,
        );
        if mute_resp.clicked() {
            action = Some(TopBarAction::ToggleMute);
        }

        // Duration timer + 4 signal bars (`calls_top_bar.cpp:760-775`).
        let mut left_x = mute_rect.right() + 4.0;
        if call.state == CallState::Established {
            let dur = format_duration_text((call.duration_ms / 1000) as i64);
            painter.text(
                Pos2::new(left_x, rect.center().y),
                Align2::LEFT_CENTER,
                &dur,
                FontId::proportional(13.0),
                palette.call_bar_fg,
            );
            left_x += (dur.len() as f32) * 7.8 + 10.0;
        }
        paint_signal_bars(
            &painter,
            Pos2::new(left_x, rect.center().y - 6.0),
            call.signal_bars,
            3.0,
            1.5,
            3.0,
            12.0,
            palette.call_bar_fg,
        );

        // Right: Hangup button + "End Call" label.
        let hangup_w = 104.0;
        let hangup_rect = Rect::from_min_size(
            Pos2::new(rect.right() - hangup_w, rect.top()),
            Vec2::new(hangup_w, CALL_BAR_HEIGHT),
        );
        let hangup_resp = ui.allocate_rect(hangup_rect, Sense::click());
        if hangup_resp.hovered() {
            painter.rect_filled(
                hangup_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 36),
            );
        }
        paint_phone_icon(
            &painter,
            Pos2::new(hangup_rect.left() + 20.0, rect.center().y),
            16.0,
            135.0,
            palette.call_bar_fg,
        );
        painter.text(
            Pos2::new(hangup_rect.left() + 36.0, rect.center().y),
            Align2::LEFT_CENTER,
            tr::lng_call_bar_hangup(),
            FontId::proportional(13.0),
            palette.call_bar_fg,
        );
        if hangup_resp.clicked() {
            action = Some(TopBarAction::Hangup);
        }

        // Center clickable return-to-call region (`_info`).
        let info_rect = Rect::from_min_max(
            Pos2::new(mute_rect.right(), rect.top()),
            Pos2::new(hangup_rect.left(), rect.bottom()),
        );
        let info_resp = ui.allocate_rect(info_rect, Sense::click());
        let label = if info_rect.width() >= 220.0 {
            call.peer.name.as_str()
        } else {
            call.peer.short_name.as_str()
        };
        painter.text(
            Pos2::new(rect.center().x, rect.center().y),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.5),
            palette.call_bar_fg,
        );
        if info_resp.clicked() && action.is_none() {
            action = Some(TopBarAction::ReturnToCall);
        }

        action
    }

    /// Paints the group call gradient top bar directly into `rect`.
    pub fn paint_group_bar(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        group: &GroupCallSnapshot,
        palette: &CallPalette,
        now_ms: u64,
    ) -> Option<TopBarAction> {
        let bar_state = group_bar_state(group.mute_state, group.instance_state);
        let target_cross = if bar_state == TopBarState::Active {
            0.0
        } else {
            1.0
        };
        self.mute_cross_progress += (target_cross - self.mute_cross_progress) * 0.35;

        let painter = ui.painter_at(rect);
        let (grad_left, grad_right) = match bar_state {
            TopBarState::Active => (palette.group_live_1, palette.group_live_2),
            TopBarState::Muted => (palette.group_muted_1, palette.group_muted_2),
            TopBarState::ForceMuted => (palette.group_force_muted_1, palette.group_force_muted_3),
            TopBarState::Connecting => (palette.call_bar_muted_bg, palette.call_bar_muted_bg),
        };
        paint_horizontal_gradient(&painter, rect, grad_left, grad_right);

        // Subtle animated speaking pulse strip at bottom edge during live group audio.
        let peak_level = group
            .members
            .iter()
            .map(|m| m.audio_level)
            .fold(group.local_audio_level, f32::max);
        if peak_level > 0.05 && bar_state != TopBarState::Connecting {
            let phase = (now_ms as f32) * 0.008;
            let bar_h = 2.0 + peak_level * 2.5 * (0.6 + 0.4 * phase.sin().abs());
            let strip = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - bar_h), rect.max);
            painter.rect_filled(strip, 0.0, with_alpha(Color32::WHITE, 0.28));
        }

        ensure_tdesktop_fonts(ui.ctx());
        let mut action = None;

        // Left: Mute toggle button
        let mute_rect = Rect::from_min_size(rect.min, Vec2::new(41.0, CALL_BAR_HEIGHT));
        let mute_resp = ui.allocate_rect(mute_rect, Sense::click());
        if mute_resp.hovered() && bar_state != TopBarState::Connecting {
            painter.rect_filled(mute_rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 32));
        }
        paint_mic_icon(
            &painter,
            mute_rect.center(),
            18.0,
            palette.call_bar_fg,
            self.mute_cross_progress,
            palette.call_bar_fg,
        );
        if mute_resp.clicked() && bar_state != TopBarState::Connecting {
            action = Some(TopBarAction::ToggleMute);
        }

        // Overlapping speaker avatars (`groupCallTopBarUserpics`: size 28, shift 8).
        let mut x = mute_rect.right() + 14.0;
        for member in group.members.iter().take(3) {
            paint_avatar(
                &painter,
                Pos2::new(x, rect.center().y),
                12.0,
                member.peer.avatar_color,
                &member.peer.initials,
                9.5,
            );
            x += 18.0;
        }

        // Right: Leave button
        let hangup_w = 88.0;
        let hangup_rect = Rect::from_min_size(
            Pos2::new(rect.right() - hangup_w, rect.top()),
            Vec2::new(hangup_w, CALL_BAR_HEIGHT),
        );
        let hangup_resp = ui.allocate_rect(hangup_rect, Sense::click());
        if hangup_resp.hovered() {
            painter.rect_filled(
                hangup_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 36),
            );
        }
        paint_phone_icon(
            &painter,
            Pos2::new(hangup_rect.left() + 18.0, rect.center().y),
            16.0,
            135.0,
            palette.call_bar_fg,
        );
        painter.text(
            Pos2::new(hangup_rect.left() + 34.0, rect.center().y),
            Align2::LEFT_CENTER,
            tr::lng_group_call_leave(),
            FontId::proportional(13.0),
            palette.call_bar_fg,
        );
        if hangup_resp.clicked() {
            action = Some(TopBarAction::Hangup);
        }

        // Center clickable title (returns to group call window).
        let info_rect = Rect::from_min_max(
            Pos2::new(mute_rect.right(), rect.top()),
            Pos2::new(hangup_rect.left(), rect.bottom()),
        );
        let info_resp = ui.allocate_rect(info_rect, Sense::click());
        let title = if bar_state == TopBarState::Connecting {
            tr::lng_group_call_connecting()
        } else {
            format!(
                "{} ({})",
                group.display_title(),
                tr::lng_group_call_members(group.joined_count() as i64)
            )
        };
        painter.text(
            Pos2::new(rect.center().x, rect.center().y),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(13.5),
            palette.call_bar_fg,
        );
        if info_resp.clicked() && action.is_none() {
            action = Some(TopBarAction::ReturnToCall);
        }

        action
    }
}
