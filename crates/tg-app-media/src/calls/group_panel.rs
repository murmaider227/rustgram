//! Group call window: video tiles viewport, participants list, animated central mute/raise-hand
//! button with speaking blobs, Push-to-Talk key handling, settings trigger, and leave confirmation.
//!
//! Port of `Telegram/SourceFiles/calls/group/calls_group_panel.cpp`.

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Painter, Pos2, Rect, RichText, Sense, Stroke, Ui,
    Vec2,
};
use tg_calls::{GroupCallState, InstanceState, MuteState};
use tg_lang::tr;
use tg_ui::fonts::ensure_tdesktop_fonts;

use crate::calls::backend::{CallsBackend, GroupCallSnapshot};
use crate::calls::group_members::{GroupMembersAction, GroupMembersPanelState};
use crate::calls::group_messages::{
    E2eVerificationBannerState, GroupMessagesAction, GroupMessagesPanelState,
};
use crate::calls::group_settings::{GroupRecordingDialog, GroupRtmpDialog, GroupSettingsDialog};
use crate::calls::group_viewport::{ViewportAction, collect_video_tiles, render_group_viewport};
use crate::calls::screen_picker::ScreenSharePickerDialog;
use crate::calls::strings;
use crate::calls::theme::{
    CallPalette, GROUP_CALL_HEIGHT, GROUP_CALL_WIDE_MIN_WIDTH, GROUP_CALL_WIDE_SIZE,
    GROUP_CALL_WIDTH, lerp_color, paint_audio_blob, paint_camera_icon, paint_chevron_down_icon,
    paint_close_icon, paint_gear_icon, paint_hand_icon, paint_mic_icon, paint_minimize_icon,
    paint_phone_icon, paint_pin_icon, paint_screen_share_icon, paint_sidebar_icon, with_alpha,
};

/// Persistent UI state for the Group Call window (`Calls::Group::Panel`).
#[derive(Debug)]
pub struct GroupCallPanelState {
    /// Whether the group call window is visible (minimizing leaves the top bar active).
    pub visible: bool,
    /// Whether the right-side participants sidebar is shown in wide video mode.
    pub show_members_column: bool,
    /// Whether the window is pinned on top.
    pub pinned_on_top: bool,
    /// Nested participants list state & speaking indicator animations.
    pub members_panel: GroupMembersPanelState,
    /// Conference E2E verification emoji banner state.
    pub e2e_banner: E2eVerificationBannerState,
    /// Nested in-call chat & paid Star reactions panel state.
    pub messages_panel: GroupMessagesPanelState,
    /// Whether the sidebar/body currently displays the in-call Chat tab instead of Participants.
    pub show_messages_tab: bool,
    /// Nested group call settings dialog.
    pub settings_dialog: GroupSettingsDialog,
    /// Nested group call recording box (`phone.toggleGroupCallRecord`).
    pub recording_dialog: GroupRecordingDialog,
    /// Nested RTMP stream key dialog (`phone.getGroupCallStreamRtmpUrl`).
    pub rtmp_dialog: GroupRtmpDialog,
    /// Nested screen-sharing source picker dialog.
    pub screen_picker: ScreenSharePickerDialog,
    /// Nested invite members dialog (`calls_group_invite_controller.cpp`).
    pub invite_dialog: GroupInviteDialog,
    /// Whether the "Leave / End Voice Chat" confirmation box is open for admins.
    pub leave_confirm_open: bool,
    /// Whether the "Start Scheduled Call Now" confirmation box is open for admins.
    pub start_scheduled_confirm_open: bool,
    /// Checkbox inside leave confirmation ("End Voice Chat for everyone").
    pub end_for_all_checked: bool,
    /// Whether user requested opening the Join-As identity switcher.
    pub request_switch_join_as: bool,
}

impl Default for GroupCallPanelState {
    fn default() -> Self {
        Self {
            visible: true,
            show_members_column: true,
            pinned_on_top: false,
            members_panel: GroupMembersPanelState::default(),
            e2e_banner: E2eVerificationBannerState::default(),
            messages_panel: GroupMessagesPanelState::default(),
            show_messages_tab: false,
            settings_dialog: GroupSettingsDialog::new(),
            recording_dialog: GroupRecordingDialog::default(),
            rtmp_dialog: GroupRtmpDialog::default(),
            screen_picker: ScreenSharePickerDialog::default(),
            invite_dialog: GroupInviteDialog::default(),
            leave_confirm_open: false,
            start_scheduled_confirm_open: false,
            end_for_all_checked: false,
            request_switch_join_as: false,
        }
    }
}

