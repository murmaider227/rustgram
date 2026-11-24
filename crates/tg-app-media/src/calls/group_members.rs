//! Group call participants list, speaking indicator timing, and per-participant volume controls.
//!
//! Port of `Telegram/SourceFiles/calls/group/calls_group_members.cpp` and
//! `Telegram/SourceFiles/calls/group/calls_group_members_row.cpp`.

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, ScrollArea, Sense, Ui, Vec2};
use tg_calls::group::{
    DEFAULT_VOLUME, MAX_VOLUME, MIN_VOLUME, SOUND_STATUS_KEPT_FOR_MS, SPEAK_LEVEL_THRESHOLD,
};
use tg_lang::tr;

use crate::calls::backend::{GroupCallSnapshot, GroupMemberView};
use crate::calls::format::format_volume_percent;
use crate::calls::strings;
use crate::calls::theme::{
    CallPalette, paint_audio_blob, paint_avatar, paint_camera_icon, paint_hand_icon,
    paint_invited_or_calling_icon, paint_mic_icon, paint_screen_share_icon, with_alpha,
};
use tg_app_core::stand_in::types::PeerId;

/// Duration of the blob enter/exit animation in ms (`kBlobsEnterDuration = 250`).
pub const BLOBS_ENTER_DURATION_MS: u64 = 250;
/// Duration to keep `"wants to speak"` status text after raising hand (`kKeepRaisedHandStatusDuration = 3000`).
pub const RAISED_HAND_STATUS_DURATION_MS: u64 = 3_000;
/// Smooth audio level interpolation time constant in ms (`kLevelDuration = 100 + 500 * 0.23 = 215`).
pub const LEVEL_DURATION_MS: f64 = 215.0;
/// Minimum userpic scale while speaking blobs pulse (`kUserpicMinScale = 0.8`).
pub const USERPIC_MIN_SCALE: f32 = 0.8;

/// Row visual state mirroring `MembersRow::State` (`calls_group_members_row.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRowState {
    /// Unmuted / speaking or sounding.
    Active,
    /// Self-muted listener who can unmute themselves.
    Inactive,
    /// Force-muted by admin.
    Muted,
    /// Force-muted with hand raised.
    RaisedHand,
    /// Invited to call, not yet joined.
    Invited,
    /// Actively being rung.
    Calling,
}

/// Deterministic speaking indicator & blob animation view-model for a single participant.
///
/// Ports `MembersRow::updateLevel` and `MembersRow::updateBlobAnimation`
/// (`calls_group_members_row.cpp:318-361`).
#[derive(Debug, Clone)]
pub struct SpeakingIndicatorState {
    /// Whether the participant is currently considered sounding.
    pub sounding: bool,
    /// Whether the participant is currently considered speaking (VAD active).
    pub speaking: bool,
    /// Last monotonic timestamp (ms) when `level >= SPEAK_LEVEL_THRESHOLD`.
    pub last_sounding_ms: u64,
    /// Last update timestamp (ms).
    pub last_tick_ms: u64,
    /// Blob enter/exit progress (`0.0..=1.0`).
    pub enter: f32,
    /// Smoothed visual audio level (`0.0..=1.0`).
    pub smoothed_level: f32,
    /// Target audio level (`0.0..=1.0`).
    pub target_level: f32,
    /// Timestamp (ms) when `"wants to speak"` temporary status expires.
    pub raised_hand_status_until_ms: Option<u64>,
}

