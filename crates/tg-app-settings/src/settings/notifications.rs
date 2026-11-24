//! Notifications and Sounds settings section (including interactive corner position picker).
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_notifications.cpp`
//! and `Telegram/SourceFiles/settings/sections/settings_notifications_type.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_core::CoreSettings;
use tg_core::settings::{NotifyView, ScreenCorner};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, MediaSliderStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::sliders::MediaSlider;

use super::backend::{
    DEFAULT_RINGTONE_MAX_SIZE, MAX_NOTIFICATIONS_COUNT, NotifyException, NotifyScope,
    ReactionsNotifyFrom, SettingsBackend, format_bytes,
};
use super::strings;
use super::widgets::{
    SettingsIconKind, settings_card, settings_checkbox_row, settings_divider, settings_radio_row,
    settings_row, settings_row_pad_x, settings_split_toggle_row, settings_toggle_row,
    subsection_title,
};

/// Persistent UI state for the Notifications & Sounds section.
#[derive(Debug, Clone, Default)]
pub struct NotificationsState {
    /// Currently expanded per-scope editor (`PrivateChats` / `Groups` / `Channels`).
    pub editing_scope: Option<NotifyScope>,
}

/// Output emitted by [`render_notifications_section`].
#[derive(Debug, Clone, Default)]
pub struct NotificationsOutput {
    /// Navigate to `Settings -> Notifications -> Reactions` (`settings_notifications_reactions.cpp`).
    pub open_reactions: bool,
    /// Optional toast message (e.g. after resetting all notification settings).
    pub toast: Option<String>,
}

