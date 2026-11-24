//! Calls UI subsystem (1:1 voice/video calls, active call top bar, group calls / voice chats,
//! recent calls box, and join-as peer chooser) built on `tg-calls`, `tg-voip`, and `tg-audio`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/calls/calls_panel.cpp` ([`panel`])
//! - `Telegram/SourceFiles/calls/calls_top_bar.cpp` ([`top_bar`])
//! - `Telegram/SourceFiles/calls/group/calls_group_panel.cpp` ([`group_panel`])
//! - `Telegram/SourceFiles/calls/group/calls_group_members.cpp` & `calls_group_members_row.cpp` ([`group_members`])
//! - `Telegram/SourceFiles/calls/group/calls_group_viewport.cpp` & `calls_group_viewport_tile.cpp` ([`group_viewport`])
//! - `Telegram/SourceFiles/calls/group/calls_group_settings.cpp` ([`group_settings`])
//! - `Telegram/SourceFiles/calls/calls_box_controller.cpp` ([`recent_calls`])
//! - `Telegram/SourceFiles/calls/group/calls_choose_join_as.cpp` ([`choose_join_as`])

pub mod audio;
pub use tg_app_core::calls::backend;
pub mod choose_join_as;
pub mod format;
pub mod group_members;
pub mod group_messages;
pub mod group_panel;
pub mod group_settings;
pub mod group_viewport;
pub mod live_backend;
pub mod mock;
pub mod panel;
pub mod recent_calls;
pub mod screen_picker;
#[cfg(test)]
pub mod screenshot;
pub mod strings;
pub mod theme;
pub mod top_bar;

use std::time::Instant;

use egui::{Key, Modifiers};
use tg_calls::MuteState;

use crate::calls::audio::CallRingtonePlayer;
use crate::calls::backend::{BackendSoundEvent, CallsBackend};
use crate::calls::choose_join_as::{ChooseJoinAsBox, ChooseJoinAsContext};
use crate::calls::group_panel::GroupCallPanelState;
use crate::calls::live_backend::{LiveCallsBackend, LiveCallsConfig};
use crate::calls::mock::MockCallsBackend;
use crate::calls::panel::CallPanelState;
use crate::calls::recent_calls::{RecentCallsAction, RecentCallsBox};
use crate::calls::theme::CallPalette;
use crate::calls::top_bar::{CallsTopBar, TopBarAction};

/// Self-contained Calls UI host wired into [`crate::app::TgforkApp`] with a minimal one-line
/// footprint in `app.rs`.
///
/// Interactive demo shortcuts when the desktop window is running:
/// - `Ctrl+Shift+I`: simulate an incoming 1:1 call (plays original synthesized ringtone)
/// - `Ctrl+Shift+C`: start an outgoing 1:1 call (runs DH key exchange -> emoji fingerprint)
/// - `Ctrl+Shift+G`: open/join a demo group call with video tiles and speaking participants
/// - `Ctrl+Shift+H`: open the Recent Calls list box
/// - `Ctrl+Shift+J`: open the "Join Voice Chat As..." chooser
pub struct AppCallsHost {
    started_at: Instant,
    backend: Box<dyn CallsBackend>,
    ringtone: CallRingtonePlayer,
    palette: CallPalette,
    top_bar: CallsTopBar,
    call_panel: CallPanelState,
    group_panel: GroupCallPanelState,
    recent_calls_box: RecentCallsBox,
    join_as_box: ChooseJoinAsBox,
    last_toast: Option<(String, u64)>,
    demo_shortcuts_enabled: bool,
}