impl Default for SpeakingIndicatorState {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeakingIndicatorState {
    /// Creates an idle indicator state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sounding: false,
            speaking: false,
            last_sounding_ms: 0,
            last_tick_ms: 0,
            enter: 0.0,
            smoothed_level: 0.0,
            target_level: 0.0,
            raised_hand_status_until_ms: None,
        }
    }

    /// Feeds a new normalized audio level sample at `now_ms`.
    pub fn update_level(&mut self, level: f32, voice: bool, now_ms: u64) {
        let clamped = level.clamp(0.0, 1.0);
        let was_sounding = self.sounding;
        self.target_level = clamped;
        if clamped >= SPEAK_LEVEL_THRESHOLD {
            self.last_sounding_ms = now_ms;
            self.sounding = true;
            if !was_sounding {
                // Port of `MembersRow::setSounding`: reset blob clock when sounding starts.
                self.last_tick_ms = now_ms;
            }
            if voice {
                self.speaking = true;
            }
        }
        if self.last_tick_ms == 0 {
            self.last_tick_ms = now_ms;
        }
    }

    /// Marks the participant as having just raised their hand at `now_ms`.
    pub fn trigger_raised_hand(&mut self, now_ms: u64) {
        self.raised_hand_status_until_ms =
            Some(now_ms.saturating_add(RAISED_HAND_STATUS_DURATION_MS));
    }

    /// Returns whether the temporary `"wants to speak"` status line is still active at `now_ms`.
    #[must_use]
    pub fn show_raised_hand_status(&self, now_ms: u64) -> bool {
        self.raised_hand_status_until_ms
            .is_some_and(|until| now_ms < until)
    }

    /// Advances speaking decay, blob enter/exit ramp, and smoothed audio level to `now_ms`.
    ///
    /// Exact port of `MembersRow::updateBlobAnimation` (`calls_group_members_row.cpp:338-361`):
    /// - Sounding state stays active for `SOUND_STATUS_KEPT_FOR_MS` (1500 ms) after last peak.
    /// - During the final `BLOBS_ENTER_DURATION_MS` (250 ms), `enter` ramps linearly down to 0.
    /// - While sounding, `enter` ramps linearly up to 1 over `BLOBS_ENTER_DURATION_MS`.
    pub fn tick(&mut self, now_ms: u64) {
        let dt_ms = now_ms.saturating_sub(self.last_tick_ms);
        self.last_tick_ms = now_ms;

        if self.sounding {
            let sounding_finishes_at = self
                .last_sounding_ms
                .saturating_add(SOUND_STATUS_KEPT_FOR_MS);
            let starts_finishing_at = sounding_finishes_at.saturating_sub(BLOBS_ENTER_DURATION_MS);

            if now_ms >= sounding_finishes_at {
                self.enter = 0.0;
                self.sounding = false;
                self.speaking = false;
                self.target_level = 0.0;
            } else if now_ms > starts_finishing_at {
                let remaining = (sounding_finishes_at - now_ms) as f32;
                self.enter = (remaining / (BLOBS_ENTER_DURATION_MS as f32)).clamp(0.0, 1.0);
            } else if self.enter < 1.0 {
                let delta = (dt_ms as f32) / (BLOBS_ENTER_DURATION_MS as f32);
                self.enter = (self.enter + delta).clamp(0.0, 1.0);
            }
        } else {
            self.enter = 0.0;
        }

        // Smooth level towards target using `LEVEL_DURATION_MS`.
        let step = ((dt_ms as f64) / LEVEL_DURATION_MS).clamp(0.0, 1.0) as f32;
        self.smoothed_level += (self.target_level - self.smoothed_level) * step;
        if self.smoothed_level < 0.001 && self.target_level == 0.0 {
            self.smoothed_level = 0.0;
        }
    }

    /// Computes avatar scale factor (`calls_group_members_row.cpp:429-433`).
    #[must_use]
    pub fn userpic_scale(&self) -> f32 {
        if !self.sounding || self.enter <= 0.0 {
            return 1.0;
        }
        let scale_userpic = USERPIC_MIN_SCALE + (1.0 - USERPIC_MIN_SCALE) * self.smoothed_level;
        scale_userpic * self.enter + (1.0 - self.enter)
    }
}