/// Renders the Notifications and Sounds section (`settings_notifications.cpp`).
pub fn render_notifications_section(
    ui: &mut Ui,
    state: &mut NotificationsState,
    core: &mut CoreSettings,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> NotificationsOutput {
    let mut out = NotificationsOutput::default();
    let toast = &mut out.toast;
    let pad_x = settings_row_pad_x();

    // 1. Global desktop notification toggles.
    subsection_title(ui, &tr::lng_settings_notify_global(), colors);

    let desktop_notify_lbl = tr::lng_settings_desktop_notify();
    settings_toggle_row(
        ui,
        &desktop_notify_lbl,
        None,
        Some(SettingsIconKind::Notifications),
        &mut core.desktop_notify,
        true,
        colors,
    );

    let mut native = core.native_notifications.unwrap_or(true);
    if settings_toggle_row(
        ui,
        strings::NATIVE_NOTIFICATIONS_LABEL,
        Some(strings::NATIVE_NOTIFICATIONS_SUBTITLE),
        None,
        &mut native,
        core.desktop_notify,
        colors,
    ) {
        core.native_notifications = Some(native);
    }

    let allow_sound_lbl = tr::lng_settings_sound_allowed();
    settings_toggle_row(
        ui,
        &allow_sound_lbl,
        None,
        Some(SettingsIconKind::Speaker),
        &mut core.sound_notify,
        true,
        colors,
    );

    // Notification volume slider (`0..=100%`).
    if core.sound_notify {
        let mut vol = f64::from(core.notifications_volume.min(100)) / 100.0;
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(strings::NOTIFICATION_VOLUME_LABEL)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(pad_x);
                ui.label(
                    egui::RichText::new(format!("{}%", core.notifications_volume))
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowActiveTextFg")),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let avail = (ui.available_width() - pad_x).max(140.0);
            if MediaSlider::new(&mut vol, MediaSliderStyle::default_media_slider(colors))
                .always_display_marker(true)
                .width(avail)
                .show(ui)
                .changed()
            {
                core.notifications_volume = (vol * 100.0).round().clamp(0.0, 100.0) as u16;
            }
        });
        ui.add_space(6.0);
    }

    settings_toggle_row(
        ui,
        strings::FLASH_TASKBAR_LABEL,
        None,
        None,
        &mut core.flash_bounce_notify,
        true,
        colors,
    );

    settings_toggle_row(
        ui,
        strings::SKIP_TOASTS_FOCUS_LABEL,
        None,
        None,
        &mut core.skip_toasts_in_focus,
        core.desktop_notify,
        colors,
    );

    let all_accounts_lbl = tr::lng_settings_notify_all();
    let all_accounts_sub = tr::lng_settings_notify_all_about();
    settings_toggle_row(
        ui,
        &all_accounts_lbl,
        Some(&all_accounts_sub),
        None,
        &mut core.notify_from_all,
        true,
        colors,
    );

    settings_divider(ui, None, colors);

    // 2. Notification content / preview text (`CoreSettings::notify_view`).
    subsection_title(ui, strings::NOTIFY_PREVIEW_TITLE, colors);
    let preview_enabled = core.desktop_notify;
    settings_radio_row(
        ui,
        &mut core.notify_view,
        NotifyView::ShowPreview,
        strings::NOTIFY_SHOW_PREVIEW,
        preview_enabled,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.notify_view,
        NotifyView::ShowName,
        strings::NOTIFY_SHOW_NAME,
        preview_enabled,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.notify_view,
        NotifyView::ShowNothing,
        strings::NOTIFY_SHOW_NOTHING,
        preview_enabled,
        colors,
    );

    settings_divider(ui, None, colors);

    // 3. Notification Position Corner Picker & Count (`NotificationsCount` in `settings_notifications.cpp`).
    subsection_title(ui, &tr::lng_settings_notifications_position(), colors);
    render_corner_picker(
        ui,
        &mut core.notifications_corner,
        core.desktop_notify,
        colors,
    );

    ui.add_space(10.0);
    let clamped_count = core.notifications_count.clamp(1, MAX_NOTIFICATIONS_COUNT);
    core.notifications_count = clamped_count;
    let mut count_norm = f64::from(clamped_count - 1) / f64::from(MAX_NOTIFICATIONS_COUNT - 1);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.label(
            egui::RichText::new(tr::lng_settings_notifications_count())
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowFg")),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(clamped_count.to_string())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(140.0);
        if MediaSlider::new(
            &mut count_norm,
            MediaSliderStyle::default_media_slider(colors),
        )
        .always_display_marker(true)
        .disabled(!core.desktop_notify)
        .width(avail)
        .show(ui)
        .changed()
        {
            core.notifications_count =
                1 + (count_norm * f64::from(MAX_NOTIFICATIONS_COUNT - 1)).round() as i32;
        }
    });
    ui.add_space(4.0);

    settings_divider(ui, None, colors);

    // 4. Badge Counter options (`CoreSettings::include_muted_counter*`, `count_unread_messages`).
    subsection_title(ui, &tr::lng_settings_badge_title(), colors);
    settings_checkbox_row(
        ui,
        &mut core.include_muted_counter,
        &tr::lng_settings_include_muted(),
        true,
        colors,
    );
    settings_checkbox_row(
        ui,
        &mut core.include_muted_counter_folders,
        &tr::lng_settings_include_muted_folders(),
        true,
        colors,
    );
    settings_checkbox_row(
        ui,
        &mut core.count_unread_messages,
        &tr::lng_settings_count_unread(),
        true,
        colors,
    );

    settings_divider(ui, None, colors);

    // 5. Notifications for chats (per-scope split toggles).
    subsection_title(ui, &tr::lng_settings_notify_title(), colors);
    for scope in NotifyScope::ALL {
        let mut scope_cfg = backend.scope_notifications(scope);
        let subtitle = scope_cfg.subtitle();
        let icon = match scope {
            NotifyScope::PrivateChats => SettingsIconKind::Account,
            NotifyScope::Groups => SettingsIconKind::Folders,
            NotifyScope::Channels => SettingsIconKind::Notifications,
        };
        let scope_title = scope.localized_title();
        let (clicked, toggled) = settings_split_toggle_row(
            ui,
            &scope_title,
            &subtitle,
            Some(icon),
            &mut scope_cfg.enabled,
            colors,
        );
        if toggled {
            backend.set_scope_notifications(scope, scope_cfg);
        }
        if clicked {
            if state.editing_scope == Some(scope) {
                state.editing_scope = None;
            } else {
                state.editing_scope = Some(scope);
            }
        }
    }

    if let Some(scope) = state.editing_scope {
        ui.add_space(6.0);
        render_scope_editor(ui, scope, backend, colors, toast);
    }

    settings_divider(ui, None, colors);

    // 6. Events (contact joined, pinned messages, reactions).
    subsection_title(ui, &tr::lng_settings_events_title(), colors);
    let mut events = backend.notification_events();
    let mut events_changed = false;

    let joined_lbl = tr::lng_settings_events_joined();
    if settings_toggle_row(
        ui,
        &joined_lbl,
        None,
        None,
        &mut events.contact_joined,
        true,
        colors,
    ) {
        events_changed = true;
    }

    let pinned_lbl = tr::lng_settings_events_pinned();
    settings_toggle_row(
        ui,
        &pinned_lbl,
        None,
        None,
        &mut core.notify_about_pinned,
        true,
        colors,
    );

    // Split-toggle entry to `Settings -> Notifications -> Reactions` (`settings_notifications_reactions.cpp`).
    let react_title = tr::lng_notification_reactions();
    let react_subtitle = events.reactions_summary();
    let mut react_enabled = events.reactions_enabled();
    let (react_clicked, react_toggled) = settings_split_toggle_row(
        ui,
        &react_title,
        &react_subtitle,
        Some(SettingsIconKind::Notifications),
        &mut react_enabled,
        colors,
    );
    if react_toggled {
        events.set_all_reactions_from(if react_enabled {
            ReactionsNotifyFrom::All
        } else {
            ReactionsNotifyFrom::None
        });
        events_changed = true;
    } else if react_clicked {
        out.open_reactions = true;
    }

    if events_changed {
        backend.set_notification_events(events);
    }

    settings_divider(ui, None, colors);

    // 7. Custom Ringtones (`boxes/ringtones_box.cpp`: getSavedRingtones / saveRingtone / uploadRingtone).
    subsection_title(ui, &tr::lng_ringtones_box_title(), colors);
    render_ringtones_subsection(ui, backend, colors, toast);

    settings_divider(ui, None, colors);

    if settings_row(
        ui,
        strings::RESET_ALL_NOTIFICATIONS_TITLE,
        Some(strings::RESET_ALL_NOTIFICATIONS_SUBTITLE),
        None,
        None,
        true,
        false,
        colors,
    )
    .clicked()
    {
        backend.reset_all_notifications();
        state.editing_scope = None;
        *toast = Some(strings::RESET_ALL_NOTIFICATIONS_TOAST.to_owned());
    }

    ui.add_space(16.0);
    out
}

