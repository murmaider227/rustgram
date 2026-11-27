//! Two-Step Verification (Cloud Password) multi-step setup and management wizard.
//!
//! Port origin: `Telegram/SourceFiles/settings/cloud_password/`
//! (`settings_cloud_password_start.cpp`, `settings_cloud_password_input.cpp`,
//! `settings_cloud_password_hint.cpp`, `settings_cloud_password_email.cpp`,
//! `settings_cloud_password_email_confirm.cpp`, `settings_cloud_password_manage.cpp`).

use egui::Ui;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::backend::SettingsBackend;
use super::mock::MOCK_EMAIL_CONFIRM_CODE;
use super::strings;
use super::widgets::{
    SettingsIconKind, settings_card, settings_divider, settings_row, subsection_title,
};

/// Wizard step for Two-Step Verification (`settings/cloud_password/*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloudPasswordStep {
    /// Entry screen: shows Start CTA when disabled, or EnterCurrent / Manage when enabled.
    #[default]
    Start,
    /// Verify existing 2FA password before managing/changing/disabling.
    EnterCurrent,
    /// Manage enabled 2FA password (Change Password / Turn Off / Change Recovery Email).
    Manage,
    /// Step 1 of setup/change: enter and confirm new password.
    InputPassword,
    /// Step 2 of setup/change: enter optional hint (validated != password).
    Hint,
    /// Step 3 of setup/change: enter recovery email or skip with warning.
    Email,
    /// Step 4: verify 6-digit email confirmation code.
    EmailConfirm,
    /// Confirm disabling 2FA password.
    DisableConfirm,
}

/// Persistent wizard state for Two-Step Verification.
#[derive(Debug, Clone, Default)]
pub struct CloudPasswordWizard {
    /// Active wizard step.
    pub step: CloudPasswordStep,
    /// Whether this flow is changing an existing password (vs creating one).
    pub is_changing: bool,
    /// Whether this flow is only updating recovery email.
    pub email_only: bool,
    /// Verified current password held in memory for the duration of the wizard.
    pub verified_current: String,
    /// Input buffer for current password check.
    pub current_input: String,
    /// New password input buffer.
    pub new_password: String,
    /// Re-entered new password buffer.
    pub confirm_password: String,
    /// Optional password hint buffer.
    pub hint: String,
    /// Optional recovery email buffer.
    pub email: String,
    /// 6-digit email verification code buffer.
    pub email_code: String,
    /// Whether the "Skip recovery email?" warning is shown.
    pub confirm_skip_email: bool,
    /// Validation or backend error message.
    pub error: Option<String>,
}

impl CloudPasswordWizard {
    /// Synchronizes the initial wizard step with the backend's current 2FA state.
    pub fn sync_initial_step(&mut self, backend: &dyn SettingsBackend) {
        // The step follows the real state, not the placeholder shown while it loads.
        if backend.settings_read_pending(super::backend::SettingsRead::CloudPassword) {
            return;
        }
        let st = backend.cloud_password_state();
        if self.step == CloudPasswordStep::Start {
            if !st.unconfirmed_email_pattern.is_empty() {
                self.step = CloudPasswordStep::EmailConfirm;
            } else if st.has_password {
                self.step = if self.verified_current.is_empty() {
                    CloudPasswordStep::EnterCurrent
                } else {
                    CloudPasswordStep::Manage
                };
            }
        }
    }

    /// Clears sensitive draft fields and resets back to Manage or Start.
    pub fn reset_drafts(&mut self, has_password: bool) {
        self.new_password.clear();
        self.confirm_password.clear();
        self.hint.clear();
        self.email.clear();
        self.email_code.clear();
        self.confirm_skip_email = false;
        self.error = None;
        self.is_changing = false;
        self.email_only = false;
        self.step = if has_password && !self.verified_current.is_empty() {
            CloudPasswordStep::Manage
        } else if has_password {
            CloudPasswordStep::EnterCurrent
        } else {
            CloudPasswordStep::Start
        };
    }

    /// Advances from [`CloudPasswordStep::InputPassword`] to [`CloudPasswordStep::Hint`].
    pub fn submit_new_password(&mut self) -> Result<(), String> {
        if self.new_password.is_empty() {
            return Err(strings::CLOUD_PWD_EMPTY_ERROR.to_owned());
        }
        if self.new_password != self.confirm_password {
            return Err(tr::lng_cloud_password_differ());
        }
        self.error = None;
        self.step = CloudPasswordStep::Hint;
        Ok(())
    }