impl GroupCallPanelState {
    /// Handles Spacebar / configured Push-to-Talk key press & release while the window is active.
    pub fn handle_push_to_talk_input(
        &self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        snapshot: &GroupCallSnapshot,
        now_ms: u64,
    ) {
        if self.settings_dialog.recording_ptt_shortcut || ctx.egui_wants_keyboard_input() {
            return;
        }
        let settings = backend.audio_settings().clone();
        if !settings.push_to_talk_enabled
            && !matches!(
                snapshot.mute_state,
                MuteState::Muted | MuteState::PushToTalk
            )
        {
            return;
        }
        let ptt_key = parse_primary_key(&settings.push_to_talk_shortcut).unwrap_or(Key::Space);
        let key_down = ctx.input(|i| i.key_down(ptt_key));
        if key_down && snapshot.mute_state == MuteState::Muted {
            backend.set_group_push_to_talk(true, now_ms);
        } else if !key_down && snapshot.mute_state == MuteState::PushToTalk {
            backend.set_group_push_to_talk(false, now_ms);
        }
    }

    /// Shows the group call floating window (plus nested Settings / Leave dialogs) in `ctx`.
    pub fn show_window(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        let Some(snapshot) = backend.active_group_call().cloned() else {
            return;
        };
        if !self.visible {
            return;
        }
        self.handle_push_to_talk_input(ctx, backend, &snapshot, now_ms);

        let has_video = !collect_video_tiles(&snapshot).is_empty();
        let default_size = if has_video {
            GROUP_CALL_WIDE_SIZE
        } else {
            Vec2::new(GROUP_CALL_WIDTH, GROUP_CALL_HEIGHT)
        };

        let mut keep_open = self.visible;
        ensure_tdesktop_fonts(ctx);
        let win_prefix = if snapshot.group_peer.is_broadcast {
            tr::lng_group_call_title_channel()
        } else {
            tr::lng_group_call_title()
        };
        egui::Window::new(format!("{win_prefix} — {}", snapshot.display_title()))
            .id(egui::Id::unique("rustgram_group_call_window"))
            .title_bar(false)
            .resizable(true)
            .default_size(default_size)
            .min_size(Vec2::new(GROUP_CALL_WIDTH, GROUP_CALL_HEIGHT))
            .open(&mut keep_open)
            .frame(egui::Frame::NONE.corner_radius(CornerRadius::same(12)))
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                self.paint_panel_contents(ui, rect, &snapshot, backend, palette, now_ms);
            });
        self.visible = keep_open;

        // Nested Voice Chat Settings dialog (`calls_group_settings.cpp`).
        self.settings_dialog
            .show(ctx, backend, &snapshot, palette, now_ms);

        if self.settings_dialog.request_recording_box {
            self.settings_dialog.request_recording_box = false;
            self.recording_dialog.open_for(snapshot.recording);
        }
        if self.settings_dialog.request_rtmp_box {
            self.settings_dialog.request_rtmp_box = false;
            self.rtmp_dialog.open_for(snapshot.group_peer.id, backend);
        }
        if self.settings_dialog.request_invite_box {
            self.settings_dialog.request_invite_box = false;
            self.invite_dialog.open();
        }

        // Nested Recording box (`calls_group_recording_box.cpp`).
        self.recording_dialog.show(ctx, backend, &snapshot, palette);

        // Nested RTMP Stream Key box (`calls_group_rtmp.cpp`).
        self.rtmp_dialog.show(ctx, backend, palette);

        // Nested Invite Members box (`calls_group_invite_controller.cpp`).
        self.invite_dialog.show(ctx, backend, &snapshot, palette);

        // Nested Screen-Sharing Source Picker.
        if let Some(source) =
            self.screen_picker
                .show(ctx, "group_call_screen_share_picker", palette, || {
                    backend.screen_share_sources()
                })
        {
            backend.start_group_screen_share_with_source(&source, now_ms);
        }

        // Admin "Start Scheduled Call Now" confirmation dialog (`calls_group_panel.cpp`).
        if self.start_scheduled_confirm_open {
            self.show_start_scheduled_confirm(ctx, backend, &snapshot, palette);
        }

        // Admin Leave / End Voice Chat confirmation dialog (`calls_group_menu.cpp:LeaveBox`).
        if self.leave_confirm_open {
            self.show_leave_confirm(ctx, backend, palette, now_ms);
        }
    }

    /// Paints the complete group call layout into `rect` (shared between desktop window and
    /// headless screenshot capture).
    pub fn paint_panel_contents(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        snapshot: &GroupCallSnapshot,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(12), palette.group_bg);
        painter.rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)),
            egui::StrokeKind::Inside,
        );

        let has_video = !collect_video_tiles(snapshot).is_empty();
        let wide_mode = has_video && rect.width() >= GROUP_CALL_WIDE_MIN_WIDTH;

        // 1. Header bar (height 54px).
        let header_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 54.0));
        self.paint_header(
            ui,
            &painter,
            header_rect,
            snapshot,
            wide_mode,
            palette,
            now_ms,
        );

        // 2. Bottom control dock (height 94px).
        let controls_h = 94.0;
        let controls_rect =
            Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - controls_h), rect.max);

        // 1b. Optional conference E2E emoji verification banner (`tde2e`).
        let e2e_banner_area = Rect::from_min_max(
            Pos2::new(rect.left() + 14.0, header_rect.bottom() + 2.0),
            Pos2::new(rect.right() - 14.0, controls_rect.top() - 8.0),
        );
        let e2e_h = self
            .e2e_banner
            .paint_banner(ui, &painter, e2e_banner_area, snapshot, palette);

        // 1c. Optional Scheduled Group Call countdown & start/reminder bar (`calls_group_scheduled_labels.cpp`).
        let mut sched_h = 0.0_f32;
        if let Some(schedule_date) = backend.active_group_call_schedule_date() {
            let sched_area = Rect::from_min_max(
                Pos2::new(rect.left() + 14.0, header_rect.bottom() + 4.0 + e2e_h),
                Pos2::new(rect.right() - 14.0, controls_rect.top() - 8.0),
            );
            sched_h = self.paint_scheduled_bar(
                ui,
                &painter,
                sched_area,
                schedule_date,
                backend,
                snapshot,
                palette,
            );
        }

        // 3. Center workspace: video tiles grid + participants/chat column (wide) or stacked (narrow).
        let body_rect = Rect::from_min_max(
            Pos2::new(
                rect.left() + 14.0,
                header_rect.bottom() + 4.0 + e2e_h + sched_h,
            ),
            Pos2::new(rect.right() - 14.0, controls_rect.top() - 8.0),
        );

        let show_chat_tab_option = snapshot.is_conference
            || snapshot.messages_enabled
            || !snapshot.chat_messages.is_empty()
            || snapshot.stars_total > 0
            || snapshot.stars_scheduled > 0;
        if !show_chat_tab_option {
            self.show_messages_tab = false;
        }

        let mut members_action: Option<GroupMembersAction> = None;
        let mut messages_action: Option<GroupMessagesAction> = None;
        let mut viewport_action: Option<ViewportAction> = None;

        if wide_mode {
            let sidebar_w = if self.show_members_column { 268.0 } else { 0.0 };
            let gap = if self.show_members_column { 12.0 } else { 0.0 };
            let video_rect = Rect::from_min_max(
                body_rect.min,
                Pos2::new(body_rect.right() - sidebar_w - gap, body_rect.bottom()),
            );
            viewport_action = render_group_viewport(ui, video_rect, snapshot, palette, now_ms);

            if self.show_members_column {
                let members_rect = Rect::from_min_max(
                    Pos2::new(video_rect.right() + gap, body_rect.top()),
                    body_rect.max,
                );
                painter.rect_filled(
                    members_rect,
                    CornerRadius::same(10),
                    palette.group_members_bg,
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(members_rect.shrink(8.0)),
                    |ui| {
                        self.paint_sidebar_tabs(ui, snapshot, show_chat_tab_option, palette);
                        ui.add_space(6.0);
                        if self.show_messages_tab {
                            messages_action = self.messages_panel.show(ui, snapshot, palette);
                        } else {
                            members_action = self.members_panel.show(ui, snapshot, palette, now_ms);
                        }
                    },
                );
            }
        } else {
            let mut members_top = body_rect.top();
            if has_video {
                let video_h = (body_rect.height() * 0.42).clamp(130.0, 220.0);
                let video_rect =
                    Rect::from_min_size(body_rect.min, Vec2::new(body_rect.width(), video_h));
                viewport_action = render_group_viewport(ui, video_rect, snapshot, palette, now_ms);
                members_top = video_rect.bottom() + 8.0;
            }
            let members_rect =
                Rect::from_min_max(Pos2::new(body_rect.left(), members_top), body_rect.max);
            painter.rect_filled(
                members_rect,
                CornerRadius::same(10),
                palette.group_members_bg,
            );
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(members_rect.shrink(8.0)),
                |ui| {
                    if show_chat_tab_option {
                        self.paint_sidebar_tabs(ui, snapshot, true, palette);
                        ui.add_space(6.0);
                    }
                    if self.show_messages_tab {
                        messages_action = self.messages_panel.show(ui, snapshot, palette);
                    } else {
                        members_action = self.members_panel.show(ui, snapshot, palette, now_ms);
                    }
                },
            );
        }

        // Dispatch viewport, member & in-call chat/stars interactions to backend.
        if let Some(vp_act) = viewport_action {
            match vp_act {
                ViewportAction::ClickTile(ep) => {
                    let toggle_off = snapshot.large_video.as_ref() == Some(&ep);
                    backend.select_large_video(if toggle_off { None } else { Some(ep) }, now_ms);
                }
                ViewportAction::TogglePin => backend.toggle_pin_large_video(),
                ViewportAction::ClearLarge => backend.select_large_video(None, now_ms),
            }
        }
        if let Some(m_act) = members_action {
            match m_act {
                GroupMembersAction::SetVolume(peer_id, vol) => {
                    backend.set_participant_volume(peer_id, vol);
                }
                GroupMembersAction::SetMuted(peer_id, muted) => {
                    backend.set_participant_muted(peer_id, muted);
                }
                GroupMembersAction::AllowToSpeak(peer_id) => {
                    backend.allow_participant_to_speak(peer_id);
                }
            }
        }
        if let Some(msg_act) = messages_action {
            match msg_act {
                GroupMessagesAction::SendMessage { text, paid_stars } => {
                    backend.send_group_call_message(text, paid_stars, now_ms);
                }
                GroupMessagesAction::DeleteMessage {
                    msg_id,
                    delete_all_from,
                    report_spam,
                } => {
                    backend.delete_group_call_message(msg_id, delete_all_from, report_spam);
                }
                GroupMessagesAction::AddPaidStars(count) => {
                    backend.add_group_paid_stars_reaction(count);
                }
                GroupMessagesAction::FlushPaidStars => {
                    backend.flush_group_paid_stars_reaction(now_ms);
                }
                GroupMessagesAction::CancelPaidStars => {
                    backend.cancel_group_paid_stars_reaction();
                }
                GroupMessagesAction::RefreshStars => {
                    backend.refresh_group_stars();
                }
            }
        }

        // 4. Bottom control dock.
        self.paint_bottom_dock(
            ui,
            &painter,
            controls_rect,
            snapshot,
            backend,
            palette,
            now_ms,
        );

        // 5. Conference E2E verification tooltip painted above body & controls.
        self.e2e_banner
            .paint_tooltip(&painter, rect, snapshot, palette);
    }

    fn paint_sidebar_tabs(
        &mut self,
        ui: &mut Ui,
        snapshot: &GroupCallSnapshot,
        show_chat_tab: bool,
        palette: &CallPalette,
    ) {
        ui.horizontal(|ui| {
            let members_label = format!(
                "{} ({})",
                strings::PARTICIPANTS_HEADER,
                snapshot.joined_count()
            );
            if show_chat_tab {
                let members_active = !self.show_messages_tab;
                if ui
                    .selectable_label(
                        members_active,
                        RichText::new(members_label)
                            .strong()
                            .size(12.5)
                            .color(if members_active {
                                palette.group_active_fg
                            } else {
                                palette.group_member_sub_fg
                            }),
                    )
                    .clicked()
                {
                    self.show_messages_tab = false;
                }
                let chat_count = snapshot.chat_messages.len();
                let chat_label = if chat_count > 0 {
                    format!("Chat ({chat_count})")
                } else {
                    "Chat".to_owned()
                };
                if ui
                    .selectable_label(
                        self.show_messages_tab,
                        RichText::new(chat_label).strong().size(12.5).color(
                            if self.show_messages_tab {
                                palette.group_active_fg
                            } else {
                                palette.group_member_sub_fg
                            },
                        ),
                    )
                    .clicked()
                {
                    self.show_messages_tab = true;
                }
                if !snapshot.is_conference && snapshot.stars_total > 0 {
                    let gold = Color32::from_rgb(0xFF, 0xC8, 0x3D);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+ Invite").clicked() {
                            self.invite_dialog.open();
                        }
                        ui.label(
                            RichText::new(format!("{}", snapshot.stars_total))
                                .size(11.0)
                                .color(gold),
                        );
                        let (sr, _) = ui.allocate_exact_size(Vec2::splat(11.0), Sense::hover());
                        tg_app_core::ui_icons::paint_star(ui.painter(), sr.center(), 4.8, gold);
                    });
                } else {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+ Invite").clicked() {
                            self.invite_dialog.open();
                        }
                    });
                }
            } else {
                ui.label(
                    RichText::new(strings::PARTICIPANTS_HEADER)
                        .strong()
                        .size(13.0)
                        .color(palette.group_members_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("+ Invite").clicked() {
                        self.invite_dialog.open();
                    }
                    ui.label(
                        RichText::new(format!("{}", snapshot.joined_count()))
                            .size(12.0)
                            .color(palette.group_active_fg),
                    );
                });
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_header(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        snapshot: &GroupCallSnapshot,
        wide_mode: bool,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        // Left: Title + participant count + optional REC indicator.
        let left = rect.left() + 18.0;
        painter.text(
            Pos2::new(left, rect.top() + 20.0),
            Align2::LEFT_CENTER,
            snapshot.display_title(),
            FontId::proportional(16.0),
            palette.group_members_fg,
        );

        let sub = match snapshot.state {
            GroupCallState::Joining | GroupCallState::Connecting => tr::lng_group_call_connecting(),
            _ => tr::lng_group_call_members(snapshot.joined_count() as i64),
        };
        painter.text(
            Pos2::new(left, rect.top() + 39.0),
            Align2::LEFT_CENTER,
            sub,
            FontId::proportional(12.0),
            palette.group_member_sub_fg,
        );

        if snapshot.recording {
            let pulse = 0.55 + 0.45 * ((now_ms as f32 * 0.005).sin());
            let rec_pos = Pos2::new(left + 120.0, rect.top() + 39.0);
            painter.circle_filled(
                rec_pos,
                4.5,
                with_alpha(palette.group_member_muted_icon, pulse),
            );
            painter.text(
                Pos2::new(rec_pos.x + 10.0, rec_pos.y),
                Align2::LEFT_CENTER,
                strings::REC_BADGE,
                FontId::proportional(11.0),
                palette.group_member_muted_icon,
            );
        }

        // Right header controls: Join-As badge · Sidebar toggle · Pin · Minimize · Close.
        let cy = rect.center().y;
        let close_r = Rect::from_center_size(Pos2::new(rect.right() - 22.0, cy), Vec2::splat(26.0));
        let min_r = Rect::from_center_size(Pos2::new(rect.right() - 52.0, cy), Vec2::splat(26.0));
        let pin_r = Rect::from_center_size(Pos2::new(rect.right() - 82.0, cy), Vec2::splat(26.0));

        if paint_header_btn(ui, painter, close_r, false, palette, |p, c, fg| {
            paint_close_icon(p, c, 14.0, fg);
        }) {
            self.visible = false;
        }
        if paint_header_btn(ui, painter, min_r, false, palette, |p, c, fg| {
            paint_minimize_icon(p, c, 14.0, fg);
        }) {
            self.visible = false;
        }
        if paint_header_btn(
            ui,
            painter,
            pin_r,
            self.pinned_on_top,
            palette,
            |p, c, fg| {
                paint_pin_icon(p, c, 15.0, fg, self.pinned_on_top);
            },
        ) {
            self.pinned_on_top = !self.pinned_on_top;
        }

        let mut next_x = rect.right() - 114.0;
        if wide_mode {
            let col_r = Rect::from_center_size(Pos2::new(next_x, cy), Vec2::splat(26.0));
            if paint_header_btn(
                ui,
                painter,
                col_r,
                self.show_members_column,
                palette,
                |p, c, fg| {
                    paint_sidebar_icon(p, c, 16.0, fg);
                },
            ) {
                self.show_members_column = !self.show_members_column;
            }
            next_x -= 36.0;
        }

        let join_as_rect =
            Rect::from_center_size(Pos2::new(next_x - 38.0, cy), Vec2::new(96.0, 26.0));
        let join_as_resp = ui.allocate_rect(join_as_rect, Sense::click());
        painter.rect_filled(
            join_as_rect,
            CornerRadius::same(13),
            if join_as_resp.hovered() {
                palette.group_members_bg_over
            } else {
                palette.group_members_bg
            },
        );
        painter.text(
            Pos2::new(join_as_rect.center().x - 6.0, join_as_rect.center().y),
            Align2::CENTER_CENTER,
            strings::DISPLAY_AS_PILL,
            FontId::proportional(11.5),
            palette.group_active_fg,
        );
        paint_chevron_down_icon(
            painter,
            Pos2::new(join_as_rect.right() - 14.0, join_as_rect.center().y),
            10.0,
            palette.group_active_fg,
        );
        if join_as_resp.clicked() {
            self.request_switch_join_as = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_bottom_dock(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        snapshot: &GroupCallSnapshot,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        // Dock backdrop capsule in wide mode (`groupCallControlsBackRadius = 12`).
        let dock_w = 430.0_f32.min(rect.width() - 28.0);
        let dock_rect = Rect::from_center_size(
            Pos2::new(rect.center().x, rect.center().y),
            Vec2::new(dock_w, 82.0),
        );
        painter.rect_filled(
            dock_rect,
            CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(28, 34, 42, 224),
        );

        let cy = dock_rect.center().y - 6.0;
        let spacing = (dock_w / 5.0).clamp(64.0, 84.0);
        let c_mid = dock_rect.center().x;

        // Left-2: Camera toggle
        let cam_pos = Pos2::new(c_mid - spacing * 2.0, cy);
        let cam_bg = if snapshot.camera_active {
            palette.group_muted_1
        } else {
            palette.call_icon_bg
        };
        let cam_lbl = if snapshot.camera_active {
            tr::lng_call_stop_video()
        } else {
            tr::lng_settings_call_camera()
        };
        if paint_dock_button(
            ui,
            painter,
            cam_pos,
            21.0,
            cam_bg,
            &cam_lbl,
            palette,
            |p, c| paint_camera_icon(p, c, 20.0, Color32::WHITE, !snapshot.camera_active),
        ) {
            backend.toggle_group_camera(now_ms);
        }

        // Left-1: Screen Share toggle
        let scr_pos = Pos2::new(c_mid - spacing, cy);
        let scr_bg = if snapshot.screen_share_active {
            palette.group_muted_1
        } else {
            palette.call_icon_bg
        };
        let scr_lbl = if snapshot.screen_share_active {
            strings::STOP_SHARE_SHORT.to_owned()
        } else {
            tr::lng_group_call_screen()
        };
        if paint_dock_button(
            ui,
            painter,
            scr_pos,
            21.0,
            scr_bg,
            &scr_lbl,
            palette,
            |p, c| paint_screen_share_icon(p, c, 19.0, Color32::WHITE),
        ) {
            if snapshot.screen_share_active {
                backend.toggle_group_screen_share(now_ms);
            } else {
                self.screen_picker
                    .open_with_sources(backend.screen_share_sources());
            }
        }

        // Center: Main Mute / Push-to-Talk / Raise Hand button with animated speaking blobs!
        let mute_pos = Pos2::new(c_mid, cy);
        let is_connecting = snapshot.instance_state == InstanceState::Disconnected;
        let (mute_bg, mute_label, is_muted_slash, show_hand) = if is_connecting {
            (
                palette.call_bar_muted_bg,
                strings::CONNECTING_SHORT.to_owned(),
                true,
                false,
            )
        } else {
            match snapshot.mute_state {
                MuteState::Active => (
                    palette.group_live_1,
                    strings::SPEAKING_LABEL.to_owned(),
                    false,
                    false,
                ),
                MuteState::PushToTalk => (
                    palette.group_live_2,
                    strings::PUSH_TO_TALK_SHORT.to_owned(),
                    false,
                    false,
                ),
                MuteState::Muted => (
                    palette.group_muted_1,
                    tr::lng_group_call_unmute(),
                    true,
                    false,
                ),
                MuteState::ForceMuted => (
                    palette.group_force_muted_2,
                    strings::RAISE_HAND_LABEL.to_owned(),
                    true,
                    true,
                ),
                MuteState::RaisedHand => (
                    palette.group_force_muted_1,
                    strings::HAND_RAISED_LABEL.to_owned(),
                    true,
                    true,
                ),
            }
        };

        // Audio-reactive speaking blobs behind center button (`callMuteMinorBlobMinRadius`).
        let speaking_live = matches!(
            snapshot.mute_state,
            MuteState::Active | MuteState::PushToTalk
        ) && !is_connecting;
        if speaking_live {
            let phase = (now_ms as f32) * 0.008;
            let level = snapshot.local_audio_level.max(0.18);
            paint_audio_blob(
                painter,
                mute_pos,
                28.0 + level * 8.0,
                3.0 + level * 4.0,
                phase,
                8,
                with_alpha(palette.group_live_2, 0.25),
            );
            paint_audio_blob(
                painter,
                mute_pos,
                25.0 + level * 5.0,
                2.5 + level * 3.0,
                -phase * 1.25,
                6,
                with_alpha(palette.group_live_1, 0.36),
            );
        }

        if paint_dock_button(
            ui,
            painter,
            mute_pos,
            24.0,
            mute_bg,
            &mute_label,
            palette,
            |p, c| {
                if show_hand {
                    paint_hand_icon(p, c, 22.0, Color32::WHITE);
                } else {
                    paint_mic_icon(
                        p,
                        c,
                        22.0,
                        Color32::WHITE,
                        if is_muted_slash { 1.0 } else { 0.0 },
                        Color32::WHITE,
                    );
                }
            },
        ) && !is_connecting
        {
            let next = match snapshot.mute_state {
                MuteState::Active | MuteState::PushToTalk => MuteState::Muted,
                MuteState::Muted => MuteState::Active,
                MuteState::ForceMuted => MuteState::RaisedHand,
                MuteState::RaisedHand => MuteState::ForceMuted,
            };
            backend.set_group_mute_state(next, now_ms);
        }

        // Right-1: Settings gear
        let set_pos = Pos2::new(c_mid + spacing, cy);
        let settings_lbl = tr::lng_group_call_settings();
        if paint_dock_button(
            ui,
            painter,
            set_pos,
            21.0,
            palette.call_icon_bg,
            &settings_lbl,
            palette,
            |p, c| paint_gear_icon(p, c, 20.0, Color32::WHITE),
        ) {
            self.settings_dialog.open();
        }

        // Right-2: Leave button
        let leave_pos = Pos2::new(c_mid + spacing * 2.0, cy);
        let leave_lbl = tr::lng_group_call_leave();
        if paint_dock_button(
            ui,
            painter,
            leave_pos,
            21.0,
            palette.group_leave_bg,
            &leave_lbl,
            palette,
            |p, c| paint_phone_icon(p, c, 20.0, 135.0, Color32::WHITE),
        ) {
            if snapshot.can_manage {
                self.leave_confirm_open = true;
                self.end_for_all_checked = false;
            } else {
                backend.leave_group_call(false, now_ms);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_scheduled_bar(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        area: Rect,
        schedule_date: i32,
        backend: &mut dyn CallsBackend,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) -> f32 {
        let bar_h = 68.0_f32.min(area.height());
        if bar_h < 40.0 {
            return 0.0;
        }
        let bar_rect = Rect::from_min_size(area.min, Vec2::new(area.width(), bar_h));
        painter.rect_filled(bar_rect, CornerRadius::same(10), palette.group_members_bg);
        painter.rect_stroke(
            bar_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, palette.group_active_fg),
            egui::StrokeKind::Inside,
        );

        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let remaining_secs = (i64::from(schedule_date) - now_unix).max(0) as u64;
        let countdown = format_scheduled_countdown(remaining_secs);
        let subscribed = backend.active_group_call_schedule_subscribed();

        let title = if snapshot.group_peer.is_broadcast {
            tr::lng_group_call_scheduled_title_channel()
        } else {
            tr::lng_group_call_scheduled_title()
        };

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(bar_rect.shrink2(Vec2::new(12.0, 8.0))),
            |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(title)
                                .strong()
                                .size(12.5)
                                .color(palette.group_members_fg),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} {countdown}",
                                tr::lng_group_call_starts_in()
                            ))
                            .monospace()
                            .strong()
                            .size(13.0)
                            .color(palette.group_active_fg),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if snapshot.can_manage
                            && ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(tr::lng_group_call_start_now())
                                            .strong()
                                            .size(12.0)
                                            .color(Color32::WHITE),
                                    )
                                    .fill(palette.group_live_1)
                                    .corner_radius(CornerRadius::same(8)),
                                )
                                .clicked()
                        {
                            self.start_scheduled_confirm_open = true;
                        }
                        let rem_label = if subscribed {
                            tr::lng_group_call_cancel_reminder()
                        } else {
                            tr::lng_group_call_set_reminder()
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(rem_label)
                                        .size(12.0)
                                        .color(palette.group_members_fg),
                                )
                                .fill(palette.group_members_bg_over)
                                .corner_radius(CornerRadius::same(8)),
                            )
                            .clicked()
                        {
                            let _ = backend.toggle_group_call_start_subscription(!subscribed);
                        }
                    });
                });
            },
        );

        bar_h + 8.0
    }

    fn show_start_scheduled_confirm(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) {
        let mut keep_open = self.start_scheduled_confirm_open;
        let mut close_requested = false;
        let sure_text = if snapshot.group_peer.is_broadcast {
            tr::lng_group_call_start_now_sure_channel()
        } else {
            tr::lng_group_call_start_now_sure()
        };
        egui::Window::new(tr::lng_group_call_start_now())
            .id(egui::Id::unique("group_call_start_scheduled_confirm"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(320.0, 140.0))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(sure_text).color(palette.group_members_fg))
                        .wrap(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(tr::lng_group_call_start_now())
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(palette.group_live_1),
                            )
                            .clicked()
                        {
                            let _ = backend.start_scheduled_group_call();
                            close_requested = true;
                        }
                        if ui.button(tr::lng_cancel()).clicked() {
                            close_requested = true;
                        }
                    });
                });
            });
        self.start_scheduled_confirm_open = keep_open && !close_requested;
    }

    fn show_leave_confirm(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        let mut keep_open = self.leave_confirm_open;
        let mut close_requested = false;
        egui::Window::new(tr::lng_group_call_leave_title())
            .id(egui::Id::unique("group_call_leave_confirm"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(320.0, 150.0))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(tr::lng_group_call_leave_sure()).color(palette.group_members_fg),
                );
                ui.add_space(8.0);
                ui.checkbox(&mut self.end_for_all_checked, tr::lng_group_call_also_end());
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr::lng_group_call_leave()).clicked() {
                            backend.leave_group_call(self.end_for_all_checked, now_ms);
                            close_requested = true;
                        }
                        if ui.button(tr::lng_cancel()).clicked() {
                            close_requested = true;
                        }
                    });
                });
            });
        self.leave_confirm_open = keep_open && !close_requested;
    }
}

