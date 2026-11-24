//! Group call settings dialog: audio/camera devices, live mic level meter, noise suppression,
//! Push-to-Talk with interactive key capture & delay slider, and invite link sharing.
//!
//! Port of `Telegram/SourceFiles/calls/group/calls_group_settings.cpp`.

use egui::{Color32, CornerRadius, Key, Modifiers, Pos2, Rect, RichText, Ui, Vec2};
use tg_lang::tr;
use tg_ui::tg_style::st;

use crate::calls::backend::{CallsBackend, GroupCallSnapshot, RtmpStreamInfo};
use crate::calls::format::{PTT_DELAY_STEPS, format_ptt_delay, ptt_delay_by_index};
use crate::calls::strings;
use crate::calls::theme::CallPalette;
use tg_app_core::stand_in::types::PeerId;

/// Number of vertical bars in the microphone test meter (`st::groupCallLevelMeter.lineCount`).
pub const LEVEL_METER_BAR_COUNT: usize = st::group_call_level_meter.line_count as usize;

/// Maximum recording title length matching `kMaxTitleLength = 40` in `calls_group_recording_box.cpp`.
pub const RECORDING_TITLE_MAX_LEN: usize = 40;

/// Transient UI state for [`GroupSettingsDialog`].
#[derive(Debug, Default)]
pub struct GroupSettingsDialog {
    /// Whether the settings dialog is currently open.
    pub open: bool,
    /// True while capturing a new Push-to-Talk keyboard shortcut.
    pub recording_ptt_shortcut: bool,
    /// Share invite link as speaker (`true`) or listener (`false`).
    pub share_speaker_link: bool,
    /// Confirmation banner after copying the invite link.
    pub copied_feedback: Option<String>,
    /// Most recently exported invite link returned by `phone.exportGroupCallInvite`.
    pub exported_link: Option<String>,
    /// Inline error from `phone.exportGroupCallInvite`.
    pub export_error: Option<String>,
    /// Set to `true` when the admin clicks "Start/Stop Recording..." in settings.
    pub request_recording_box: bool,
    /// Set to `true` when the admin clicks "Stream With (RTMP)..." in settings.
    pub request_rtmp_box: bool,
    /// Set to `true` when the user clicks "Invite Members..." in settings.
    pub request_invite_box: bool,
}