impl Default for AppCallsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AppCallsHost {
    /// Creates a new [`AppCallsHost`] backed by [`MockCallsBackend`] and `tg-audio` (demo mode).
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Box::new(MockCallsBackend::new()), true)
    }

    /// Creates a new [`AppCallsHost`] backed by [`LiveCallsBackend`] wired to real `tg-voip`
    /// [`tg_voip::MediaEngine`] and [`tg_voip::GroupMediaEngine`] pipelines.
    pub fn new_live(config: LiveCallsConfig) -> anyhow::Result<Self> {
        let backend = LiveCallsBackend::new(config)?;
        Ok(Self::with_backend(Box::new(backend), true))
    }

    /// Creates an [`AppCallsHost`] with an injected [`CallsBackend`] (and optional hardware audio).
    #[must_use]
    pub fn with_backend(backend: Box<dyn CallsBackend>, attach_hardware_audio: bool) -> Self {
        Self {
            started_at: Instant::now(),
            backend,
            ringtone: CallRingtonePlayer::new(attach_hardware_audio),
            palette: CallPalette::tdesktop(),
            top_bar: CallsTopBar::default(),
            call_panel: CallPanelState::default(),
            group_panel: GroupCallPanelState::default(),
            recent_calls_box: RecentCallsBox::default(),
            join_as_box: ChooseJoinAsBox::default(),
            last_toast: None,
            demo_shortcuts_enabled: true,
        }
    }

    /// Configures whether mock-peer demo call shortcuts (`Ctrl+Shift+I/C/G/J`) are active.
    ///
    /// `Ctrl+Shift+H` (Recent Calls dialog) remains available in both demo and live modes.
    #[must_use]
    pub const fn with_demo_shortcuts(mut self, enabled: bool) -> Self {
        self.demo_shortcuts_enabled = enabled;
        self
    }

    /// Opens the Recent Calls dialog (`Calls::BoxController`).
    pub fn open_recent_calls(&mut self) {
        self.recent_calls_box.open = true;
        self.backend.refresh_recent_calls();
    }

    /// Handles a `DeepLinkOverlayRequest::JoinCall` deep link (`tg://call?slug=...` or `?voicechat`).
    pub fn handle_join_call_deep_link(
        &mut self,
        peer_id: Option<tg_app_core::stand_in::types::PeerId>,
        slug_or_hash: Option<String>,
    ) {
        let now_ms = self.elapsed_ms();
        if peer_id.is_none()
            && let Some(slug) = slug_or_hash.filter(|s| !s.is_empty())
        {
            self.backend.join_conference_call_by_slug(slug, now_ms);
            self.group_panel.visible = true;
            return;
        }
        if let Some(pid) = peer_id {
            let candidates = self.backend.join_as_candidates(pid);
            let join_as = candidates
                .iter()
                .find(|c| c.is_personal)
                .or_else(|| candidates.first())
                .map_or(MockCallsBackend::PEER_SELF, |c| c.peer.id);
            self.backend.join_group_call(pid, join_as, now_ms);
            self.group_panel.visible = true;
            return;
        }
        self.open_recent_calls();
    }

    /// Closes the Recent Calls dialog.
    pub fn close_recent_calls(&mut self) {
        self.recent_calls_box.open = false;
    }

    /// Returns `true` while the Recent Calls dialog is open.
    #[must_use]
    pub const fn is_recent_calls_open(&self) -> bool {
        self.recent_calls_box.open
    }

    /// Returns a shared reference to the underlying [`CallsBackend`].
    #[must_use]
    pub fn backend(&self) -> &dyn CallsBackend {
        self.backend.as_ref()
    }

    /// Returns a mutable reference to the underlying [`CallsBackend`].
    pub fn backend_mut(&mut self) -> &mut dyn CallsBackend {
        self.backend.as_mut()
    }

    /// Mutable access to the Group Call panel UI state (for tests and screenshots).
    pub fn group_panel_mut(&mut self) -> &mut GroupCallPanelState {
        &mut self.group_panel
    }

    /// Mutable access to the Recent Calls modal box state (for tests and screenshots).
    pub fn recent_calls_box_mut(&mut self) -> &mut RecentCallsBox {
        &mut self.recent_calls_box
    }

    /// Drives one UI frame: ticks backend state machines & synthesized audio, renders the green
    /// top bar over the main window (when in a call), and renders active 1:1 / group call windows
    /// and dialogs.
    pub fn update_and_render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.palette = CallPalette::from_theme(ctx.global_style().visuals.dark_mode);
        let now_ms = self.elapsed_ms();
        self.handle_demo_shortcuts(&ctx, now_ms);

        self.backend.tick(now_ms);
        for ev in self.backend.drain_sound_events() {
            match ev {
                BackendSoundEvent::StartWaitingTrack { outgoing } => {
                    self.ringtone.start_waiting_loop(outgoing, now_ms);
                }
                BackendSoundEvent::StopWaitingTrack => {
                    self.ringtone.stop_waiting_loop();
                }
                BackendSoundEvent::CallSound(sound) => {
                    self.ringtone.play_call_sound(sound);
                }
                BackendSoundEvent::GroupCallSound(sound) => {
                    self.ringtone.play_group_sound(sound);
                }
            }
        }
        self.ringtone.tick(now_ms);

        for toast in self.backend.drain_toasts() {
            self.last_toast = Some((toast, now_ms + 3_500));
        }

        // 1. Active call top bar (`calls_top_bar.cpp`) above the main workspace.
        let top_action = self.top_bar.show(
            ui,
            self.backend.active_call(),
            self.backend.active_group_call(),
            &self.palette,
            now_ms,
        );
        if let Some(act) = top_action {
            match act {
                TopBarAction::ReturnToCall => {
                    if self.backend.active_call().is_some() {
                        self.call_panel.visible = true;
                    } else if self.backend.active_group_call().is_some() {
                        self.group_panel.visible = true;
                    }
                }
                TopBarAction::ToggleMute => {
                    if let Some(call) = self.backend.active_call() {
                        let next = !call.muted;
                        self.backend.set_call_muted(next);
                    } else if let Some(group) = self.backend.active_group_call() {
                        let next = match group.mute_state {
                            MuteState::Active | MuteState::PushToTalk => MuteState::Muted,
                            MuteState::Muted => MuteState::Active,
                            MuteState::ForceMuted | MuteState::RaisedHand => group.mute_state,
                        };
                        self.backend.set_group_mute_state(next, now_ms);
                    }
                }
                TopBarAction::Hangup => {
                    if self.backend.active_call().is_some() {
                        self.backend.hangup_call(now_ms);
                    } else if self.backend.active_group_call().is_some() {
                        self.backend.leave_group_call(false, now_ms);
                    }
                }
            }
        }

        // 2. Floating 1:1 call window (`calls_panel.cpp`).
        let ring_peak = self.ringtone.waiting_sound_peak_value(now_ms);
        self.call_panel.show_window(
            &ctx,
            self.backend.as_mut(),
            &self.palette,
            ring_peak,
            now_ms,
        );

        // 3. Floating Group Call window (`calls_group_panel.cpp`).
        self.group_panel
            .show_window(&ctx, self.backend.as_mut(), &self.palette, now_ms);
        if self.group_panel.request_switch_join_as {
            self.group_panel.request_switch_join_as = false;
            if let Some(group) = self.backend.active_group_call() {
                let group_id = group.group_peer.id;
                let is_broadcast = group.group_peer.is_broadcast;
                let current_join_as = group.join_as;
                let cands = self.backend.join_as_candidates(group_id);
                self.join_as_box.open_for(
                    group_id,
                    is_broadcast,
                    ChooseJoinAsContext::Switch,
                    cands,
                    current_join_as,
                );
            }
        }

        // 4. Recent Calls box (`calls_box_controller.cpp`).
        if let Some(act) = self
            .recent_calls_box
            .show(&ctx, self.backend.as_ref(), &self.palette)
        {
            match act {
                RecentCallsAction::CallBack { peer_id, video } => {
                    self.recent_calls_box.open = false;
                    self.backend.start_outgoing_call(peer_id, video, now_ms);
                    self.backend.confirm_outgoing_call(video, now_ms);
                    if let Some(c) = self.backend.active_call() {
                        self.call_panel.reset_for_call(c, now_ms);
                    }
                }
                RecentCallsAction::Delete(ids) => {
                    self.backend.delete_recent_calls(&ids);
                }
                RecentCallsAction::JoinGroupCall(group_peer_id) => {
                    self.recent_calls_box.open = false;
                    let cands = self.backend.join_as_candidates(group_peer_id);
                    let initial = cands
                        .first()
                        .map_or(MockCallsBackend::PEER_SELF, |c| c.peer.id);
                    self.join_as_box.open_for(
                        group_peer_id,
                        false,
                        ChooseJoinAsContext::Join,
                        cands,
                        initial,
                    );
                }
                RecentCallsAction::CreateConferenceCall => {
                    self.recent_calls_box.open = false;
                    self.backend.create_conference_call(now_ms);
                    self.group_panel.visible = true;
                }
                RecentCallsAction::ClearAll { revoke } => {
                    match self.backend.delete_phone_call_history(revoke) {
                        Ok(count) => {
                            self.last_toast =
                                Some((format!("Cleared {count} call log entries"), now_ms + 3_500));
                        }
                        Err(err) => {
                            self.last_toast = Some((err, now_ms + 3_500));
                        }
                    }
                }
            }
        }

        // 5. Join-As peer chooser (`calls_choose_join_as.cpp`).
        if let Some(confirmed) = self.join_as_box.show(&ctx, &self.palette) {
            let _ = self
                .backend
                .save_default_group_call_join_as(confirmed.group_peer_id, confirmed.join_as);
            match confirmed.context {
                ChooseJoinAsContext::Switch => {
                    self.backend.switch_group_join_as(confirmed.join_as, now_ms);
                }
                ChooseJoinAsContext::Create | ChooseJoinAsContext::Join => {
                    self.backend.join_group_call(
                        confirmed.group_peer_id,
                        confirmed.join_as,
                        now_ms,
                    );
                    self.group_panel.visible = true;
                }
            }
        }

        // 6. Transient bottom toast notification.
        if let Some((ref msg, until)) = self.last_toast {
            if now_ms < until {
                egui::Area::new(egui::Id::unique("calls_toast_area"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::new(0.0, -24.0))
                    .show(&ctx, |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(20, 24, 29, 232))
                            .corner_radius(egui::CornerRadius::same(16))
                            .inner_margin(egui::Margin::symmetric(16, 8))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(msg)
                                        .color(egui::Color32::WHITE)
                                        .size(12.5),
                                );
                            });
                    });
            } else {
                self.last_toast = None;
            }
        }

        // Keep animating smoothly while any call or ringtone is active.
        if self.backend.active_call().is_some() || self.backend.active_group_call().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    /// Stops any active ringtone loop cleanly on exit.
    pub fn on_exit(&mut self) {
        self.ringtone.stop_waiting_loop();
        self.ringtone.detach_if_idle();
    }

    fn handle_demo_shortcuts(&mut self, ctx: &egui::Context, now_ms: u64) {
        let chord = |key: Key| {
            ctx.input(|i| {
                i.modifiers
                    .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                    && i.key_pressed(key)
            })
        };
        if chord(Key::H) {
            self.recent_calls_box.open = !self.recent_calls_box.open;
            if self.recent_calls_box.open {
                self.backend.refresh_recent_calls();
            }
            return;
        }
        if !self.demo_shortcuts_enabled {
            return;
        }
        if chord(Key::I) {
            self.backend.simulate_incoming_call(
                MockCallsBackend::PEER_DESIGN_STUDIO,
                false,
                now_ms,
            );
            if let Some(c) = self.backend.active_call() {
                self.call_panel.reset_for_call(c, now_ms);
            }
        } else if chord(Key::C) {
            self.backend
                .start_outgoing_call(MockCallsBackend::PEER_AUDIO_LAB, false, now_ms);
            self.backend.confirm_outgoing_call(false, now_ms);
            if let Some(c) = self.backend.active_call() {
                self.call_panel.reset_for_call(c, now_ms);
            }
        } else if chord(Key::G) {
            self.backend.join_group_call(
                MockCallsBackend::PEER_CORE_SYNC,
                MockCallsBackend::PEER_SELF,
                now_ms,
            );
            self.group_panel.visible = true;
        } else if chord(Key::J) {
            let group_id = MockCallsBackend::PEER_CORE_SYNC;
            let cands = self.backend.join_as_candidates(group_id);
            self.join_as_box.open_for(
                group_id,
                false,
                ChooseJoinAsContext::Join,
                cands,
                MockCallsBackend::PEER_SELF,
            );
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {

    use egui::{Pos2, Rect, Vec2};
    use tg_calls::group::{SOUND_STATUS_KEPT_FOR_MS, SPEAK_LEVEL_THRESHOLD};
    use tg_calls::{
        CallSound, CallState, GroupCallState, InstanceState, VideoEndpoint, VideoEndpointType,
        VideoQuality,
    };

    use crate::calls::audio::{CallRingtonePlayer, WaitingToneKind};
    use crate::calls::backend::{
        CallsBackend, RecentCallDirection, RecentCallEntry, to_calls_peer_id,
    };
    use crate::calls::format::{
        call_status_text, emoji_fingerprint_tooltip, format_duration_text, format_ptt_delay,
        format_recent_call_status, format_volume_percent, ms_until_next_second, ptt_delay_by_index,
    };
    use crate::calls::group_members::{
        BLOBS_ENTER_DURATION_MS, RAISED_HAND_STATUS_DURATION_MS, SpeakingIndicatorState,
        USERPIC_MIN_SCALE,
    };
    use crate::calls::group_panel::GroupCallPanelState;
    use crate::calls::group_settings::GroupSettingsDialog;
    use crate::calls::group_viewport::{
        TileRect, VideoTileInput, apply_large_layout, compute_column_layout, compute_narrow_layout,
        compute_tile_video_quality, compute_wide_layout, interpolate_rect,
    };
    use crate::calls::mock::MockCallsBackend;
    use crate::calls::panel::CallPanelState;
    use crate::calls::recent_calls::group_recent_calls;
    use crate::calls::screenshot::{render_egui_offscreen, save_rgba_png};
    use crate::calls::theme::{CALL_BAR_HEIGHT, CallPalette};
    use crate::calls::top_bar::{CallsTopBar, TopBarState, group_bar_state};
    use tg_app_core::stand_in::types::PeerId;

    // -------------------------------------------------------------------------
    // 1. Timer text & status formatting view-model tests
    // -------------------------------------------------------------------------

    #[test]
    fn timer_text_matches_tdesktop_format_duration_text() {
        assert_eq!(format_duration_text(-5), "00:00");
        assert_eq!(format_duration_text(0), "00:00");
        assert_eq!(format_duration_text(7), "00:07");
        assert_eq!(format_duration_text(65), "01:05");
        assert_eq!(format_duration_text(599), "09:59");
        assert_eq!(format_duration_text(3599), "59:59");
        assert_eq!(format_duration_text(3600), "1:00:00");
        assert_eq!(format_duration_text(3661), "1:01:01");
        assert_eq!(format_duration_text(36_000), "10:00:00");

        assert_eq!(ms_until_next_second(0), 1005);
        assert_eq!(ms_until_next_second(250), 755);
        assert_eq!(ms_until_next_second(999), 6);
    }

    #[test]
    fn call_status_text_covers_every_state_and_rating_override() {
        assert_eq!(
            call_status_text(CallState::Requesting, 0, false, None),
            tg_lang::tr::lng_call_status_requesting()
        );
        assert_eq!(
            call_status_text(CallState::Waiting, 0, false, None),
            tg_lang::tr::lng_call_status_waiting()
        );
        assert_eq!(
            call_status_text(CallState::Ringing, 0, false, None),
            tg_lang::tr::lng_call_status_ringing()
        );
        assert_eq!(
            call_status_text(CallState::WaitingIncoming, 0, false, None),
            tg_lang::tr::lng_call_status_incoming()
        );
        assert_eq!(
            call_status_text(CallState::WaitingIncoming, 0, true, None),
            tg_lang::tr::lng_call_status_group_invite()
        );
        assert_eq!(
            call_status_text(CallState::ExchangingKeys, 0, false, None),
            tg_lang::tr::lng_call_status_exchanging()
        );
        assert_eq!(
            call_status_text(CallState::WaitingInit, 0, false, None),
            tg_lang::tr::lng_call_status_connecting()
        );
        assert_eq!(
            call_status_text(CallState::Established, 165_400, false, None),
            "02:45"
        );
        assert_eq!(
            call_status_text(CallState::Busy, 0, false, None),
            tg_lang::tr::lng_call_status_busy()
        );
        assert_eq!(
            call_status_text(CallState::Ended, 0, false, Some(182)),
            "03:02"
        );
        assert!(emoji_fingerprint_tooltip("Design Studio").contains("Design Studio"));
    }

    #[test]
    fn ptt_delay_and_volume_and_recent_call_formatting() {
        assert_eq!(ptt_delay_by_index(0), 0);
        assert_eq!(ptt_delay_by_index(25), 250);
        assert_eq!(ptt_delay_by_index(200), 2000);
        assert_eq!(ptt_delay_by_index(999), 2000);

        assert_eq!(format_ptt_delay(0), "0 ms");
        assert_eq!(format_ptt_delay(250), "250 ms");
        assert_eq!(format_ptt_delay(990), "990 ms");
        assert_eq!(format_ptt_delay(1000), "1.00 s");
        assert_eq!(format_ptt_delay(1750), "1.75 s");

        assert_eq!(format_volume_percent(0), "0%");
        assert_eq!(format_volume_percent(10_000), "100%");
        assert_eq!(format_volume_percent(20_000), "200%");

        assert_eq!(
            format_recent_call_status(0, 14, 5, 3, Some(125)),
            "(3) 14:05, 02:05"
        );
        assert_eq!(
            format_recent_call_status(1, 9, 30, 1, None),
            "yesterday at 09:30"
        );
    }

    // -------------------------------------------------------------------------
    // 2. Video tile grid layout view-model tests (`calls_group_viewport`)
    // -------------------------------------------------------------------------

    fn make_tile(id: u64, kind: VideoEndpointType, w: i32, h: i32) -> VideoTileInput {
        let ep = VideoEndpoint::new(
            kind,
            to_calls_peer_id(PeerId::from_user(id)),
            format!("ep-{id}"),
        );
        VideoTileInput {
            endpoint: ep,
            frame_size: (w, h),
            paused: false,
        }
    }

    #[test]
    fn wide_tile_layout_minimizes_letterbox_and_handles_spotlight() {
        // Single tile occupies full viewport.
        let one = compute_wide_layout(
            &[make_tile(1, VideoEndpointType::Camera, 1280, 720)],
            800,
            600,
        );
        assert_eq!(one.rect_for(0), Some(TileRect::new(0, 0, 800, 600)));

        // 3 landscape tiles in a wide 900x500 viewport: verify non-overlapping bounds inside outer.
        let three_tiles = vec![
            make_tile(1, VideoEndpointType::Screen, 1920, 1080),
            make_tile(2, VideoEndpointType::Camera, 1280, 720),
            make_tile(3, VideoEndpointType::Camera, 1280, 720),
        ];
        let layout = compute_wide_layout(&three_tiles, 900, 500);
        assert_eq!(layout.list.len(), 3);
        for i in 0..3 {
            let r = layout.rect_for(i).expect("tile rect present");
            assert!(r.x >= 0 && r.y >= 0);
            assert!(r.width > 0 && r.height > 0);
            assert!(r.right() <= 900 && r.bottom() <= 500);
        }

        // Spotlight (`apply_large_layout`) expands tile 0 to full viewport and pushes others off-screen.
        let large_ep = three_tiles[0].endpoint.clone();
        let expanded = apply_large_layout(layout.clone(), Some(&large_ep));
        assert_eq!(expanded.rect_for(0), Some(TileRect::new(0, 0, 900, 500)));
        // Midpoint interpolation between grid and spotlight stays smoothly between rectangles.
        let mid = interpolate_rect(
            layout.rect_for(0).expect("grid"),
            expanded.rect_for(0).expect("expanded"),
            0.5,
        );
        assert!(mid.width > layout.rect_for(0).unwrap().width);
        assert!(mid.width < 900);
    }

    #[test]
    fn narrow_and_column_layouts_and_quality_tiers_match_tdesktop() {
        let tiles = vec![
            make_tile(1, VideoEndpointType::Screen, 1920, 1080),
            make_tile(2, VideoEndpointType::Camera, 1280, 720),
            make_tile(3, VideoEndpointType::Camera, 1280, 720),
        ];
        // 3 tiles in narrow 348px layout: top tile spans full width, bottom 2 share row 1.
        let (placed, total_h) = compute_narrow_layout(&tiles, 348, 0);
        assert_eq!(placed.len(), 3);
        assert_eq!(placed[0].rect.x, 0);
        assert_eq!(placed[0].rect.width, 348);
        assert_eq!(placed[1].rect.y, placed[2].rect.y);
        assert!(placed[1].rect.width < 200);
        assert!(placed[2].rect.x > placed[1].rect.right());
        assert!(total_h > placed[2].rect.bottom());
        // Multi-tile narrow forces Thumbnail quality (`calls_group_viewport.cpp:864-866`).
        assert_eq!(placed[0].quality, VideoQuality::Thumbnail);

        // Column layout hides the large tile and stacks the remaining tiles vertically at 120px.
        let (col, col_h) = compute_column_layout(&tiles, Some(&tiles[0].endpoint), 200, 0);
        assert_eq!(col.len(), 2);
        assert_eq!(col[0].rect.height, 120);
        assert_eq!(col[1].rect.y, 124);
        assert_eq!(col_h, 248);

        // Quality thresholds: Full (>= 540 or large/stream), Medium (>= 240), Thumbnail (< 240).
        assert_eq!(
            compute_tile_video_quality(TileRect::new(0, 0, 600, 540), true, false, false, 2),
            VideoQuality::Full
        );
        assert_eq!(
            compute_tile_video_quality(TileRect::new(0, 0, 400, 260), true, false, false, 2),
            VideoQuality::Medium
        );
        assert_eq!(
            compute_tile_video_quality(TileRect::new(0, 0, 200, 160), true, false, false, 2),
            VideoQuality::Thumbnail
        );
        assert_eq!(
            compute_tile_video_quality(TileRect::new(0, 0, 160, 120), true, false, true, 2),
            VideoQuality::Full
        );
    }

    // -------------------------------------------------------------------------
    // 3. Speaking indicator timing & blob animation tests (`calls_group_members_row`)
    // -------------------------------------------------------------------------

    #[test]
    fn speaking_indicator_timing_respects_1500ms_hold_and_250ms_fade() {
        let mut ind = SpeakingIndicatorState::new();
        assert!(!ind.sounding);
        assert_eq!(ind.userpic_scale(), 1.0);

        // Below threshold does not trigger sounding.
        ind.update_level(SPEAK_LEVEL_THRESHOLD - 0.05, false, 1_000);
        ind.tick(1_050);
        assert!(!ind.sounding);

        // Above threshold triggers sounding + speaking and ramps `enter` up over 250 ms.
        ind.update_level(0.75, true, 2_000);
        ind.tick(2_125);
        assert!(ind.sounding);
        assert!(ind.speaking);
        assert!((ind.enter - 0.5).abs() < 0.05);
        assert!(ind.userpic_scale() >= USERPIC_MIN_SCALE && ind.userpic_scale() <= 1.0);

        ind.tick(2_250);
        assert!((ind.enter - 1.0).abs() < 0.01);

        // Still fully entered just before the final 250 ms fade window (2000 + 1500 - 250 = 3250).
        ind.update_level(0.0, false, 3_200);
        ind.tick(3_200);
        assert!(ind.sounding);
        assert!((ind.enter - 1.0).abs() < 0.01);

        // Halfway through the final 250 ms fade window (3375 ms -> enter ~ 0.5).
        ind.tick(2_000 + SOUND_STATUS_KEPT_FOR_MS - BLOBS_ENTER_DURATION_MS / 2);
        assert!(ind.sounding);
        assert!((ind.enter - 0.5).abs() < 0.05);

        // At 2000 + 1500 ms -> sounding & speaking expire completely.
        ind.tick(2_000 + SOUND_STATUS_KEPT_FOR_MS);
        assert!(!ind.sounding);
        assert!(!ind.speaking);
        assert_eq!(ind.enter, 0.0);
        assert_eq!(ind.userpic_scale(), 1.0);

        // Raised hand status lasts exactly RAISED_HAND_STATUS_DURATION_MS (3000 ms).
        ind.trigger_raised_hand(5_000);
        assert!(ind.show_raised_hand_status(7_999));
        assert!(!ind.show_raised_hand_status(5_000 + RAISED_HAND_STATUS_DURATION_MS));
    }

    // -------------------------------------------------------------------------
    // 4. Mock backend state progression, recent calls grouping, ringtone & top bar
    // -------------------------------------------------------------------------

    #[test]
    fn mock_backend_progresses_incoming_outgoing_rating_and_group_calls() {
        let mut backend = MockCallsBackend::new();
        let mut player = CallRingtonePlayer::headless();

        // Incoming call starts ringing -> plays synthesized ringtone -> Answer -> Established with 4 emoji.
        backend.simulate_incoming_call(MockCallsBackend::PEER_DESIGN_STUDIO, false, 1_000);
        backend.tick(1_200);
        assert_eq!(
            backend.active_call().unwrap().state,
            CallState::WaitingIncoming
        );
        for ev in backend.drain_sound_events() {
            if let crate::calls::backend::BackendSoundEvent::StartWaitingTrack { outgoing } = ev {
                player.start_waiting_loop(outgoing, 1_200);
            }
        }
        assert_eq!(player.active_loop(), Some(WaitingToneKind::Incoming));
        assert!(player.waiting_sound_peak_value(1_400) > 0.0);

        backend.answer_call(2_000);
        for t in [2_300_u64, 2_600, 2_900] {
            backend.tick(t);
        }
        let call = backend.active_call().expect("active call");
        assert_eq!(call.state, CallState::Established);
        assert!(call.emoji_fingerprint.is_some());
        assert_eq!(call.signal_bars.raw_count(), 4);

        // Hanging up an established call transitions into post-call rating card.
        backend.hangup_call(12_000);
        let rating_call = backend.active_call().expect("rating call");
        assert!(rating_call.rating_requested);
        backend.submit_call_rating(5, String::new());
        backend.finish_call_rating();
        assert!(backend.active_call().is_none());

        // Group call joins cleanly and tracks synthetic audio levels.
        backend.join_group_call(
            MockCallsBackend::PEER_CORE_SYNC,
            MockCallsBackend::PEER_SELF,
            20_000,
        );
        backend.tick(20_500);
        let grp = backend.active_group_call().expect("group call");
        assert_eq!(grp.state, GroupCallState::Joined);
        assert_eq!(grp.instance_state, InstanceState::Connected);
        assert!(grp.joined_count() >= 5);
        assert_eq!(
            group_bar_state(grp.mute_state, grp.instance_state),
            TopBarState::Muted
        );
        assert_eq!(
            GroupSettingsDialog::format_captured_shortcut(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::Space
            ),
            "Ctrl + Shift + Space"
        );

        // Recent calls grouping collapses consecutive same-peer + same-direction + same-day rows.
        let grouped = group_recent_calls(backend.recent_calls());
        assert!(grouped.len() < backend.recent_calls().len());
        let deleted_ids = grouped[0].msg_ids.clone();
        let before = backend.recent_calls().len();
        backend.delete_recent_calls(&deleted_ids);
        assert_eq!(backend.recent_calls().len(), before - deleted_ids.len());

        // Headless ringtone plays one-shot CallSound cues cleanly.
        player.play_call_sound(CallSound::Ended);
        player.stop_waiting_loop();
        assert!(player.active_loop().is_none());

        // Keep unused import check satisfied on RecentCallEntry.
        let _sample_dir = RecentCallDirection::Out;
        let _ = std::mem::size_of::<RecentCallEntry>();
    }

    // -------------------------------------------------------------------------
    // 5. Offscreen egui screenshot generation (`crates/tg-app/docs/calls*.png`)
    // -------------------------------------------------------------------------

    #[test]
    fn generate_calls_ui_screenshots() {
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let palette = CallPalette::tdesktop();

        // 1) Incoming call screenshot (`calls_incoming.png`)
        {
            let mut backend = MockCallsBackend::new();
            let now_ms = 2_400;
            backend.setup_demo_incoming_call(MockCallsBackend::PEER_DESIGN_STUDIO, false, now_ms);
            let call = backend.active_call().expect("incoming call").clone();
            let mut panel = CallPanelState::default();
            panel.reset_for_call(&call, now_ms);

            let img = render_egui_offscreen(720, 540, palette.call_bg, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 540.0));
                        panel.paint_panel_contents(
                            ui,
                            rect,
                            &call,
                            &mut backend,
                            &palette,
                            0.78,
                            now_ms,
                        );
                    });
            });
            let path = docs_dir.join("calls_incoming.png");
            save_rgba_png(&path, &img).expect("save calls_incoming.png");
            assert!(path.exists());
        }

        // 2) Active 1:1 call with emoji verification key, tooltip, signal bars & green top bar (`calls_active.png`)
        {
            let mut backend = MockCallsBackend::new();
            let now_ms = 200_000;
            backend.setup_demo_active_call(MockCallsBackend::PEER_AUDIO_LAB, 165, now_ms);
            let call = backend.active_call().expect("active call").clone();
            assert!(call.emoji_fingerprint.is_some());

            let mut top_bar = CallsTopBar::default();
            let mut panel = CallPanelState::default();
            panel.reset_for_call(&call, now_ms);
            panel.hangup_progress = 1.0;
            panel.fingerprint_tooltip_pinned = true;

            let img = render_egui_offscreen(720, 578, palette.call_bg, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let bar_rect =
                            Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, CALL_BAR_HEIGHT));
                        let _ = top_bar.paint_1to1_bar(ui, bar_rect, &call, &palette);
                        let panel_rect = Rect::from_min_size(
                            Pos2::new(0.0, CALL_BAR_HEIGHT),
                            Vec2::new(720.0, 540.0),
                        );
                        panel.paint_panel_contents(
                            ui,
                            panel_rect,
                            &call,
                            &mut backend,
                            &palette,
                            0.0,
                            now_ms,
                        );
                    });
            });
            let path = docs_dir.join("calls_active.png");
            save_rgba_png(&path, &img).expect("save calls_active.png");
            assert!(path.exists());
        }

        // 3) Group call wide mode with video tiles grid, speaking indicators, and dock (`calls_group.png`)
        {
            let mut backend = MockCallsBackend::new();
            let now_ms = 42_500;
            backend.setup_demo_group_call(now_ms);
            let group = backend.active_group_call().expect("group call").clone();
            let mut panel = GroupCallPanelState::default();
            // Prime speaking animations so blobs are clearly visible in the screenshot.
            panel.members_panel.sync_from_snapshot(&group, now_ms - 300);
            panel.members_panel.sync_from_snapshot(&group, now_ms);

            let img = render_egui_offscreen(960, 580, palette.group_bg, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 580.0));
                        panel.paint_panel_contents(
                            ui,
                            rect,
                            &group,
                            &mut backend,
                            &palette,
                            now_ms,
                        );
                    });
            });
            let path = docs_dir.join("calls_group.png");
            save_rgba_png(&path, &img).expect("save calls_group.png");
            assert!(path.exists());
        }
    }
}