fn paint_header_btn(
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
                palette.group_active_fg
            } else {
                palette.group_members_bg_over
            },
        );
    }
    let fg = if active {
        Color32::WHITE
    } else {
        palette.group_member_sub_fg
    };
    paint_icon(painter, rect.center(), fg);
    resp.clicked()
}

#[allow(clippy::too_many_arguments)]
fn paint_dock_button(
    ui: &mut Ui,
    painter: &Painter,
    center: Pos2,
    radius: f32,
    bg: Color32,
    label: &str,
    palette: &CallPalette,
    paint_icon: impl FnOnce(&Painter, Pos2),
) -> bool {
    let hit = Rect::from_center_size(Pos2::new(center.x, center.y + 8.0), Vec2::new(66.0, 68.0));
    let resp = ui.allocate_rect(hit, Sense::click());
    let fill = if resp.hovered() {
        lerp_color(bg, Color32::WHITE, 0.14)
    } else {
        bg
    };
    painter.circle_filled(center, radius, fill);
    paint_icon(painter, center);
    painter.text(
        Pos2::new(center.x, center.y + radius + 13.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.0),
        palette.group_members_fg,
    );
    resp.clicked()
}

fn parse_primary_key(shortcut: &str) -> Option<Key> {
    let token = shortcut.rsplit('+').next()?.trim();
    Key::from_name(token)
}