impl GroupSettingsDialog {
    /// Creates a new closed [`GroupSettingsDialog`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: false,
            recording_ptt_shortcut: false,
            share_speaker_link: true,
            copied_feedback: None,
            exported_link: None,
            export_error: None,
            request_recording_box: false,
            request_rtmp_box: false,
            request_invite_box: false,
        }
    }

    /// Opens the settings dialog.
    pub fn open(&mut self) {
        self.open = true;
        self.recording_ptt_shortcut = false;
        self.copied_feedback = None;
        self.export_error = None;
    }

    /// Formats a captured key combination cleanly (`Ctrl + Shift + V`, `Space`, etc.).
    #[must_use]
    pub fn format_captured_shortcut(modifiers: Modifiers, key: Key) -> String {
        let mut parts = Vec::new();
        if modifiers.ctrl {
            parts.push("Ctrl");
        }
        if modifiers.alt {
            parts.push("Alt");
        }
        if modifiers.shift {
            parts.push("Shift");
        }
        if modifiers.mac_cmd || modifiers.command && !modifiers.ctrl {
            parts.push("Cmd");
        }
        parts.push(key.name());
        parts.join(" + ")
    }

    /// Renders the Group Call Settings modal window when `self.open` is true.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
        now_ms: u64,
    ) {
        if !self.open {
            return;
        }

        // Capture a keyboard shortcut when recording Push-to-Talk key binding.
        if self.recording_ptt_shortcut {
            let captured = ctx.input(|input| {
                for ev in &input.events {
                    if let egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } = ev
                    {
                        return Some((*modifiers, *key));
                    }
                }
                None
            });
            if let Some((mods, key)) = captured {
                if key == Key::Escape {
                    self.recording_ptt_shortcut = false;
                } else {
                    let shortcut = Self::format_captured_shortcut(mods, key);
                    let cur = backend.audio_settings().clone();
                    backend.set_push_to_talk_settings(true, shortcut, cur.push_to_talk_delay_ms);
                    self.recording_ptt_shortcut = false;
                }
            }
        }

        let mut keep_open = self.open;
        let screen = ctx.viewport_rect();
        let win_w = (screen.width() - 24.0).clamp(320.0, 388.0);
        let win_h = (screen.height() - 48.0).clamp(340.0, 560.0);
        egui::Window::new(tr::lng_group_call_settings_title())
            .id(egui::Id::unique("group_call_settings_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(win_w, win_h))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Admin permissions toggles (when user can manage group call).
                    if snapshot.can_manage {
                        section_heading(ui, strings::PERMISSIONS_HEADING, palette);
                        let mut join_muted = snapshot.join_muted;
                        if ui
                            .checkbox(&mut join_muted, tr::lng_group_call_new_muted())
                            .changed()
                        {
                            backend.set_group_join_muted(join_muted);
                        }
                        let mut msgs_enabled = snapshot.messages_enabled;
                        if ui
                            .checkbox(&mut msgs_enabled, tr::lng_group_call_enable_messages())
                            .changed()
                        {
                            backend.set_group_messages_enabled(msgs_enabled);
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let rec_label = if snapshot.recording {
                                "Stop Recording..."
                            } else {
                                "Start Recording..."
                            };
                            if ui.button(rec_label).clicked() {
                                self.request_recording_box = true;
                            }
                            if ui.button("Stream With (RTMP)...").clicked() {
                                self.request_rtmp_box = true;
                            }
                        });
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                    }

                    // Audio & Camera Devices.
                    section_heading(ui, strings::AUDIO_VIDEO_DEVICES_HEADING, palette);
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

                    let default_dev = tr::lng_settings_call_device_default();
                    let mut selected_output = settings.output_device_id.clone();
                    let selected_output_name = outputs
                        .iter()
                        .find(|(id, _)| *id == selected_output)
                        .map_or(default_dev.as_str(), |(_, n)| n.as_str());
                    egui::ComboBox::from_label(tr::lng_group_call_speakers())
                        .width(220.0)
                        .selected_text(selected_output_name)
                        .show_ui(ui, |ui| {
                            for (id, name) in &outputs {
                                ui.selectable_value(&mut selected_output, id.clone(), name);
                            }
                        });

                    let mut selected_input = settings.input_device_id.clone();
                    let selected_input_name = inputs
                        .iter()
                        .find(|(id, _)| *id == selected_input)
                        .map_or(default_dev.as_str(), |(_, n)| n.as_str());
                    egui::ComboBox::from_label(tr::lng_group_call_microphone())
                        .width(220.0)
                        .selected_text(selected_input_name)
                        .show_ui(ui, |ui| {
                            for (id, name) in &inputs {
                                ui.selectable_value(&mut selected_input, id.clone(), name);
                            }
                        });

                    if selected_input != settings.input_device_id
                        || selected_output != settings.output_device_id
                    {
                        backend.set_audio_devices(selected_input, selected_output);
                    }

                    // Live 44-bar microphone level meter (`groupCallLevelMeter`).
                    ui.add_space(4.0);
                    let mic_level = (snapshot.local_audio_level
                        + 0.12 * ((now_ms as f32 * 0.009).sin().abs()))
                    .clamp(0.05, 1.0);
                    paint_level_meter(ui, mic_level, palette);
                    ui.add_space(6.0);

                    let mut selected_camera = settings.camera_device_id.clone();
                    let selected_cam_name = cameras
                        .iter()
                        .find(|(id, _)| *id == selected_camera)
                        .map_or(default_dev.as_str(), |(_, n)| n.as_str());
                    egui::ComboBox::from_label(tr::lng_settings_call_camera())
                        .width(220.0)
                        .selected_text(selected_cam_name)
                        .show_ui(ui, |ui| {
                            for (id, name) in &cameras {
                                ui.selectable_value(&mut selected_camera, id.clone(), name);
                            }
                        });
                    if selected_camera != settings.camera_device_id {
                        backend.set_camera_device(selected_camera);
                    }

                    ui.add_space(8.0);
                    let mut noise_suppression = settings.noise_suppression;
                    if ui
                        .checkbox(
                            &mut noise_suppression,
                            tr::lng_group_call_noise_suppression(),
                        )
                        .changed()
                    {
                        backend.set_noise_suppression(noise_suppression);
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Push-to-Talk section with interactive key capture and release delay slider.
                    section_heading(ui, &tr::lng_group_call_push_to_talk(), palette);
                    let mut ptt_enabled = settings.push_to_talk_enabled;
                    if ui
                        .checkbox(&mut ptt_enabled, strings::ENABLE_PTT_CHECKBOX)
                        .changed()
                    {
                        if !ptt_enabled {
                            self.recording_ptt_shortcut = false;
                        }
                        backend.set_push_to_talk_settings(
                            ptt_enabled,
                            settings.push_to_talk_shortcut.clone(),
                            settings.push_to_talk_delay_ms,
                        );
                    }

                    if ptt_enabled {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(strings::SHORTCUT_LABEL)
                                    .color(palette.group_members_fg)
                                    .size(12.5),
                            );
                            let btn_label = if self.recording_ptt_shortcut {
                                strings::PRESS_SHORTCUT_HINT.to_owned()
                            } else if settings.push_to_talk_shortcut.is_empty() {
                                strings::CLICK_TO_SET_SHORTCUT.to_owned()
                            } else {
                                settings.push_to_talk_shortcut.clone()
                            };
                            let text_color = if self.recording_ptt_shortcut {
                                palette.group_member_muted_icon
                            } else {
                                palette.group_active_fg
                            };
                            if ui
                                .button(RichText::new(btn_label).color(text_color))
                                .clicked()
                            {
                                self.recording_ptt_shortcut = !self.recording_ptt_shortcut;
                            }
                        });

                        ui.add_space(4.0);
                        let mut delay_idx = ((settings.push_to_talk_delay_ms / 10) as usize)
                            .min(PTT_DELAY_STEPS - 1);
                        let delay_label = tr::lng_group_call_ptt_delay(&format_ptt_delay(
                            ptt_delay_by_index(delay_idx),
                        ));
                        ui.label(
                            RichText::new(delay_label)
                                .color(palette.group_member_sub_fg)
                                .size(12.0),
                        );
                        if ui
                            .add(
                                egui::Slider::new(&mut delay_idx, 0..=(PTT_DELAY_STEPS - 1))
                                    .show_value(false),
                            )
                            .changed()
                        {
                            backend.set_push_to_talk_settings(
                                true,
                                settings.push_to_talk_shortcut.clone(),
                                ptt_delay_by_index(delay_idx),
                            );
                        }
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Invite link sharing & member invitation (`ShareInviteLinkBox`, `calls_group_invite_controller.cpp`).
                    section_heading(ui, strings::INVITE_LINK_HEADING, palette);
                    ui.checkbox(
                        &mut self.share_speaker_link,
                        tr::lng_group_call_share_speaker(),
                    );
                    let default_link =
                        if self.share_speaker_link && !snapshot.invite_link_speaker.is_empty() {
                            snapshot.invite_link_speaker.as_str()
                        } else {
                            snapshot.invite_link_listener.as_str()
                        };
                    let link = self
                        .exported_link
                        .as_deref()
                        .unwrap_or(default_link)
                        .to_owned();
                    if !link.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&link)
                                    .monospace()
                                    .size(11.0)
                                    .color(palette.group_active_fg),
                            )
                            .wrap(),
                        );
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let copy_label = if self.share_speaker_link {
                            tr::lng_group_call_copy_speaker_link()
                        } else {
                            tr::lng_group_call_copy_listener_link()
                        };
                        if ui.button(copy_label).clicked() {
                            ctx.copy_text(link.to_owned());
                            self.copied_feedback = Some(tr::lng_create_channel_link_copied());
                            self.export_error = None;
                        }
                        if ui.button("Export Link").clicked() {
                            match backend.export_group_call_invite(self.share_speaker_link) {
                                Ok(new_link) => {
                                    ctx.copy_text(new_link.clone());
                                    self.exported_link = Some(new_link);
                                    self.copied_feedback =
                                        Some(tr::lng_create_channel_link_copied());
                                    self.export_error = None;
                                }
                                Err(err) => {
                                    self.export_error = Some(err);
                                }
                            }
                        }
                        if ui.button(tr::lng_group_call_invite()).clicked() {
                            self.request_invite_box = true;
                        }
                    });
                    if let Some(msg) = &self.copied_feedback {
                        ui.label(
                            RichText::new(msg)
                                .size(11.5)
                                .color(palette.group_member_active),
                        );
                    }
                    if let Some(err) = &self.export_error {
                        ui.label(
                            RichText::new(err)
                                .size(11.5)
                                .color(palette.group_member_muted_icon),
                        );
                    }
                });
            });

        self.open = keep_open;
    }
}

