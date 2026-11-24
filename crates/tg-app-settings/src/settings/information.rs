//! My Account / Information settings section.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_information.cpp`
//! and `Telegram/SourceFiles/boxes/username_box.cpp`.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::{EmptyUserpic, userpic_colors};
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::backend::{Birthday, MAX_BIO_LEN, ProfilePhotoUpload, SettingsBackend, UsernameStatus};
use super::strings;
use super::widgets::{
    SettingsIconKind, paint_tinted_icon, settings_divider, settings_row, settings_row_pad_x,
    subsection_title,
};

/// Active inline edit box inside My Account settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InfoEditor {
    /// No sub-editor open.
    #[default]
    None,
    /// Editing first & last name.
    Name {
        first_name: String,
        last_name: String,
        error: Option<String>,
    },
    /// Editing `@username`.
    Username {
        input: String,
        error: Option<String>,
    },
    /// Editing birthday.
    Birthday {
        day: String,
        month: String,
        year: String,
        error: Option<String>,
    },
    /// Selecting avatar photo color, local photo/video file upload, or fallback photo.
    Photo {
        color_index: u8,
        label: String,
        file_path: String,
        fallback: bool,
        video_start_ts: String,
        error: Option<String>,
    },
}

/// Persistent UI state for the My Account section.
#[derive(Debug, Clone, Default)]
pub struct InformationState {
    /// Live bio text buffer synced with backend.
    pub bio_draft: String,
    /// Whether `bio_draft` has been initialized from backend.
    pub bio_initialized: bool,
    /// Active inline editor box.
    pub editor: InfoEditor,
    /// Last bio error, if any.
    pub bio_error: Option<String>,
}

/// Output emitted by [`render_information_section`].
#[derive(Debug, Clone, Default)]
pub struct InformationOutput {
    /// Toast message to display.
    pub toast: Option<String>,
    /// User clicked `"Your Color"` to open `EditPeerColorBox` for their own profile.
    pub open_peer_color: bool,
    /// User clicked Log Out.
    pub request_logout: bool,
}

