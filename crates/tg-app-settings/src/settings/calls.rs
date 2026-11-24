//! Speakers, Microphone, Camera, and Call settings backed by `tg-audio` [`DeviceManager`].
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_calls.cpp`.

use egui::{CornerRadius, Rect, Sense, Ui, Vec2};
use tg_audio::{DEFAULT_DEVICE_ID, DeviceInfo, DeviceManager, DeviceType};
use tg_core::CoreSettings;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, MediaSliderStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::sliders::MediaSlider;

use super::strings;
use super::widgets::{
    SettingsIconKind, settings_card, settings_divider, settings_radio_row, settings_row,
    settings_row_pad_x, settings_toggle_row, subsection_title,
};

/// Which audio/video device picker is currently expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePickerTarget {
    Playback,
    Capture,
    CallPlayback,
    CallCapture,
    Camera,
}

/// Persistent UI state for the Calls / Speakers & Camera settings section.
#[derive(Debug, Clone)]
pub struct CallsSettingsState {
    /// Cached playback (speaker/headphone) devices from `tg-audio`.
    pub playback_devices: Vec<DeviceInfo>,
    /// Cached capture (microphone) devices from `tg-audio`.
    pub capture_devices: Vec<DeviceInfo>,
    /// Available camera device names (mock/platform list).
    pub camera_devices: Vec<(String, String)>,
    /// Whether `tg-audio` devices have been enumerated at least once.
    pub initialized: bool,
    /// Currently open device selection picker.
    pub open_picker: Option<DevicePickerTarget>,
    /// Whether the microphone level test indicator is active.
    pub testing_mic: bool,
}

impl Default for CallsSettingsState {
    fn default() -> Self {
        Self {
            playback_devices: Vec::new(),
            capture_devices: Vec::new(),
            camera_devices: vec![
                (
                    DEFAULT_DEVICE_ID.to_owned(),
                    strings::CALLS_DEFAULT_CAMERA.to_owned(),
                ),
                (
                    "cam-integrated".to_owned(),
                    "Integrated Webcam (HD)".to_owned(),
                ),
            ],
            initialized: false,
            open_picker: None,
            testing_mic: false,
        }
    }
}

impl CallsSettingsState {
    /// Enumerates live audio playback and capture devices through [`tg_audio::DeviceManager`].
    pub fn refresh_devices(&mut self) {
        let manager = DeviceManager::new();
        self.playback_devices = manager.devices(DeviceType::Playback).unwrap_or_default();
        self.capture_devices = manager.devices(DeviceType::Capture).unwrap_or_default();
        self.initialized = true;
    }

    /// Resolves a human-readable device name from `devices` (falling back to localized `"Default"`).
    #[must_use]
    pub fn device_display_name(devices: &[DeviceInfo], saved_id: &str) -> String {
        if saved_id.is_empty() || saved_id == DEFAULT_DEVICE_ID {
            return tr::lng_settings_call_device_default();
        }
        devices
            .iter()
            .find(|d| d.id == saved_id && !d.inactive)
            .map_or_else(tr::lng_settings_call_device_default, |d| d.name.clone())
    }
}

/// Renders the Speakers and Camera / Calls section (`settings_calls.cpp`).
pub fn render_calls_section(
    ui: &mut Ui,
    state: &mut CallsSettingsState,
    core: &mut CoreSettings,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    let pad_x = settings_row_pad_x();
    if !state.initialized {
        state.refresh_devices();
    }

    // 1. Speakers & Headphones (`BuildOutputSection`).
    subsection_title(ui, &tr::lng_settings_call_section_output(), colors);
    let playback_name =
        CallsSettingsState::device_display_name(&state.playback_devices, &core.playback_device_id);
    let output_lbl = tr::lng_settings_call_output_device();
    if settings_row(
        ui,
        &output_lbl,
        None,
        Some(&playback_name),
        Some(SettingsIconKind::Speaker),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_picker(&mut state.open_picker, DevicePickerTarget::Playback);
    }
    if state.open_picker == Some(DevicePickerTarget::Playback) {
        render_audio_device_picker(
            ui,
            &state.playback_devices,
            &mut core.playback_device_id,
            colors,
        );
    }

    settings_divider(ui, None, colors);

    // 2. Microphone (`BuildInputSection`).
    subsection_title(ui, &tr::lng_settings_call_section_input(), colors);
    let capture_name =
        CallsSettingsState::device_display_name(&state.capture_devices, &core.capture_device_id);
    let input_lbl = tr::lng_settings_call_input_device();
    if settings_row(
        ui,
        &input_lbl,
        None,
        Some(&capture_name),
        Some(SettingsIconKind::Mic),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_picker(&mut state.open_picker, DevicePickerTarget::Capture);
    }
    if state.open_picker == Some(DevicePickerTarget::Capture) {
        render_audio_device_picker(
            ui,
            &state.capture_devices,
            &mut core.capture_device_id,
            colors,
        );
    }

    // Animated Microphone Level Meter (`Ui::LevelMeter` in `settings_calls.cpp`).
    render_mic_level_meter(ui, state.testing_mic, colors);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let btn_text = if state.testing_mic {
            strings::CALLS_STOP_MIC_TEST
        } else {
            strings::CALLS_TEST_MIC
        };
        if RoundButton::new(btn_text, RoundButtonStyle::default_light(colors))
            .show(ui)
            .clicked()
        {
            state.testing_mic = !state.testing_mic;
        }
        ui.add_space(8.0);
        if RoundButton::new(
            strings::CALLS_REFRESH_DEVICES,
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
        {
            state.refresh_devices();
            toast = Some(format!(
                "{}: {}, {}: {}",
                tr::lng_settings_call_output_device(),
                state.playback_devices.len(),
                tr::lng_settings_call_input_device(),
                state.capture_devices.len()
            ));
        }
    });

    settings_divider(ui, None, colors);

    // 3. Call Devices & Volume (`BuildCallDevicesSection`).
    subsection_title(ui, strings::CALLS_VOICE_VIDEO_TITLE, colors);
    let mut same_as_default =
        core.call_playback_device_id.is_empty() && core.call_capture_device_id.is_empty();
    if settings_toggle_row(
        ui,
        strings::CALLS_SAME_AS_ABOVE,
        None,
        None,
        &mut same_as_default,
        true,
        colors,
    ) {
        if same_as_default {
            core.call_playback_device_id.clear();
            core.call_capture_device_id.clear();
        } else {
            core.call_playback_device_id = DEFAULT_DEVICE_ID.to_owned();
            core.call_capture_device_id = DEFAULT_DEVICE_ID.to_owned();
        }
    }

    if !same_as_default {
        let call_out = CallsSettingsState::device_display_name(
            &state.playback_devices,
            &core.call_playback_device_id,
        );
        if settings_row(
            ui,
            strings::CALLS_OUTPUT_DEVICE,
            None,
            Some(&call_out),
            None,
            false,
            false,
            colors,
        )
        .clicked()
        {
            toggle_picker(&mut state.open_picker, DevicePickerTarget::CallPlayback);
        }
        if state.open_picker == Some(DevicePickerTarget::CallPlayback) {
            render_audio_device_picker(
                ui,
                &state.playback_devices,
                &mut core.call_playback_device_id,
                colors,
            );
        }

        let call_in = CallsSettingsState::device_display_name(
            &state.capture_devices,
            &core.call_capture_device_id,
        );
        if settings_row(
            ui,
            strings::CALLS_INPUT_DEVICE,
            None,
            Some(&call_in),
            None,
            false,
            false,
            colors,
        )
        .clicked()
        {
            toggle_picker(&mut state.open_picker, DevicePickerTarget::CallCapture);
        }
        if state.open_picker == Some(DevicePickerTarget::CallCapture) {
            render_audio_device_picker(
                ui,
                &state.capture_devices,
                &mut core.call_capture_device_id,
                colors,
            );
        }
    }

    // Output & Input volume sliders (`0..=200%`).
    render_volume_slider(
        ui,
        strings::CALLS_SPEAKER_VOLUME,
        &mut core.call_output_volume,
        colors,
    );
    render_volume_slider(
        ui,
        strings::CALLS_MIC_GAIN,
        &mut core.call_input_volume,
        colors,
    );

    settings_toggle_row(
        ui,
        strings::CALLS_AUDIO_DUCKING,
        None,
        None,
        &mut core.call_audio_ducking_enabled,
        true,
        colors,
    );

    settings_divider(ui, None, colors);

    // 4. Camera.
    subsection_title(ui, &tr::lng_settings_call_camera(), colors);
    let cam_name = state
        .camera_devices
        .iter()
        .find(|(id, _)| {
            id == if core.camera_device_id.is_empty() {
                DEFAULT_DEVICE_ID
            } else {
                &core.camera_device_id
            }
        })
        .map_or(strings::CALLS_DEFAULT_CAMERA, |(_, n)| n.as_str());
    if settings_row(
        ui,
        strings::CALLS_VIDEO_INPUT,
        None,
        Some(cam_name),
        Some(SettingsIconKind::Camera),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_picker(&mut state.open_picker, DevicePickerTarget::Camera);
    }
    if state.open_picker == Some(DevicePickerTarget::Camera) {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            let cur = if core.camera_device_id.is_empty() {
                DEFAULT_DEVICE_ID.to_owned()
            } else {
                core.camera_device_id.clone()
            };
            for (idx, (id, name)) in state.camera_devices.iter().enumerate() {
                let mut selected_idx = state
                    .camera_devices
                    .iter()
                    .position(|(cid, _)| cid == &cur)
                    .unwrap_or(0);
                if settings_radio_row(ui, &mut selected_idx, idx, name, true, colors).changed() {
                    core.camera_device_id = id.clone();
                }
            }
        });
    }

    settings_divider(ui, None, colors);

    // 5. Group Calls / Voice Chats (Push-to-Talk & Noise Suppression).
    subsection_title(ui, strings::CALLS_GROUP_CALLS_TITLE, colors);
    settings_toggle_row(
        ui,
        strings::CALLS_NOISE_SUPPRESSION,
        Some(strings::CALLS_NOISE_SUBTITLE),
        None,
        &mut core.group_call_noise_suppression,
        true,
        colors,
    );
    settings_toggle_row(
        ui,
        strings::CALLS_PUSH_TO_TALK,
        Some(strings::CALLS_PTT_SUBTITLE),
        None,
        &mut core.group_call_push_to_talk,
        true,
        colors,
    );
    if core.group_call_push_to_talk {
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(strings::CALLS_PTT_DELAY)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            for ms in [0_i64, 20, 100, 300, 500] {
                if ui
                    .selectable_label(core.group_call_push_to_talk_delay == ms, format!("{ms} ms"))
                    .clicked()
                {
                    core.group_call_push_to_talk_delay = ms;
                }
            }
        });
    }

    ui.add_space(16.0);
    toast
}

fn toggle_picker(slot: &mut Option<DevicePickerTarget>, target: DevicePickerTarget) {
    if *slot == Some(target) {
        *slot = None;
    } else {
        *slot = Some(target);
    }
}

fn render_audio_device_picker(
    ui: &mut Ui,
    devices: &[DeviceInfo],
    saved_id: &mut String,
    colors: &dyn Colors,
) {
    ui.add_space(4.0);
    settings_card(ui, colors, |ui| {
        let mut options: Vec<(String, String)> = vec![(
            DEFAULT_DEVICE_ID.to_owned(),
            tr::lng_settings_call_device_default(),
        )];
        for dev in devices {
            if !dev.inactive {
                let label = if dev.is_default {
                    format!("{} ({})", dev.name, tr::lng_settings_call_device_default())
                } else {
                    dev.name.clone()
                };
                options.push((dev.id.clone(), label));
            }
        }
        let effective = if saved_id.is_empty() {
            DEFAULT_DEVICE_ID
        } else {
            saved_id.as_str()
        };
        let mut selected = options
            .iter()
            .position(|(id, _)| id == effective)
            .unwrap_or(0);
        for (idx, (id, label)) in options.iter().enumerate() {
            if settings_radio_row(ui, &mut selected, idx, label, true, colors).changed() {
                *saved_id = if id == DEFAULT_DEVICE_ID {
                    String::new()
                } else {
                    id.clone()
                };
            }
        }
    });
    ui.add_space(4.0);
}

fn render_volume_slider(ui: &mut Ui, label: &str, volume_pct: &mut i32, colors: &dyn Colors) {
    let pad_x = settings_row_pad_x();
    *volume_pct = (*volume_pct).clamp(0, 200);
    let mut norm = f64::from(*volume_pct) / 200.0;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.label(
            egui::RichText::new(label)
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowFg")),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(format!("{volume_pct}%"))
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(140.0);
        if MediaSlider::new(&mut norm, MediaSliderStyle::default_media_slider(colors))
            .always_display_marker(true)
            .width(avail)
            .show(ui)
            .changed()
        {
            *volume_pct = (norm * 200.0).round().clamp(0.0, 200.0) as i32;
        }
    });
}

fn render_mic_level_meter(ui: &mut Ui, testing: bool, colors: &dyn Colors) {
    let pad_x = settings_row_pad_x();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(160.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(avail, 8.0), Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            colors.color("sliderBgInactive"),
        );
        let level = if testing {
            let t = ui.ctx().input(|i| i.time);
            ui.ctx().request_repaint();
            ((t * 3.7).sin() * 0.35 + 0.45).clamp(0.08, 0.92) as f32
        } else {
            0.0
        };
        if level > 0.01 {
            let active =
                Rect::from_min_size(rect.min, Vec2::new(rect.width() * level, rect.height()));
            ui.painter().rect_filled(
                active,
                CornerRadius::same(4),
                colors.color("sliderBgActive"),
            );
        }
    });
}