fn section_heading(ui: &mut Ui, title: &str, palette: &CallPalette) {
    ui.label(
        RichText::new(title)
            .strong()
            .size(13.0)
            .color(palette.group_active_fg),
    );
    ui.add_space(4.0);
}

fn paint_level_meter(ui: &mut Ui, level: f32, palette: &CallPalette) {
    let desired = Vec2::new(ui.available_width(), 18.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();
    let active_bars = ((level.clamp(0.0, 1.0)) * (LEVEL_METER_BAR_COUNT as f32)).round() as usize;
    let bar_w = 3.0_f32;
    let total_bars = LEVEL_METER_BAR_COUNT;
    let spacing =
        ((rect.width() - bar_w * (total_bars as f32)) / ((total_bars - 1) as f32)).max(1.0);
    for i in 0..total_bars {
        let x = rect.left() + (i as f32) * (bar_w + spacing);
        let bar_rect = Rect::from_min_size(Pos2::new(x, rect.top() + 2.0), Vec2::new(bar_w, 14.0));
        let col = if i < active_bars {
            palette.group_active_fg
        } else {
            Color32::from_rgb(0x39, 0x42, 0x4F)
        };
        painter.rect_filled(bar_rect, CornerRadius::same(2), col);
    }
}

// =============================================================================
// Group Call Recording Dialog (`calls_group_recording_box.cpp`)
// =============================================================================

/// Modal box for starting/stopping group call recording (`phone.toggleGroupCallRecord`)
/// with audio/video mode, landscape/portrait orientation, and optional title.
#[derive(Debug, Default)]
pub struct GroupRecordingDialog {
    /// Whether the recording box is currently open.
    pub open: bool,
    /// `true` to record video + audio, `false` for audio-only.
    pub record_video: bool,
    /// `true` for portrait orientation, `false` for landscape (used when `record_video` is true).
    pub video_portrait: bool,
    /// Optional recording title (capped to [`RECORDING_TITLE_MAX_LEN`] chars).
    pub title: String,
}

impl GroupRecordingDialog {
    /// Opens the recording dialog in its default state.
    pub fn open_for(&mut self, currently_recording: bool) {
        self.open = true;
        if !currently_recording {
            self.title.clear();
        }
    }

    /// Renders the recording dialog when `self.open` is true.
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
        let mut close_now = false;
        let title_str = if snapshot.recording {
            "Stop Recording"
        } else {
            "Start Recording"
        };

        egui::Window::new(title_str)
            .id(egui::Id::unique("group_call_recording_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(360.0, if snapshot.recording { 150.0 } else { 270.0 }))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                if snapshot.recording {
                    ui.label(
                        RichText::new(
                            "Stop recording this voice chat? The recording will be sent to your Saved Messages.",
                        )
                        .color(palette.group_members_fg),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Stop").clicked() {
                                backend.toggle_group_recording(false, None, false, false);
                                close_now = true;
                            }
                            if ui.button(tr::lng_cancel()).clicked() {
                                close_now = true;
                            }
                        });
                    });
                } else {
                    ui.label(
                        RichText::new("Record Mode")
                            .strong()
                            .size(12.5)
                            .color(palette.group_active_fg),
                    );
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.record_video, "Record Video and Audio");
                    if self.record_video {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Video Orientation")
                                .size(12.0)
                                .color(palette.group_member_sub_fg),
                        );
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.video_portrait, false, "Landscape (16:9)");
                            ui.radio_value(&mut self.video_portrait, true, "Portrait (9:16)");
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Recording Title (optional)")
                            .size(12.0)
                            .color(palette.group_member_sub_fg),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.title)
                            .hint_text("Recording title")
                            .char_limit(RECORDING_TITLE_MAX_LEN)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Start").clicked() {
                                let trimmed = self.title.trim().to_owned();
                                let title_opt = (!trimmed.is_empty()).then_some(trimmed);
                                backend.toggle_group_recording(
                                    true,
                                    title_opt,
                                    self.record_video,
                                    self.video_portrait,
                                );
                                close_now = true;
                            }
                            if ui.button(tr::lng_cancel()).clicked() {
                                close_now = true;
                            }
                        });
                    });
                }
            });

        self.open = keep_open && !close_now;
    }
}

