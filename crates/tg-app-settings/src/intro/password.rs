//! Two-step verification cloud password, email recovery, and account reset step.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_password_check.cpp` and
//! `Telegram/SourceFiles/intro/intro_widget.cpp` (`showResetButton`, `resetAccount`).

use std::time::Instant;

use egui::{
    Align, CornerRadius, Frame, Id, Key, Layout, Margin, Order, Pos2, RichText, Sense, Stroke, Ui,
    Vec2,
};
use tg_api::login::{CloudPasswordState, PasswordCheckMode};
use tg_lang::tr;

use crate::intro::phone::show_simple_inform_modal;
use crate::intro::style::{
    INTRO_NEXT_BUTTON_WIDTH, ShakeAnimation, attention_light_button, link_button,
    primary_intro_button, underline_input,
};
use tg_app_core::stand_in::theme::Palette;

/// Formats `2FA_CONFIRM_WAIT_<seconds>` using tdesktop's exact day/hour/minute breakdown
/// (`Intro::Widget::resetAccount` in `intro_widget.cpp:573-615`).
#[must_use]
pub fn format_reset_confirm_wait(formatted_phone: &str, seconds: u32) -> String {
    let adjusted = seconds.saturating_add(59);
    let days = i64::from(adjusted / 86_400);
    let hours = i64::from((adjusted % 86_400) / 3_600);
    let minutes = i64::from((adjusted % 3_600) / 60);
    let minutes_str = tr::lng_minutes(minutes);
    let when = if days > 0 {
        let days_str = tr::lng_days(days);
        let hours_str = tr::lng_hours(hours);
        tr::lng_signin_reset_in_days(&days_str, &hours_str, &minutes_str)
    } else if hours > 0 {
        let hours_str = tr::lng_hours(hours);
        tr::lng_signin_reset_in_hours(&hours_str, &minutes_str)
    } else {
        minutes_str
    };
    tr::lng_signin_reset_wait(formatted_phone, &when)
}

/// Active modal box inside the Password step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordModal {
    /// Inform modal shown when `has_recovery == false` (`lng_signin_no_email_forgot`).
    NoEmailForgot,
    /// Inform modal shown when user clicks `"Unable to access your email?"` (`lng_signin_cant_email_forgot`).
    CantEmailForgot,
    /// Confirmation modal warning that Telegram Passport data will be lost on recovery.
    PassportWarning {
        /// Recovery code waiting to be checked.
        code: String,
    },
    /// Confirmation dialog before calling `account.deleteAccount` (`lng_signin_sure_reset`).
    ConfirmReset,
    /// Inform dialog showing `2FA_CONFIRM_WAIT_X` or `2FA_RECENT_CONFIRM`.
    ResetNotice(String),
}

/// Actions emitted by [`PasswordStepState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordStepAction {
    /// Submit 2FA cloud password for SRP check.
    SubmitPassword(String),
    /// User clicked `"Forgot password?"` when recovery email is available.
    RequestRecovery,
    /// Submit 2FA email recovery code.
    SubmitRecoveryCode(String),
    /// Finalize 2FA password recovery after code verification.
    CompleteRecovery,
    /// User confirmed destructive account reset (`account.deleteAccount`).
    ConfirmAccountReset,
}

/// UI state for `LoginStep::PasswordCheck` (`Intro::PasswordCheckWidget`).
#[derive(Debug)]
pub struct PasswordStepState {
    /// Current plaintext password input buffer.
    pub password_input: String,
    /// Whether the password field is temporarily revealed.
    pub show_password: bool,
    /// Current email recovery code input buffer.
    pub recovery_code_input: String,
    /// Whether the bottom red `"Reset your account"` button has been unlocked.
    pub reset_button_visible: bool,
    /// Active modal overlay, if any.
    pub modal: Option<PasswordModal>,
    /// Horizontal shake animation on invalid password or recovery code.
    pub shake: ShakeAnimation,
    /// Focus request on entering step or after error.
    pub request_focus: bool,
}