    /// Advances from [`CloudPasswordStep::Hint`] to [`CloudPasswordStep::Email`].
    pub fn submit_hint(&mut self) -> Result<(), String> {
        let trimmed = self.hint.trim();
        if !trimmed.is_empty() && trimmed == self.new_password {
            return Err(tr::lng_cloud_password_bad());
        }
        self.error = None;
        self.step = CloudPasswordStep::Email;
        Ok(())
    }
}

/// Renders the Two-Step Verification (Cloud Password) multi-step section.
pub fn render_cloud_password_section(
    ui: &mut Ui,
    wizard: &mut CloudPasswordWizard,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast = None;
    wizard.sync_initial_step(backend);
    let state = backend.cloud_password_state();

    match wizard.step {
        CloudPasswordStep::Start => {
            subsection_title(ui, &tr::lng_settings_cloud_password_start_title(), colors);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_settings_cloud_password_start_about())
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowBoldFg")),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(tr::lng_cloud_password_about())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
                ui.add_space(12.0);
                if RoundButton::new(
                    strings::CLOUD_PWD_SET_BUTTON,
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    wizard.is_changing = false;
                    wizard.email_only = false;
                    wizard.error = None;
                    wizard.step = CloudPasswordStep::InputPassword;
                }
            });
        }
        CloudPasswordStep::EnterCurrent => {
            subsection_title(
                ui,
                &tr::lng_settings_cloud_password_check_subtitle(),
                colors,
            );
            settings_card(ui, colors, |ui| {
                if !state.hint.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {}",
                            tr::lng_settings_cloud_password_hint_title(),
                            state.hint
                        ))
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(6.0);
                }
                InputField::single_line(
                    "cloud_pwd_current",
                    &mut wizard.current_input,
                    &tr::lng_cloud_password_enter_old(),
                    InputFieldStyle::default_input(colors),
                )
                .password(true)
                .error(wizard.error.is_some())
                .show(ui);
                render_error(ui, &wizard.error, colors);
                ui.add_space(10.0);
                if RoundButton::new(
                    &tr::lng_continue(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    match backend.verify_cloud_password(&wizard.current_input) {
                        Ok(()) => {
                            wizard.verified_current = wizard.current_input.clone();
                            wizard.current_input.clear();
                            wizard.error = None;
                            wizard.step = CloudPasswordStep::Manage;
                        }
                        Err(e) => wizard.error = Some(e.to_string()),
                    }
                }
            });
        }
        CloudPasswordStep::Manage => {
            subsection_title(ui, strings::CLOUD_PWD_IS_ON_TITLE, colors);
            settings_card(ui, colors, |ui| {
                let hint_text = if state.hint.is_empty() {
                    strings::CLOUD_PWD_NO_HINT.to_owned()
                } else {
                    format!(
                        "{}: {}",
                        tr::lng_settings_cloud_password_hint_title(),
                        state.hint
                    )
                };
                let email_text = if state.has_recovery {
                    format!(
                        "{}: {}",
                        tr::lng_settings_cloud_password_email_title(),
                        state.email_pattern
                    )
                } else if !state.unconfirmed_email_pattern.is_empty() {
                    tr::lng_cloud_password_waiting_code(&state.unconfirmed_email_pattern)
                } else {
                    strings::CLOUD_PWD_EMAIL_NOT_SET.to_owned()
                };
                ui.label(
                    egui::RichText::new(hint_text)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                ui.label(
                    egui::RichText::new(email_text)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            settings_divider(ui, None, colors);

            let change_pwd_lbl = tr::lng_settings_cloud_password_manage_password_change();
            if settings_row(
                ui,
                &change_pwd_lbl,
                None,
                None,
                Some(SettingsIconKind::Key),
                false,
                false,
                colors,
            )
            .clicked()
            {
                wizard.is_changing = true;
                wizard.email_only = false;
                wizard.error = None;
                wizard.step = CloudPasswordStep::InputPassword;
            }

            let email_row_lbl = if state.has_recovery {
                tr::lng_settings_cloud_password_manage_email_change()
            } else {
                tr::lng_settings_cloud_password_manage_email_new()
            };
            if settings_row(
                ui,
                &email_row_lbl,
                None,
                if state.has_recovery {
                    Some(state.email_pattern.as_str())
                } else {
                    None
                },
                Some(SettingsIconKind::Account),
                false,
                false,
                colors,
            )
            .clicked()
            {
                wizard.is_changing = true;
                wizard.email_only = true;
                wizard.new_password = wizard.verified_current.clone();
                wizard.hint = state.hint.clone();
                wizard.error = None;
                wizard.step = CloudPasswordStep::Email;
            }

            if settings_row(
                ui,
                strings::CLOUD_PWD_TURN_OFF_ROW,
                None,
                None,
                Some(SettingsIconKind::Blocked),
                true,
                false,
                colors,
            )
            .clicked()
            {
                wizard.step = CloudPasswordStep::DisableConfirm;
            }
        }
        CloudPasswordStep::InputPassword => {
            subsection_title(
                ui,
                if wizard.is_changing {
                    strings::CLOUD_PWD_STEP1_CHANGE
                } else {
                    strings::CLOUD_PWD_STEP1_CREATE
                },
                colors,
            );
            settings_card(ui, colors, |ui| {
                InputField::single_line(
                    "cloud_pwd_new",
                    &mut wizard.new_password,
                    &tr::lng_cloud_password_enter_new(),
                    InputFieldStyle::default_input(colors),
                )
                .password(true)
                .show(ui);
                ui.add_space(6.0);
                InputField::single_line(
                    "cloud_pwd_confirm",
                    &mut wizard.confirm_password,
                    &tr::lng_cloud_password_confirm_new(),
                    InputFieldStyle::default_input(colors),
                )
                .password(true)
                .show(ui);
                render_error(ui, &wizard.error, colors);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        &tr::lng_continue(),
                        RoundButtonStyle::default_active(colors),
                    )
                    .show(ui)
                    .clicked()
                        && let Err(e) = wizard.submit_new_password()
                    {
                        wizard.error = Some(e);
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                        .show(ui)
                        .clicked()
                    {
                        wizard.reset_drafts(state.has_password);
                    }
                });
            });
        }
        CloudPasswordStep::Hint => {
            subsection_title(ui, strings::CLOUD_PWD_STEP2_HINT, colors);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_settings_cloud_password_hint_about())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
                ui.add_space(6.0);
                InputField::single_line(
                    "cloud_pwd_hint",
                    &mut wizard.hint,
                    &tr::lng_cloud_password_hint(),
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
                render_error(ui, &wizard.error, colors);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        &tr::lng_continue(),
                        RoundButtonStyle::default_active(colors),
                    )
                    .show(ui)
                    .clicked()
                        && let Err(e) = wizard.submit_hint()
                    {
                        wizard.error = Some(e);
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(
                        strings::CLOUD_PWD_SKIP_BTN,
                        RoundButtonStyle::default_light(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        wizard.hint.clear();
                        wizard.error = None;
                        wizard.step = CloudPasswordStep::Email;
                    }
                });
            });
        }
        CloudPasswordStep::Email => {
            subsection_title(ui, strings::CLOUD_PWD_STEP3_EMAIL, colors);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_settings_cloud_password_email_about())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
                ui.add_space(6.0);
                InputField::single_line(
                    "cloud_pwd_email",
                    &mut wizard.email,
                    &tr::lng_cloud_password_email(),
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
                render_error(ui, &wizard.error, colors);

                if wizard.confirm_skip_email {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(tr::lng_cloud_password_about_recover())
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("boxTextFgError")),
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        strings::CLOUD_PWD_SAVE_SEND_CODE,
                        RoundButtonStyle::default_active(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        let cur = if state.has_password {
                            Some(wizard.verified_current.as_str())
                        } else {
                            None
                        };
                        let email_trimmed = wizard.email.trim();
                        if email_trimmed.is_empty() {
                            wizard.error = Some(strings::CLOUD_PWD_ENTER_OR_SKIP.to_owned());
                        } else {
                            match backend.set_cloud_password(
                                cur,
                                &wizard.new_password,
                                &wizard.hint,
                                Some(email_trimmed),
                            ) {
                                Ok(_) => {
                                    wizard.verified_current = wizard.new_password.clone();
                                    wizard.error = None;
                                    wizard.step = CloudPasswordStep::EmailConfirm;
                                    toast = Some(format!(
                                        "{} ({MOCK_EMAIL_CONFIRM_CODE})",
                                        tr::lng_cloud_password_waiting_code(email_trimmed)
                                    ));
                                }
                                Err(e) => wizard.error = Some(e.to_string()),
                            }
                        }
                    }
                    ui.add_space(8.0);
                    if !wizard.email_only {
                        let skip_label = if wizard.confirm_skip_email {
                            strings::CLOUD_PWD_SKIP_ANYWAY_BTN
                        } else {
                            strings::CLOUD_PWD_SKIP_BTN
                        };
                        if RoundButton::new(skip_label, RoundButtonStyle::default_light(colors))
                            .show(ui)
                            .clicked()
                        {
                            if !wizard.confirm_skip_email {
                                wizard.confirm_skip_email = true;
                            } else {
                                let cur = if state.has_password {
                                    Some(wizard.verified_current.as_str())
                                } else {
                                    None
                                };
                                match backend.set_cloud_password(
                                    cur,
                                    &wizard.new_password,
                                    &wizard.hint,
                                    None,
                                ) {
                                    Ok(_) => {
                                        wizard.verified_current = wizard.new_password.clone();
                                        wizard.reset_drafts(true);
                                        toast = Some(tr::lng_cloud_password_was_set());
                                    }
                                    Err(e) => wizard.error = Some(e.to_string()),
                                }
                            }
                        }
                    } else if RoundButton::new(
                        &tr::lng_cancel(),
                        RoundButtonStyle::default_light(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        wizard.reset_drafts(state.has_password);
                    }
                });
            });
        }
        CloudPasswordStep::EmailConfirm => {
            subsection_title(ui, &tr::lng_cloud_password_confirm(), colors);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} ({MOCK_EMAIL_CONFIRM_CODE})",
                        tr::lng_cloud_password_waiting_code(&state.unconfirmed_email_pattern)
                    ))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
                );
                ui.add_space(6.0);
                InputField::single_line(
                    "cloud_pwd_code",
                    &mut wizard.email_code,
                    strings::CLOUD_PWD_CODE_PH,
                    InputFieldStyle::default_input(colors),
                )
                .max_length(6)
                .show(ui);
                render_error(ui, &wizard.error, colors);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        strings::CLOUD_PWD_CONFIRM_BTN,
                        RoundButtonStyle::default_active(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        match backend.confirm_recovery_email(&wizard.email_code) {
                            Ok(_) => {
                                wizard.reset_drafts(true);
                                toast = Some(strings::CLOUD_PWD_EMAIL_CONFIRMED_TOAST.to_owned());
                            }
                            Err(e) => wizard.error = Some(e.to_string()),
                        }
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(
                        strings::CLOUD_PWD_ABORT_EMAIL_BTN,
                        RoundButtonStyle::attention_box(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        let updated = backend.cancel_recovery_email();
                        wizard.reset_drafts(updated.has_password);
                        toast = Some(strings::CLOUD_PWD_EMAIL_CANCELLED_TOAST.to_owned());
                    }
                });
            });
        }
        CloudPasswordStep::DisableConfirm => {
            subsection_title(ui, strings::CLOUD_PWD_DISABLE_TITLE, colors);
            settings_card(ui, colors, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_settings_cloud_password_manage_disable_sure())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                render_error(ui, &wizard.error, colors);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if RoundButton::new(
                        strings::PASSCODE_TURN_OFF_BTN,
                        RoundButtonStyle::attention_box(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        match backend.disable_cloud_password(&wizard.verified_current) {
                            Ok(_) => {
                                wizard.verified_current.clear();
                                wizard.reset_drafts(false);
                                toast = Some(tr::lng_cloud_password_removed());
                            }
                            Err(e) => wizard.error = Some(e.to_string()),
                        }
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                        .show(ui)
                        .clicked()
                    {
                        wizard.step = CloudPasswordStep::Manage;
                    }
                });
            });
        }
    }

    ui.add_space(16.0);
    toast
}

fn render_error(ui: &mut Ui, error: &Option<String>, colors: &dyn Colors) {
    if let Some(msg) = error {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(msg)
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("boxTextFgError")),
        );
    }
}