// =============================================================================
// RTMP Stream Key Dialog (`calls_group_rtmp.cpp`)
// =============================================================================

/// Modal box displaying the RTMP server URL and stream key from `phone.getGroupCallStreamRtmpUrl`,
/// with copy buttons, stream key reveal toggle, and key revocation.
#[derive(Debug, Default)]
pub struct GroupRtmpDialog {
    /// Whether the RTMP dialog is open.
    pub open: bool,
    /// Target channel/group peer ID for which credentials were requested.
    pub peer_id: Option<PeerId>,
    /// Loaded RTMP server URL & stream key.
    pub info: Option<RtmpStreamInfo>,
    /// Error message if `phone.getGroupCallStreamRtmpUrl` failed.
    pub error: Option<String>,
    /// Whether the secret stream key is revealed in plaintext.
    pub key_visible: bool,
    /// Transient clipboard confirmation text.
    pub copied_feedback: Option<String>,
}

impl GroupRtmpDialog {
    /// Opens the RTMP dialog for `peer_id` and immediately fetches the URL/key from `backend`.
    pub fn open_for(&mut self, peer_id: PeerId, backend: &mut dyn CallsBackend) {
        self.open = true;
        self.peer_id = Some(peer_id);
        self.key_visible = false;
        self.copied_feedback = None;
        self.reload(backend, false);
    }