impl Default for PasswordStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordStepState {
    /// Creates a new `PasswordStepState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            password_input: String::new(),
            show_password: false,
            recovery_code_input: String::new(),
            reset_button_visible: false,
            modal: None,
            shake: ShakeAnimation::default(),
            request_focus: true,
        }
    }

    /// Resets all inputs and modals when entering a fresh `PasswordCheck` step.
    pub fn reset(&mut self) {
        self.password_input.clear();
        self.show_password = false;
        self.recovery_code_input.clear();
        self.reset_button_visible = false;
        self.modal = None;
        self.shake.stop();
        self.request_focus = true;
    }

    /// Triggers input shake and refocuses the active input.
    pub fn trigger_error(&mut self, now: Instant) {
        self.shake.trigger(now);
        self.request_focus = true;
    }

    /// Reveals the bottom `"Reset your account"` button (`PasswordCheckWidget::showReset`).
    pub fn reveal_reset_button(&mut self) {
        self.reset_button_visible = true;
        self.recovery_code_input.clear();
        self.request_focus = true;
    }

    /// Renders the 2FA password / recovery step (`PasswordCheckWidget`).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        password_state: &CloudPasswordState,
        mode: &PasswordCheckMode,
        error_text: &str,
        now: Instant,
        palette: &Palette,
    ) -> Option<PasswordStepAction> {
        let mut action = None;
        let width = card_width.min(INTRO_NEXT_BUTTON_WIDTH);
        let shake_dx = self.shake.offset(now);
        if self.shake.is_animating(now) {
            ui.ctx().request_repaint();
        }

        let in_recovery = matches!(
            mode,
            PasswordCheckMode::Recovery { .. } | PasswordCheckMode::RecoveryVerified { .. }
        );

        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
            ui.label(
                RichText::new(tr::lng_signin_title())
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);

            let desc = match mode {
                PasswordCheckMode::Password => tr::lng_signin_desc(),
                PasswordCheckMode::Recovery { email_pattern } => {
                    tr::lng_signin_recover_desc(email_pattern)
                }
                PasswordCheckMode::RecoveryVerified { .. } => tr::lng_signin_recover_title(),
            };
            ui.label(
                RichText::new(desc)
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(20.0);

            let mut submitted_via_enter = false;
            match mode {
                PasswordCheckMode::Password => {
                    let resp = underline_input(
                        ui,
                        "intro_password_field",
                        &mut self.password_input,
                        &tr::lng_signin_password(),
                        width,
                        !self.show_password,
                        !error_text.is_empty(),
                        shake_dx,
                        palette,
                    );
                    if self.request_focus && self.modal.is_none() {
                        resp.request_focus();
                        self.request_focus = false;
                    }
                    submitted_via_enter =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

                    ui.add_space(6.0);
                    if !password_state.hint.is_empty() {
                        ui.label(
                            RichText::new(tr::lng_signin_hint(&password_state.hint))
                                .size(13.0)
                                .color(palette.window_fg),
                        );
                    }
                    ui.add_space(12.0);
                    if link_button(ui, &tr::lng_signin_recover(), palette).clicked() {
                        if password_state.has_recovery {
                            action = Some(PasswordStepAction::RequestRecovery);
                        } else {
                            self.modal = Some(PasswordModal::NoEmailForgot);
                        }
                    }
                }
                PasswordCheckMode::Recovery { .. } => {
                    let resp = underline_input(
                        ui,
                        "intro_recovery_code_field",
                        &mut self.recovery_code_input,
                        &tr::lng_signin_code(),
                        width,
                        false,
                        !error_text.is_empty(),
                        shake_dx,
                        palette,
                    );
                    if self.request_focus && self.modal.is_none() {
                        resp.request_focus();
                        self.request_focus = false;
                    }
                    submitted_via_enter =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

                    ui.add_space(16.0);
                    if link_button(ui, &tr::lng_signin_try_password(), palette).clicked() {
                        self.modal = Some(PasswordModal::CantEmailForgot);
                    }
                }
                PasswordCheckMode::RecoveryVerified { .. } => {
                    ui.label(
                        RichText::new(tr::lng_signin_recover_title())
                            .size(14.0)
                            .color(palette.window_active_text_fg),
                    );
                    ui.add_space(20.0);
                }
            }

            // Error slot
            ui.add_space(10.0);
            ui.allocate_ui_with_layout(
                Vec2::new(width, 20.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    if !error_text.is_empty() {
                        ui.label(
                            RichText::new(error_text)
                                .size(12.5)
                                .color(palette.attention_fg),
                        );
                    }
                },
            );
            ui.add_space(10.0);

            // Submit button (`tr::lng_intro_submit()`)
            let submit_btn = primary_intro_button(ui, width, &tr::lng_intro_submit(), palette);
            if submit_btn.clicked() || submitted_via_enter {
                match mode {
                    PasswordCheckMode::Password => {
                        if self.password_input.is_empty() {
                            self.trigger_error(now);
                        } else {
                            action = Some(PasswordStepAction::SubmitPassword(
                                self.password_input.clone(),
                            ));
                        }
                    }
                    PasswordCheckMode::Recovery { .. } => {
                        let code = self.recovery_code_input.trim().to_owned();
                        if code.is_empty() {
                            self.trigger_error(now);
                        } else if password_state.not_empty_passport {
                            self.modal = Some(PasswordModal::PassportWarning { code });
                        } else {
                            action = Some(PasswordStepAction::SubmitRecoveryCode(code));
                        }
                    }
                    PasswordCheckMode::RecoveryVerified { .. } => {
                        action = Some(PasswordStepAction::CompleteRecovery);
                    }
                }
            }

            // Bottom "Reset your account" button (`Intro::Widget::showResetButton`)
            if self.reset_button_visible && !in_recovery {
                ui.add_space(26.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(width, 38.0),
                    Layout::top_down(Align::Center),
                    |ui| {
                        if attention_light_button(ui, &tr::lng_signin_reset_account(), palette)
                            .clicked()
                        {
                            self.modal = Some(PasswordModal::ConfirmReset);
                        }
                    },
                );
            }
        });

        // Modals
        if let Some(modal) = self.modal.clone() {
            match modal {
                PasswordModal::NoEmailForgot => {
                    if show_simple_inform_modal(
                        ui.ctx(),
                        "intro_no_email_forgot",
                        &tr::lng_signin_no_email_forgot(),
                        palette,
                    ) {
                        self.modal = None;
                        self.reveal_reset_button();
                    }
                }
                PasswordModal::CantEmailForgot => {
                    if show_simple_inform_modal(
                        ui.ctx(),
                        "intro_cant_email_forgot",
                        &tr::lng_signin_cant_email_forgot(),
                        palette,
                    ) {
                        self.modal = None;
                        self.reveal_reset_button();
                    }
                }
                PasswordModal::ResetNotice(msg) => {
                    if show_simple_inform_modal(ui.ctx(), "intro_reset_notice", &msg, palette) {
                        self.modal = None;
                    }
                }
                PasswordModal::PassportWarning { code } => {
                    match show_confirm_modal(
                        ui.ctx(),
                        "intro_passport_warning",
                        &tr::lng_cloud_password_passport_losing(),
                        &tr::lng_continue(),
                        false,
                        palette,
                    ) {
                        Some(true) => {
                            self.modal = None;
                            action = Some(PasswordStepAction::SubmitRecoveryCode(code));
                        }
                        Some(false) => {
                            self.modal = None;
                        }
                        None => {}
                    }
                }
                PasswordModal::ConfirmReset => {
                    match show_confirm_modal(
                        ui.ctx(),
                        "intro_confirm_reset",
                        &tr::lng_signin_sure_reset(),
                        &tr::lng_signin_reset(),
                        true,
                        palette,
                    ) {
                        Some(true) => {
                            self.modal = None;
                            action = Some(PasswordStepAction::ConfirmAccountReset);
                        }
                        Some(false) => {
                            self.modal = None;
                        }
                        None => {}
                    }
                }
            }
        }

        action
    }
}