/// Computes the visual [`MemberRowState`] for a [`GroupMemberView`] (`calls_group_members_row.cpp:161-190`).
#[must_use]
pub fn classify_member_state(member: &GroupMemberView) -> MemberRowState {
    if member.is_calling {
        return MemberRowState::Calling;
    }
    if member.is_invited {
        return MemberRowState::Invited;
    }
    let p = &member.participant;
    if !p.muted || (p.sounding && p.ssrc != 0) || p.additional_sounding {
        MemberRowState::Active
    } else if p.can_self_unmute {
        MemberRowState::Inactive
    } else if p.raised_hand_rating != 0 {
        MemberRowState::RaisedHand
    } else {
        MemberRowState::Muted
    }
}

/// Computes the status line text and color for a group participant row.
#[must_use]
pub fn member_status_text_and_color(
    member: &GroupMemberView,
    indicator: &SpeakingIndicatorState,
    palette: &CallPalette,
    now_ms: u64,
) -> (String, Color32) {
    let state = classify_member_state(member);
    match state {
        MemberRowState::Calling => (
            tr::lng_group_call_calling_status(),
            palette.group_member_sub_fg,
        ),
        MemberRowState::Invited => (
            tr::lng_group_call_invited_status(),
            palette.group_member_sub_fg,
        ),
        MemberRowState::Active => {
            if member.participant.muted_by_me {
                (
                    tr::lng_group_call_muted_by_me_status(),
                    palette.group_member_muted_icon,
                )
            } else if member.participant.speaking || indicator.speaking {
                let vol_suffix = if member.participant.volume != DEFAULT_VOLUME {
                    format!(" · {}", format_volume_percent(member.participant.volume))
                } else {
                    String::new()
                };
                (
                    format!("{}{vol_suffix}", tr::lng_group_call_active()),
                    palette.group_member_active,
                )
            } else if member.participant.volume != DEFAULT_VOLUME {
                (
                    format!(
                        "{} · {}",
                        tr::lng_group_call_inactive(),
                        format_volume_percent(member.participant.volume)
                    ),
                    palette.group_member_inactive_status,
                )
            } else {
                (
                    tr::lng_group_call_inactive(),
                    palette.group_member_inactive_status,
                )
            }
        }
        MemberRowState::Inactive => {
            if !member.peer.about.is_empty() {
                (member.peer.about.clone(), palette.group_member_sub_fg)
            } else {
                (tr::lng_group_call_inactive(), palette.group_member_sub_fg)
            }
        }
        MemberRowState::RaisedHand => {
            if indicator.show_raised_hand_status(now_ms)
                || member.participant.raised_hand_rating > 0
            {
                (
                    tr::lng_group_call_raised_hand_status(),
                    palette.group_member_inactive_status,
                )
            } else {
                (tr::lng_group_call_inactive(), palette.group_member_sub_fg)
            }
        }
        MemberRowState::Muted => (
            tr::lng_group_call_force_muted(),
            palette.group_member_sub_fg,
        ),
    }
}

/// User actions emitted from the participants list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupMembersAction {
    /// Adjust participant volume (`1..=20000`).
    SetVolume(PeerId, i32),
    /// Toggle mute for a participant.
    SetMuted(PeerId, bool),
    /// Admin grants speaking permission to a participant with a raised hand.
    AllowToSpeak(PeerId),
}

/// Persistent UI state for the group call participants list.
#[derive(Debug, Default)]
pub struct GroupMembersPanelState {
    /// Per-participant speaking indicator animation state.
    pub indicators: HashMap<PeerId, SpeakingIndicatorState>,
    /// Currently expanded participant row (shows inline volume slider & mute/allow controls).
    pub expanded_peer: Option<PeerId>,
}

impl GroupMembersPanelState {
    /// Synchronizes speaking indicators with the latest [`GroupCallSnapshot`] at `now_ms`.
    pub fn sync_from_snapshot(&mut self, snapshot: &GroupCallSnapshot, now_ms: u64) {
        for member in &snapshot.members {
            let entry = self.indicators.entry(member.peer.id).or_default();
            if member.participant.raised_hand_rating != 0
                && entry.raised_hand_status_until_ms.is_none()
            {
                entry.trigger_raised_hand(now_ms);
            } else if member.participant.raised_hand_rating == 0 {
                entry.raised_hand_status_until_ms = None;
            }
            let effective_level = if member.participant.muted || member.participant.muted_by_me {
                0.0
            } else {
                member.audio_level
            };
            entry.update_level(effective_level, member.participant.speaking, now_ms);
            entry.tick(now_ms);
        }
    }