    /// Calls `backend.request_group_rtmp_url` (`revoke = true` rotates the stream key).
    pub fn reload(&mut self, backend: &mut dyn CallsBackend, revoke: bool) {
        let Some(peer_id) = self.peer_id else {
            return;
        };
        match backend.request_group_rtmp_url(peer_id, revoke) {
            Ok(info) => {
                self.info = Some(info);
                self.error = None;
                if revoke {
                    self.copied_feedback = Some("Stream key revoked and regenerated.".to_owned());
                }
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    /// Renders the RTMP Stream Key modal window when `self.open` is true.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn CallsBackend,
        palette: &CallPalette,
    ) {
        if !self.open {
            return;
        }
        let mut keep_open = self.open;
        let mut close_now = false;
        let mut do_revoke = false;

        egui::Window::new("Stream With (RTMP)")
            .id(egui::Id::unique("group_call_rtmp_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(400.0, 290.0))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Enter these credentials in your streaming app (such as OBS Studio or XSplit) to broadcast to this chat.",
                    )
                    .size(12.0)
                    .color(palette.group_member_sub_fg),
                );
                ui.add_space(10.0);

                if let Some(ref err) = self.error {
                    ui.label(
                        RichText::new(err)
                            .size(12.0)
                            .color(palette.group_member_muted_icon),
                    );
                    ui.add_space(8.0);
                }

                if let Some(ref info) = self.info {
                    ui.label(
                        RichText::new("Server URL")
                            .strong()
                            .size(12.0)
                            .color(palette.group_active_fg),
                    );
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&info.url)
                                .monospace()
                                .size(11.5)
                                .color(palette.group_members_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Copy").clicked() {
                                ctx.copy_text(info.url.clone());
                                self.copied_feedback = Some("Server URL copied.".to_owned());
                            }
                        });
                    });

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Stream Key")
                            .strong()
                            .size(12.0)
                            .color(palette.group_active_fg),
                    );
                    let display_key = if self.key_visible {
                        info.key.clone()
                    } else {
                        "•".repeat(info.key.chars().count().clamp(8, 24))
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(display_key)
                                .monospace()
                                .size(11.5)
                                .color(palette.group_members_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Copy").clicked() {
                                ctx.copy_text(info.key.clone());
                                self.copied_feedback = Some("Stream key copied.".to_owned());
                            }
                            let toggle_lbl = if self.key_visible { "Hide" } else { "Show" };
                            if ui.button(toggle_lbl).clicked() {
                                self.key_visible = !self.key_visible;
                            }
                        });
                    });
                }

                if let Some(ref msg) = self.copied_feedback {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(msg)
                            .size(11.5)
                            .color(palette.group_member_active),
                    );
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.button("Revoke Stream Key").clicked() {
                        do_revoke = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_now = true;
                        }
                    });
                });
            });

        if do_revoke {
            self.reload(backend, true);
        }

        self.open = keep_open && !close_now;
    }
}