/// Renders a two-button confirm modal (`Ui::MakeConfirmBox`). Returns `Some(true)` on confirm,
/// `Some(false)` on cancel, or `None` while still open.
pub(crate) fn show_confirm_modal(
    ctx: &egui::Context,
    id_salt: &str,
    message: &str,
    confirm_label: &str,
    destructive: bool,
    palette: &Palette,
) -> Option<bool> {
    let screen = ctx.content_rect();
    let mut result = None;
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        result = Some(false);
    }

    egui::Area::new(Id::unique(format!("{id_salt}_backdrop")))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, palette.layer_backdrop);
            if resp.clicked() {
                result = Some(false);
            }
        });

    let box_w = 350.0_f32.min(screen.width() - 32.0);
    let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 100.0);

    egui::Area::new(Id::unique(id_salt))
        .order(Order::Tooltip)
        .fixed_pos(box_pos)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(palette.window_bg)
                .corner_radius(CornerRadius::same(8))
                .stroke(Stroke::new(1.0, palette.divider))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(box_w - 40.0);
                    ui.label(RichText::new(message).size(14.0).color(palette.window_fg));
                    ui.add_space(20.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let confirm_color = if destructive {
                            palette.attention_fg
                        } else {
                            palette.window_active_text_fg
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(confirm_label.to_uppercase())
                                        .size(13.0)
                                        .strong()
                                        .color(confirm_color),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            result = Some(true);
                        }
                        ui.add_space(16.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(tr::lng_cancel().to_uppercase())
                                        .size(13.0)
                                        .strong()
                                        .color(palette.window_active_text_fg),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            result = Some(false);
                        }
                    });
                });
        });

    result
}