    /// Renders the scrollable participants list inside `ui`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
        now_ms: u64,
    ) -> Option<GroupMembersAction> {
        self.sync_from_snapshot(snapshot, now_ms);
        let mut action = None;

        ScrollArea::vertical()
            .id_salt("group_call_members_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for member in &snapshot.members {
                    let indicator = self
                        .indicators
                        .get(&member.peer.id)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(a) = self.show_member_row(
                        ui,
                        member,
                        &indicator,
                        snapshot.can_manage,
                        palette,
                        now_ms,
                    ) {
                        action = Some(a);
                    }
                }
            });

        action
    }

    fn show_member_row(
        &mut self,
        ui: &mut Ui,
        member: &GroupMemberView,
        indicator: &SpeakingIndicatorState,
        can_manage: bool,
        palette: &CallPalette,
        now_ms: u64,
    ) -> Option<GroupMembersAction> {
        let mut action = None;
        let is_expanded = self.expanded_peer == Some(member.peer.id);
        let row_h = 54.0;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());

        if resp.clicked() && !member.is_self && !member.is_invited {
            self.expanded_peer = if is_expanded {
                None
            } else {
                Some(member.peer.id)
            };
        }

        let painter = ui.painter();
        let bg = if resp.hovered() || is_expanded {
            palette.group_members_bg_over
        } else {
            palette.group_members_bg
        };
        painter.rect_filled(rect, CornerRadius::same(8), bg);

        // Avatar + speaking blob aura.
        let avatar_center = Pos2::new(rect.left() + 28.0, rect.center().y);
        let base_r = 19.0;
        if indicator.sounding && indicator.enter > 0.01 && !member.participant.muted_by_me {
            let blob_color = if member.participant.speaking {
                palette.group_member_active
            } else {
                palette.group_member_inactive_status
            };
            let phase = (now_ms as f32) * 0.007;
            let amp = 3.0 + indicator.smoothed_level * 5.0;
            paint_audio_blob(
                painter,
                avatar_center,
                base_r + 4.0 * indicator.enter,
                amp * indicator.enter,
                phase,
                8,
                with_alpha(blob_color, 0.24 * indicator.enter),
            );
            paint_audio_blob(
                painter,
                avatar_center,
                base_r + 2.5 * indicator.enter,
                amp * 0.7 * indicator.enter,
                -phase * 1.2,
                6,
                with_alpha(blob_color, 0.42 * indicator.enter),
            );
        }

        let scaled_r = base_r * indicator.userpic_scale();
        paint_avatar(
            painter,
            avatar_center,
            scaled_r,
            member.peer.avatar_color,
            &member.peer.initials,
            13.0,
        );

        // Name + self badge + media badges.
        let text_left = rect.left() + 56.0;
        let name = if member.is_self {
            format!("{} {}", member.peer.name, strings::SELF_BADGE_SUFFIX)
        } else {
            member.peer.name.clone()
        };
        painter.text(
            Pos2::new(text_left, rect.top() + 18.0),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(13.5),
            palette.group_members_fg,
        );

        let (status_text, status_color) =
            member_status_text_and_color(member, indicator, palette, now_ms);
        painter.text(
            Pos2::new(text_left, rect.top() + 37.0),
            Align2::LEFT_CENTER,
            status_text,
            FontId::proportional(11.5),
            status_color,
        );

        // Right-side status icons: camera / screen badge + mic / raised-hand indicator.
        let mut icon_x = rect.right() - 22.0;
        let state = classify_member_state(member);
        match state {
            MemberRowState::RaisedHand => {
                painter.circle_filled(
                    Pos2::new(icon_x, rect.center().y),
                    11.0,
                    with_alpha(palette.group_member_inactive_status, 0.22),
                );
                paint_hand_icon(
                    painter,
                    Pos2::new(icon_x, rect.center().y),
                    15.0,
                    palette.group_member_inactive_status,
                );
            }
            MemberRowState::Active => {
                let col = if member.participant.muted_by_me {
                    palette.group_member_muted_icon
                } else {
                    palette.group_member_active
                };
                paint_mic_icon(
                    painter,
                    Pos2::new(icon_x, rect.center().y),
                    17.0,
                    col,
                    if member.participant.muted_by_me {
                        1.0
                    } else {
                        0.0
                    },
                    palette.group_member_muted_icon,
                );
            }
            MemberRowState::Muted => {
                paint_mic_icon(
                    painter,
                    Pos2::new(icon_x, rect.center().y),
                    17.0,
                    palette.group_member_muted_icon,
                    1.0,
                    palette.group_member_muted_icon,
                );
            }
            MemberRowState::Inactive => {
                paint_mic_icon(
                    painter,
                    Pos2::new(icon_x, rect.center().y),
                    17.0,
                    palette.group_member_inactive_icon,
                    1.0,
                    palette.group_member_inactive_icon,
                );
            }
            MemberRowState::Invited | MemberRowState::Calling => {
                paint_invited_or_calling_icon(
                    painter,
                    Pos2::new(icon_x, rect.center().y),
                    17.0,
                    palette.group_member_sub_fg,
                    state == MemberRowState::Calling,
                );
            }
        }
        icon_x -= 24.0;
        if member.screen_frame_size.is_some() {
            paint_screen_share_icon(
                painter,
                Pos2::new(icon_x, rect.center().y),
                15.0,
                palette.group_active_fg,
            );
            icon_x -= 22.0;
        }
        if member.camera_frame_size.is_some() {
            paint_camera_icon(
                painter,
                Pos2::new(icon_x, rect.center().y),
                15.0,
                palette.group_active_fg,
                member.participant.is_camera_paused(),
            );
        }

        // Expanded per-participant volume slider & mute/allow actions (`calls_volume_item.cpp`).
        if is_expanded && !member.is_self && !member.is_invited {
            egui::Frame::new()
                .fill(palette.group_menu_bg)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    let mut vol_pct = ((member.participant.volume as f32) / (MAX_VOLUME as f32)
                        * 200.0)
                        .round()
                        .clamp(1.0, 200.0) as i32;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{}: {vol_pct}%", strings::VOLUME_PREFIX))
                                .size(11.5)
                                .color(palette.group_members_fg),
                        );
                        let slider = egui::Slider::new(&mut vol_pct, 1..=200)
                            .show_value(false)
                            .trailing_fill(true);
                        if ui.add(slider).changed() {
                            let raw_vol = ((vol_pct as f32 / 200.0) * (MAX_VOLUME as f32))
                                .round()
                                .clamp(MIN_VOLUME as f32, MAX_VOLUME as f32)
                                as i32;
                            action = Some(GroupMembersAction::SetVolume(member.peer.id, raw_vol));
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let mute_label =
                            if member.participant.muted_by_me || member.participant.muted {
                                tr::lng_group_call_context_unmute_for_me()
                            } else {
                                tr::lng_group_call_context_mute_for_me()
                            };
                        if ui.small_button(mute_label).clicked() {
                            let next_muted =
                                !(member.participant.muted_by_me || member.participant.muted);
                            action = Some(GroupMembersAction::SetMuted(member.peer.id, next_muted));
                        }
                        if can_manage
                            && member.participant.raised_hand_rating != 0
                            && ui
                                .small_button(tr::lng_group_call_context_unmute())
                                .clicked()
                        {
                            action = Some(GroupMembersAction::AllowToSpeak(member.peer.id));
                        }
                    });
                });
        }

        action
    }
}