/// Interactive mini-monitor 4-corner notification position picker (`settings_notifications.cpp`).
pub fn render_corner_picker(
    ui: &mut Ui,
    corner: &mut ScreenCorner,
    enabled: bool,
    colors: &dyn Colors,
) {
    let pad_x = settings_row_pad_x();
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let monitor_w = 240.0;
        let monitor_h = 136.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(monitor_w, monitor_h), Sense::hover());

        let screen_bg = colors.color("windowBgOver");
        let border_col = colors.color("windowSubTextFg");
        let active_col = colors.color("windowBgActive");
        let inactive_col = colors.color("inputBorderFg");

        // Monitor bezel.
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), screen_bg);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(2.0, border_col),
            StrokeKind::Inside,
        );

        let pad = 12.0;
        let box_w = 88.0;
        let box_h = 44.0;
        let corners = [
            (
                ScreenCorner::TopLeft,
                Rect::from_min_size(
                    Pos2::new(rect.left() + pad, rect.top() + pad),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_TOP_LEFT,
            ),
            (
                ScreenCorner::TopRight,
                Rect::from_min_size(
                    Pos2::new(rect.right() - pad - box_w, rect.top() + pad),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_TOP_RIGHT,
            ),
            (
                ScreenCorner::BottomLeft,
                Rect::from_min_size(
                    Pos2::new(rect.left() + pad, rect.bottom() - pad - box_h),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_BOTTOM_LEFT,
            ),
            (
                ScreenCorner::BottomRight,
                Rect::from_min_size(
                    Pos2::new(rect.right() - pad - box_w, rect.bottom() - pad - box_h),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_BOTTOM_RIGHT,
            ),
        ];

        for (candidate, c_rect, label) in corners {
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let resp = ui.interact(
                c_rect,
                ui.scope_id().with(("notify_corner", candidate as i32)),
                sense,
            );
            let selected = *corner == candidate;
            if enabled && resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if enabled && resp.clicked() {
                *corner = candidate;
            }

            let fill = if selected {
                active_col
            } else if resp.hovered() && enabled {
                colors.color("lightButtonBgOver")
            } else {
                colors.color("windowBg")
            };
            let stroke = if selected {
                Stroke::new(1.5, active_col)
            } else {
                Stroke::new(1.0, inactive_col)
            };
            ui.painter()
                .rect_filled(c_rect, CornerRadius::same(5), fill);
            ui.painter()
                .rect_stroke(c_rect, CornerRadius::same(5), stroke, StrokeKind::Inside);
            ui.painter().text(
                c_rect.center(),
                Align2::CENTER_CENTER,
                label,
                egui::FontId::new(11.0, tg_ui::fonts::family_regular()),
                if selected {
                    Color32::WHITE
                } else {
                    colors.color("windowFg")
                },
            );
        }

        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(strings::CORNER_SCREEN_CORNER)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            let name = match corner {
                ScreenCorner::TopLeft => strings::CORNER_TOP_LEFT,
                ScreenCorner::TopRight => strings::CORNER_TOP_RIGHT,
                ScreenCorner::BottomLeft => strings::CORNER_BOTTOM_LEFT,
                ScreenCorner::BottomRight => strings::CORNER_BOTTOM_RIGHT,
            };
            ui.label(
                egui::RichText::new(format!("{} {name}", strings::CORNER_SELECTED_PREFIX))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(strings::CORNER_CLICK_HINT)
                    .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    });
}

fn render_scope_editor(
    ui: &mut Ui,
    scope: NotifyScope,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    let mut cfg = backend.scope_notifications(scope);
    let contacts = backend.contacts();
    let mut changed = false;

    let heading = match scope {
        NotifyScope::PrivateChats => tr::lng_notification_title_private_chats(),
        NotifyScope::Groups => tr::lng_notification_title_groups(),
        NotifyScope::Channels => tr::lng_notification_title_channels(),
    };

    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(heading)
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);
        if ui
            .checkbox(&mut cfg.show_preview, strings::SCOPE_SHOW_PREVIEW)
            .changed()
        {
            changed = true;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("{}:", tr::lng_notification_sound()))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
            );
            for sound in ["Default", "Chime", "Note", "None"] {
                if ui.selectable_label(cfg.sound == sound, sound).clicked() {
                    cfg.sound = sound.to_owned();
                    changed = true;
                }
            }
            for tone in backend.saved_ringtones() {
                let label = tone.display_name();
                let key = format!("Ringtone {}", tone.id);
                let is_selected = cfg.sound == label || cfg.sound == key;
                if ui.selectable_label(is_selected, &label).clicked() {
                    cfg.sound = label;
                    changed = true;
                }
            }
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!(
                "{} ({}):",
                tr::lng_notification_exceptions_title(),
                cfg.exceptions.len()
            ))
            .font(font_id_from_style(st::semibold_font))
            .color(colors.color("windowBoldFg")),
        );
        let mut remove_idx = None;
        let muted_lbl = tr::lng_notification_exceptions_muted();
        let unmuted_lbl = tr::lng_notification_exceptions_unmuted();
        let remove_lbl = tr::lng_notification_exceptions_remove();
        for (idx, exc) in cfg.exceptions.iter().enumerate() {
            ui.horizontal(|ui| {
                let status = if exc.muted { &muted_lbl } else { &unmuted_lbl };
                ui.label(
                    egui::RichText::new(format!("{} — {status}", exc.peer.name))
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                if ui.small_button(&remove_lbl).clicked() {
                    remove_idx = Some(idx);
                }
            });
        }
        if let Some(idx) = remove_idx {
            cfg.exceptions.remove(idx);
            changed = true;
        }

        // Quick-add exception from known contacts matching scope.
        if let Some(candidate) = contacts.into_iter().find(|c| {
            let matches_scope = match scope {
                NotifyScope::PrivateChats => !c.is_group_or_channel,
                NotifyScope::Groups | NotifyScope::Channels => c.is_group_or_channel,
            };
            matches_scope && !cfg.exceptions.iter().any(|e| e.peer.id == c.id)
        }) {
            ui.add_space(6.0);
            let add_label = format!(
                "{}: {}",
                tr::lng_notification_exceptions_add(),
                candidate.name
            );
            if RoundButton::new(&add_label, RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                cfg.exceptions.push(NotifyException {
                    peer: candidate,
                    muted: cfg.enabled,
                    sound: "None".to_owned(),
                });
                changed = true;
                *toast = Some(strings::EXCEPTION_ADDED_TOAST.to_owned());
            }
        }
    });

    if changed {
        backend.set_scope_notifications(scope, cfg);
    }
}