/// Renders the My Account (`settings_information.cpp`) section.
pub fn render_information_section(
    ui: &mut Ui,
    state: &mut InformationState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> InformationOutput {
    let mut out = InformationOutput::default();
    let profile = backend.profile();
    let pad_x = settings_row_pad_x();
    if !state.bio_initialized {
        state.bio_draft = profile.bio.clone();
        state.bio_initialized = true;
    }

    // Profile avatar + name cover (`st::settings_info_photo_*`).
    ui.add_space(st::settings_info_photo_top.scaled().max(12) as f32);
    ui.vertical_centered(|ui| {
        let photo_size = st::settings_info_photo_size.scaled().max(80) as f32;
        let (av_rect, av_resp) = ui.allocate_exact_size(Vec2::splat(photo_size), Sense::click());
        let userpic =
            EmptyUserpic::for_color_index(profile.photo_color_index, &profile.full_name(), colors);
        userpic.paint_circle(ui.painter(), av_rect);

        // Camera / Edit photo badge on bottom-right corner using real `st::menu_icon_photo_set`.
        let badge_r = 14.0_f32;
        let badge_c = Pos2::new(av_rect.right() - badge_r, av_rect.bottom() - badge_r);
        ui.painter()
            .circle_filled(badge_c, badge_r, colors.color("windowBgActive"));
        ui.painter()
            .circle_stroke(badge_c, badge_r, Stroke::new(2.0, colors.color("windowBg")));
        let badge_rect = Rect::from_center_size(badge_c, Vec2::splat(16.0));
        paint_tinted_icon(
            ui.painter(),
            badge_rect,
            st::menu_icon_photo_set,
            colors,
            Some(Color32::WHITE),
        );

        if av_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if av_resp
            .on_hover_text(strings::CHANGE_PROFILE_PHOTO_TOOLTIP)
            .clicked()
        {
            state.editor = InfoEditor::Photo {
                color_index: profile.photo_color_index,
                label: profile.photo_label.clone().unwrap_or_default(),
                file_path: String::new(),
                fallback: false,
                video_start_ts: String::new(),
                error: None,
            };
        }

        ui.add_space(st::settings_info_name_skip.scaled().max(6) as f32);
        ui.label(
            egui::RichText::new(profile.full_name())
                .font(font_id_from_style(st::settings_cover_name.style.font))
                .color(colors.color("windowBoldFg")),
        );
        ui.label(
            egui::RichText::new(tr::lng_status_online())
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowActiveTextFg")),
        );
    });
    ui.add_space(12.0);

    // Inline Bio editor with remaining-character counter (`SetupBio` in `settings_information.cpp`).
    let bio_len = state.bio_draft.chars().count();
    let remaining = MAX_BIO_LEN as isize - bio_len as isize;
    let bio_label = tr::lng_bio_placeholder();
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.label(
            egui::RichText::new(&bio_label)
                .font(font_id_from_style(st::default_subsection_title.style.font))
                .color(colors.color("windowActiveTextFg")),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad_x);
            let count_color = if remaining < 0 {
                colors.color("boxTextFgError")
            } else {
                colors.color("windowSubTextFg")
            };
            ui.label(
                egui::RichText::new(format!("{remaining}"))
                    .font(font_id_from_style(st::normal_font))
                    .color(count_color),
            );
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let avail = (ui.available_width() - pad_x).max(180.0);
        let mut field_style = InputFieldStyle::default_input(colors);
        field_style.height_min = 42.0;
        field_style.height_max = 86.0;
        InputField::multi_line(
            "settings_info_bio",
            &mut state.bio_draft,
            &bio_label,
            field_style,
        )
        .error(remaining < 0)
        .width(avail)
        .show(ui);
    });
    if state.bio_draft.trim() != profile.bio {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            if RoundButton::new(
                &tr::lng_settings_save(),
                RoundButtonStyle::default_active(colors),
            )
            .enabled(remaining >= 0)
            .show(ui)
            .clicked()
            {
                match backend.update_bio(&state.bio_draft) {
                    Ok(()) => {
                        state.bio_error = None;
                        out.toast = Some(strings::BIO_UPDATED_TOAST.to_owned());
                    }
                    Err(e) => state.bio_error = Some(e),
                }
            }
            ui.add_space(8.0);
            if RoundButton::new(
                strings::BIO_RESET_BUTTON,
                RoundButtonStyle::default_light(colors),
            )
            .show(ui)
            .clicked()
            {
                state.bio_draft = profile.bio.clone();
                state.bio_error = None;
            }
        });
    }
    if let Some(err) = &state.bio_error {
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        });
    }

    settings_divider(ui, Some(strings::BIO_ABOUT_EXAMPLE), colors);

    // Editable account details rows matching `settings_information.cpp` icons.
    subsection_title(ui, &tr::lng_settings_section_info(), colors);

    let name_val = profile.full_name();
    let name_label = tr::lng_settings_name_label();
    if settings_row(
        ui,
        &name_label,
        None,
        Some(&name_val),
        Some(SettingsIconKind::Account),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = InfoEditor::Name {
            first_name: profile.first_name.clone(),
            last_name: profile.last_name.clone(),
            error: None,
        };
    }

    let phone_display = if profile.phone_hidden {
        strings::PHONE_HIDDEN_CLICK_REVEAL.to_owned()
    } else {
        profile.phone.clone()
    };
    let phone_label = tr::lng_settings_phone_label();
    if settings_row(
        ui,
        &phone_label,
        Some(strings::PHONE_VISIBILITY_SUBTITLE),
        Some(&phone_display),
        Some(SettingsIconKind::Phone),
        false,
        false,
        colors,
    )
    .clicked()
    {
        backend.set_phone_hidden(!profile.phone_hidden);
        out.toast = Some(if !profile.phone_hidden {
            strings::PHONE_HIDDEN_TOAST.to_owned()
        } else {
            strings::PHONE_VISIBLE_TOAST.to_owned()
        });
    }

    let username_val = if profile.username.is_empty() {
        strings::USERNAME_NONE.to_owned()
    } else {
        format!("@{}", profile.username)
    };
    let username_label = tr::lng_settings_username_label();
    if settings_row(
        ui,
        &username_label,
        None,
        Some(&username_val),
        Some(SettingsIconKind::Username),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editor = InfoEditor::Username {
            input: profile.username.clone(),
            error: None,
        };
    }

    let birthday_val = profile
        .birthday
        .map_or_else(tr::lng_settings_birthday_add, Birthday::format_display);
    let birthday_label = tr::lng_settings_birthday_label();
    if settings_row(
        ui,
        &birthday_label,
        None,
        Some(&birthday_val),
        Some(SettingsIconKind::Birthday),
        false,
        false,
        colors,
    )
    .clicked()
    {
        let (day, month, year) = match profile.birthday {
            Some(b) => (
                b.day.to_string(),
                b.month.to_string(),
                b.year.map(|y| y.to_string()).unwrap_or_default(),
            ),
            None => ("1".to_owned(), "1".to_owned(), String::new()),
        };
        state.editor = InfoEditor::Birthday {
            day,
            month,
            year,
            error: None,
        };
    }

    if settings_row(
        ui,
        "Your Color",
        Some("Customize your name and profile accent color"),
        None,
        Some(SettingsIconKind::Brush),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.open_peer_color = true;
    }

    // Inline editor card when a field is being edited.
    render_inline_editor(ui, state, backend, colors, &mut out);

    let username_about = tr::lng_settings_username_about();
    settings_divider(ui, Some(&username_about), colors);

    let logout_label = tr::lng_settings_logout();
    if settings_row(
        ui,
        &logout_label,
        None,
        None,
        Some(SettingsIconKind::Logout),
        true,
        false,
        colors,
    )
    .clicked()
    {
        out.request_logout = true;
    }

    ui.add_space(16.0);
    out
}