/// Formats remaining seconds until a scheduled group call as `Dd HH:MM:SS` or `HH:MM:SS`
/// (`calls_group_scheduled_labels.cpp`).
#[must_use]
pub fn format_scheduled_countdown(remaining_secs: u64) -> String {
    let days = remaining_secs / 86_400;
    let rem = remaining_secs % 86_400;
    let hours = rem / 3_600;
    let mins = (rem % 3_600) / 60;
    let secs = rem % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{mins:02}:{secs:02}")
    } else {
        format!("{hours:02}:{mins:02}:{secs:02}")
    }
}

/// Modal participant picker for inviting contacts to an active group call
/// (`Calls::Group::InviteController` in `calls_group_invite_controller.cpp`).
#[derive(Debug, Default)]
pub struct GroupInviteDialog {
    /// Whether the dialog is open.
    pub open: bool,
    /// Case-insensitive search query filtering candidate peers.
    pub search_query: String,
    /// Peer IDs currently checked for invitation.
    pub selected_peers: Vec<tg_app_core::stand_in::types::PeerId>,
    /// Last success feedback message.
    pub feedback: Option<String>,
    /// Last error message from `phone.inviteToGroupCall`.
    pub error: Option<String>,
}

impl GroupInviteDialog {
    /// Opens the invite dialog and resets selection/errors.
    pub fn open(&mut self) {
        self.open = true;
        self.search_query.clear();
        self.selected_peers.clear();
        self.feedback = None;
        self.error = None;
    }

