//! Login email setup step (`Intro::EmailWidget`).
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_email.h` and
//! `Telegram/SourceFiles/intro/intro_email.cpp`.

use std::time::Instant;

use egui::{
    Align, Color32, CornerRadius, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;

use crate::intro::style::{
    INTRO_NEXT_BUTTON_WIDTH, ShakeAnimation, primary_intro_button, underline_input,
};
use tg_app_core::stand_in::theme::Palette;

/// Actions emitted by [`EmailStepState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailStepAction {
    /// Submit the entered login email address (`account.sendVerifyEmailCode`).
    Submit(String),
}

/// UI state for `LoginStep::EmailSetup` (`Intro::EmailWidget`).
#[derive(Debug)]
pub struct EmailStepState {
    /// Email address text field value.
    pub email_input: String,
    /// Shake animation on invalid / disallowed email error.
    pub shake: ShakeAnimation,
    /// Focus request when step activates.
    pub request_focus: bool,
}

impl Default for EmailStepState {
    fn default() -> Self {
        Self::new()
    }
}

impl EmailStepState {
    /// Creates a new `EmailStepState`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            email_input: String::new(),
            shake: ShakeAnimation::default(),
            request_focus: true,
        }
    }

    /// Resets the email step state.
    pub fn reset(&mut self) {
        self.email_input.clear();
        self.shake.stop();
        self.request_focus = true;
    }

    /// Triggers input shake and refocuses the email field.
    pub fn trigger_error(&mut self, now: Instant) {
        self.shake.trigger(now);
        self.request_focus = true;
    }

    /// Renders the Login Email Setup step (`EmailWidget`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card_width: f32,
        error_text: &str,
        now: Instant,
        palette: &Palette,
    ) -> Option<EmailStepAction> {
        let mut action = None;
        let width = card_width.min(INTRO_NEXT_BUTTON_WIDTH);
        let shake_dx = self.shake.offset(now);
        if self.shake.is_animating(now) {
            ui.ctx().request_repaint();
        }

        ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
            ui.label(
                RichText::new(tr::lng_intro_email_setup_title())
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(tr::lng_settings_cloud_login_email_about())
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(16.0);

            // Stylized envelope emblem (replaces cloud_password/email lottie in intro_email.cpp:59)
            ui.allocate_ui_with_layout(
                Vec2::new(width, 56.0),
                Layout::top_down(Align::Center),
                |ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
                    let painter = ui.painter();
                    let circle_bg = Color32::from_rgba_unmultiplied(
                        palette.window_bg_active.r(),
                        palette.window_bg_active.g(),
                        palette.window_bg_active.b(),
                        32,
                    );
                    painter.circle_filled(rect.center(), 26.0, circle_bg);
                    let env = Rect::from_center_size(rect.center(), Vec2::new(26.0, 18.0));
                    painter.rect_stroke(
                        env,
                        CornerRadius::same(3),
                        Stroke::new(1.8, palette.window_bg_active),
                        egui::StrokeKind::Inside,
                    );
                    painter.line_segment(
                        [
                            Pos2::new(env.left() + 2.0, env.top() + 2.0),
                            Pos2::new(env.center().x, env.center().y + 1.0),
                        ],
                        Stroke::new(1.6, palette.window_bg_active),
                    );
                    painter.line_segment(
                        [
                            Pos2::new(env.right() - 2.0, env.top() + 2.0),
                            Pos2::new(env.center().x, env.center().y + 1.0),
                        ],
                        Stroke::new(1.6, palette.window_bg_active),
                    );
                },
            );

            ui.add_space(12.0);
            let resp = underline_input(
                ui,
                "intro_email_setup_field",
                &mut self.email_input,
                &tr::lng_settings_cloud_login_email_placeholder(),
                width,
                false,
                !error_text.is_empty(),
                shake_dx,
                palette,
            );
            if self.request_focus {
                resp.request_focus();
                self.request_focus = false;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

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

            if primary_intro_button(ui, width, &tr::lng_intro_next(), palette).clicked() || enter {
                let trimmed = self.email_input.trim().to_owned();
                if trimmed.is_empty() {
                    self.trigger_error(now);
                } else {
                    action = Some(EmailStepAction::Submit(trimmed));
                }
            }
        });

        action
    }
}