fn render_inline_editor(
    ui: &mut Ui,
    state: &mut InformationState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut InformationOutput,
) {
    if state.editor == InfoEditor::None {
        return;
    }

    let mut close = false;
    ui.add_space(8.0);
    super::widgets::settings_card(ui, colors, |ui| match &mut state.editor {
        InfoEditor::None => {}
        InfoEditor::Name {
            first_name,
            last_name,
            error,
        } => {
            ui.label(
                egui::RichText::new(strings::EDIT_YOUR_NAME_TITLE)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            InputField::single_line(
                "info_first_name",
                first_name,
                &tr::lng_signup_firstname(),
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
            ui.add_space(6.0);
            InputField::single_line(
                "info_last_name",
                last_name,
                &tr::lng_signup_lastname(),
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
            if let Some(msg) = error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(msg.as_str())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    match backend.update_name(first_name, last_name) {
                        Ok(()) => {
                            out.toast = Some(strings::NAME_UPDATED_TOAST.to_owned());
                            close = true;
                        }
                        Err(e) => *error = Some(e),
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        InfoEditor::Username { input, error } => {
            ui.label(
                egui::RichText::new(tr::lng_username_title())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            let status = backend.check_username(input);
            let is_err = matches!(
                status,
                UsernameStatus::Taken
                    | UsernameStatus::TooShort
                    | UsernameStatus::TooLong
                    | UsernameStatus::InvalidChars
            );
            InputField::single_line(
                "info_username",
                input,
                &tr::lng_settings_username_label(),
                InputFieldStyle::default_input(colors),
            )
            .error(is_err)
            .show(ui);
            ui.add_space(4.0);
            let (status_text, status_color) = match status {
                UsernameStatus::Empty => (
                    strings::USERNAME_REMOVE_HINT.to_owned(),
                    colors.color("windowSubTextFg"),
                ),
                UsernameStatus::Current => (
                    strings::USERNAME_CURRENT_HINT.to_owned(),
                    colors.color("windowSubTextFg"),
                ),
                UsernameStatus::Available => {
                    (tr::lng_username_available(), colors.color("boxTextFgGood"))
                }
                UsernameStatus::Taken => {
                    (tr::lng_username_occupied(), colors.color("boxTextFgError"))
                }
                UsernameStatus::TooShort => {
                    (tr::lng_username_too_short(), colors.color("boxTextFgError"))
                }
                UsernameStatus::TooLong => (
                    strings::USERNAME_TOO_LONG.to_owned(),
                    colors.color("boxTextFgError"),
                ),
                UsernameStatus::InvalidChars => (
                    tr::lng_username_bad_symbols(),
                    colors.color("boxTextFgError"),
                ),
            };
            ui.label(
                egui::RichText::new(status_text)
                    .font(font_id_from_style(st::normal_font))
                    .color(status_color),
            );
            if let Some(msg) = error {
                ui.label(
                    egui::RichText::new(msg.as_str())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .enabled(!is_err)
                .show(ui)
                .clicked()
                {
                    match backend.update_username(input) {
                        Ok(()) => {
                            out.toast = Some(strings::USERNAME_SAVED_TOAST.to_owned());
                            close = true;
                        }
                        Err(e) => *error = Some(e),
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        InfoEditor::Birthday {
            day,
            month,
            year,
            error,
        } => {
            ui.label(
                egui::RichText::new(tr::lng_settings_birthday_title())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                InputField::single_line(
                    "bday_day",
                    day,
                    strings::BIRTHDAY_DAY_PH,
                    InputFieldStyle::default_input(colors),
                )
                .width(90.0)
                .show(ui);
                ui.add_space(8.0);
                InputField::single_line(
                    "bday_month",
                    month,
                    strings::BIRTHDAY_MONTH_PH,
                    InputFieldStyle::default_input(colors),
                )
                .width(100.0)
                .show(ui);
                ui.add_space(8.0);
                InputField::single_line(
                    "bday_year",
                    year,
                    strings::BIRTHDAY_YEAR_PH,
                    InputFieldStyle::default_input(colors),
                )
                .width(100.0)
                .show(ui);
            });
            if let Some(msg) = error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(msg.as_str())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let parsed_day = day.trim().parse::<u8>().ok();
                    let parsed_month = month.trim().parse::<u8>().ok();
                    let parsed_year = if year.trim().is_empty() {
                        Ok(None)
                    } else {
                        year.trim().parse::<u16>().map(Some).map_err(|_| ())
                    };
                    match (parsed_day, parsed_month, parsed_year) {
                        (Some(d), Some(m), Ok(y)) => {
                            let b = Birthday {
                                day: d,
                                month: m,
                                year: y,
                            };
                            match backend.update_birthday(Some(b)) {
                                Ok(()) => {
                                    out.toast = Some(tr::lng_settings_birthday_saved());
                                    close = true;
                                }
                                Err(e) => *error = Some(e),
                            }
                        }
                        _ => *error = Some(strings::BIRTHDAY_INVALID_ERROR.to_owned()),
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(
                    &tr::lng_settings_birthday_reset(),
                    RoundButtonStyle::attention_box(colors),
                )
                .show(ui)
                .clicked()
                {
                    let _ = backend.update_birthday(None);
                    out.toast = Some(strings::BIRTHDAY_REMOVED_TOAST.to_owned());
                    close = true;
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
        InfoEditor::Photo {
            color_index,
            label,
            file_path,
            fallback,
            video_start_ts,
            error,
        } => {
            ui.label(
                egui::RichText::new(strings::PHOTO_AVATAR_EDITOR_TITLE)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for idx in 0u8..7 {
                    let bg = userpic_colors(idx, colors);
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                    ui.painter().circle_filled(r.center(), 13.0, bg.color1);
                    if *color_index == idx {
                        ui.painter().circle_stroke(
                            r.center(),
                            14.5,
                            Stroke::new(2.0, colors.color("windowBoldFg")),
                        );
                    }
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        *color_index = idx;
                    }
                }
            });
            ui.add_space(8.0);
            InputField::single_line(
                "info_photo_label",
                label,
                strings::PHOTO_LABEL_PH,
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
            ui.add_space(6.0);
            InputField::single_line(
                "info_photo_file_path",
                file_path,
                "Image or video file path (.jpg, .png, .mp4)",
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(fallback, "Set as public fallback photo");
                ui.add_space(12.0);
                InputField::single_line(
                    "info_photo_video_ts",
                    video_start_ts,
                    "Video frame (sec)",
                    InputFieldStyle::default_input(colors),
                )
                .width(120.0)
                .show(ui);
            });
            if let Some(msg) = error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(msg.as_str())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    strings::PHOTO_APPLY_BUTTON,
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let trimmed_path = file_path.trim();
                    if !trimmed_path.is_empty() {
                        let path = std::path::Path::new(trimmed_path);
                        match std::fs::read(path) {
                            Ok(bytes) => {
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("avatar.jpg")
                                    .to_owned();
                                let ext = path
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .map(str::to_ascii_lowercase)
                                    .unwrap_or_default();
                                let is_video =
                                    matches!(ext.as_str(), "mp4" | "mov" | "webm" | "mkv");
                                let start_ts = video_start_ts.trim().parse::<f64>().ok();
                                let upload = if is_video {
                                    ProfilePhotoUpload::video_avatar(
                                        name, bytes, start_ts, *fallback,
                                    )
                                } else {
                                    ProfilePhotoUpload::static_photo(name, bytes, *fallback)
                                };
                                match backend.upload_profile_photo(upload) {
                                    Ok(_) => {
                                        *error = None;
                                        out.toast = Some(strings::PHOTO_UPDATED_TOAST.to_owned());
                                        close = true;
                                    }
                                    Err(e) => *error = Some(e),
                                }
                            }
                            Err(e) => *error = Some(format!("Could not read file: {e}")),
                        }
                    } else {
                        let opt = if label.trim().is_empty() {
                            None
                        } else {
                            Some(label.trim().to_owned())
                        };
                        backend.update_profile_photo(*color_index, opt);
                        out.toast = Some(strings::PHOTO_UPDATED_TOAST.to_owned());
                        close = true;
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
        }
    });

    if close {
        state.editor = InfoEditor::None;
    }
}