    /// Renders the Invite Members modal window when `self.open` is true.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) {
        if !self.open {
            return;
        }
        let mut keep_open = self.open;
        let mut close_requested = false;
        let candidates = backend.group_invite_candidates();
        let query = self.search_query.trim().to_ascii_lowercase();
        let screen = ctx.viewport_rect();
        let win_w = (screen.width() - 24.0).clamp(320.0, 380.0);
        let win_h = (screen.height() - 48.0).clamp(320.0, 460.0);

        egui::Window::new(tr::lng_group_call_invite_title())
            .id(egui::Id::unique("group_call_invite_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(win_w, win_h))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .desired_width(ui.available_width())
                        .hint_text("Search members to invite..."),
                );
                ui.add_space(8.0);

                if let Some(ref err) = self.error {
                    ui.label(
                        RichText::new(err)
                            .size(12.0)
                            .color(palette.group_member_muted_icon),
                    );
                    ui.add_space(4.0);
                }
                if let Some(ref msg) = self.feedback {
                    ui.label(
                        RichText::new(msg)
                            .size(12.0)
                            .color(palette.group_member_active),
                    );
                    ui.add_space(4.0);
                }

                let list_h = (win_h - 140.0).max(120.0);
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .show(ui, |ui| {
                        let mut visible_count = 0usize;
                        for cand in &candidates {
                            if !query.is_empty()
                                && !cand.name.to_ascii_lowercase().contains(&query)
                                && !cand.short_name.to_ascii_lowercase().contains(&query)
                            {
                                continue;
                            }
                            visible_count += 1;
                            let existing = snapshot.members.iter().find(|m| m.peer.id == cand.id);
                            let already_joined = existing.is_some_and(|m| !m.is_invited);
                            let already_invited = existing.is_some_and(|m| m.is_invited);
                            let disabled = already_joined || already_invited;

                            let mut checked = self.selected_peers.contains(&cand.id);
                            ui.horizontal(|ui| {
                                if disabled {
                                    ui.add_enabled(
                                        false,
                                        egui::Checkbox::new(&mut checked, &cand.name),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let badge =
                                                if already_joined { "In call" } else { "Invited" };
                                            ui.label(
                                                RichText::new(badge)
                                                    .size(11.0)
                                                    .color(palette.group_member_sub_fg),
                                            );
                                        },
                                    );
                                } else if ui.checkbox(&mut checked, &cand.name).changed() {
                                    if checked {
                                        if !self.selected_peers.contains(&cand.id) {
                                            self.selected_peers.push(cand.id);
                                        }
                                    } else {
                                        self.selected_peers.retain(|id| *id != cand.id);
                                    }
                                }
                            });
                            ui.add_space(2.0);
                        }
                        if visible_count == 0 {
                            ui.label(
                                RichText::new("No matching members found.")
                                    .size(12.5)
                                    .color(palette.group_member_sub_fg),
                            );
                        }
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let count = self.selected_peers.len();
                    ui.label(
                        RichText::new(if count == 0 {
                            "Select members".to_owned()
                        } else {
                            format!("{count} selected")
                        })
                        .size(12.0)
                        .color(palette.group_member_sub_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let invite_btn_text = tr::lng_group_call_invite_button();
                        if ui
                            .add_enabled(
                                !self.selected_peers.is_empty(),
                                egui::Button::new(
                                    RichText::new(invite_btn_text)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(palette.group_muted_1)
                                .corner_radius(CornerRadius::same(8)),
                            )
                            .clicked()
                        {
                            match backend.invite_to_group_call(&self.selected_peers) {
                                Ok(invited) => {
                                    self.selected_peers.clear();
                                    self.error = None;
                                    self.feedback =
                                        Some(format!("Invited {} member(s)", invited.len()));
                                    close_requested = true;
                                }
                                Err(err) => {
                                    self.error = Some(err);
                                }
                            }
                        }
                        if ui.button(tr::lng_cancel()).clicked() {
                            close_requested = true;
                        }
                    });
                });
            });

        self.open = keep_open && !close_requested;
    }
}