fn render_ringtones_subsection(
    ui: &mut Ui,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    let ringtones = backend.saved_ringtones();
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_ringtones_box_cloud_subtitle())
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);

        if ringtones.is_empty() {
            ui.label(
                egui::RichText::new(strings::RINGTONES_EMPTY_HINT)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            let delete_lbl = tr::lng_box_delete();
            for tone in &ringtones {
                ui.horizontal(|ui| {
                    let meta = if tone.duration_secs > 0.0 {
                        format!(
                            "{}  ·  {:.1}s  ·  {}",
                            tone.display_name(),
                            tone.duration_secs,
                            format_bytes(tone.size)
                        )
                    } else {
                        format!("{}  ·  {}", tone.display_name(), format_bytes(tone.size))
                    };
                    ui.label(
                        egui::RichText::new(meta)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowFg")),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(&delete_lbl).clicked() {
                            match backend.save_ringtone(tone.id, true) {
                                Ok(()) => *toast = Some(strings::RINGTONE_REMOVED_TOAST.to_owned()),
                                Err(err) => *toast = Some(err),
                            }
                        }
                    });
                });
            }
        }

        ui.add_space(8.0);
        if RoundButton::new(
            &tr::lng_ringtones_box_upload_button(),
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_title(tr::lng_ringtones_box_upload_choose())
                .add_filter("Audio files", &["mp3", "ogg", "opus", "wav"])
                .pick_file()
        {
            match std::fs::read(&path) {
                Ok(bytes) if bytes.len() > DEFAULT_RINGTONE_MAX_SIZE => {
                    let limit = format_bytes(DEFAULT_RINGTONE_MAX_SIZE as i64);
                    *toast = Some(tr::lng_ringtones_error_max_size(&limit));
                }
                Ok(bytes) => {
                    let file_name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("ringtone.mp3")
                        .to_owned();
                    let mime = ringtone_mime_for_filename(&file_name);
                    match backend.upload_ringtone(&file_name, mime, &bytes) {
                        Ok(_) => *toast = Some(tr::lng_ringtones_toast_added()),
                        Err(err) => *toast = Some(err),
                    }
                }
                Err(err) => *toast = Some(format!("Failed to read ringtone file: {err}")),
            }
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tr::lng_ringtones_box_about())
                .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                .color(colors.color("windowSubTextFg")),
        );
    });
}

fn ringtone_mime_for_filename(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".ogg") || lower.ends_with(".opus") {
        "audio/ogg"
    } else if lower.ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/mpeg"
    }
}
