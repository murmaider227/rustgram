//! Intro error and security modal boxes: `PhoneBannedBox` and `UnconfirmedAuthDeniedBox`.
//!
//! Port origin: `Telegram/SourceFiles/boxes/phone_banned_box.h`,
//! `Telegram/SourceFiles/boxes/phone_banned_box.cpp`,
//! `Telegram/SourceFiles/boxes/unconfirmed_auth_denied_box.h`,
//! and `Telegram/SourceFiles/boxes/unconfirmed_auth_denied_box.cpp`.

use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Key, Layout, Margin, Order, Pos2, RichText,
    Sense, Stroke, Vec2,
};
use tg_lang::tr;

use tg_app_core::stand_in::theme::Palette;

/// Support email recipient used by `SendToBannedHelp` in `phone_banned_box.cpp:39`.
pub const PHONE_BANNED_HELP_EMAIL: &str = "login@stel.com";

/// Countdown lock duration in seconds matching `kTimer = 5` in `unconfirmed_auth_denied_box.cpp:135`.
pub const UNCONFIRMED_AUTH_TIMER_SECS: u32 = 5;

/// Percent-encodes a query component matching `qthelp::url_encode` in `phone_banned_box.cpp`.
#[must_use]
pub fn url_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for &b in input.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Constructs the exact `mailto:` support link matching `SendToBannedHelp` (`phone_banned_box.cpp:22-46`).
#[must_use]
pub fn build_phone_banned_mailto(
    phone: &str,
    app_version: &str,
    os_version: &str,
    locale: &str,
) -> String {
    let subject = format!("Banned phone number: {phone}");
    let body = format!(
        "I'm trying to use my mobile phone number: {phone}\n\
But Telegram says it's banned. Please help.\n\
\n\
App version: {app_version}\n\
OS version: {os_version}\n\
Locale: {locale}"
    );
    format!(
        "mailto:?to={}&subject={}&body={}",
        url_encode_component(PHONE_BANNED_HELP_EMAIL),
        url_encode_component(&subject),
        url_encode_component(&body),
    )
}

/// Actions returned by [`PhoneBannedBoxState::show_modal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneBannedAction {
    /// User dismissed the box (`OK` / Escape / outside click).
    Dismiss,
    /// User clicked `Help` to open the prefilled support email (`mailto:` URL).
    OpenHelp(String),
}

/// Modal box shown when `auth.sendCode` / `auth.signIn` returns `PHONE_NUMBER_BANNED`
/// (`Ui::ShowPhoneBannedError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneBannedBoxState {
    /// Formatted phone number that was rejected.
    pub phone: String,
    /// Localized body text (`tr::lng_signin_banned_text()`).
    pub body_text: String,
    /// App version included in the support mailto body.
    pub app_version: String,
    /// OS description included in the support mailto body.
    pub os_version: String,
    /// Locale code included in the support mailto body.
    pub locale: String,
    /// Last generated help mailto URL (populated when Help is clicked).
    pub last_help_url: Option<String>,
}

impl PhoneBannedBoxState {
    /// Creates a [`PhoneBannedBoxState`] for `phone` with default localized text.
    #[must_use]
    pub fn new(phone: impl Into<String>) -> Self {
        Self {
            phone: phone.into(),
            body_text: tr::lng_signin_banned_text(),
            app_version: tg_app_core::core_types::misc::TGFORK_VERSION.to_owned(),
            os_version: std::env::consts::OS.to_owned(),
            locale: "en".to_owned(),
            last_help_url: None,
        }
    }

    /// Builds the prefilled `mailto:` URL for [`Self::phone`].
    #[must_use]
    pub fn help_mailto_url(&self) -> String {
        build_phone_banned_mailto(
            &self.phone,
            &self.app_version,
            &self.os_version,
            &self.locale,
        )
    }

    /// Renders the centered `PhoneBannedBox` modal overlay over the intro window.
    pub fn show_modal(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
    ) -> Option<PhoneBannedAction> {
        let screen = ctx.content_rect();
        let mut action = None;

        egui::Area::new(Id::unique("intro_phone_banned_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (_, bg_resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, palette.layer_backdrop);
                if bg_resp.clicked() {
                    action = Some(PhoneBannedAction::Dismiss);
                }
            });

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(PhoneBannedAction::Dismiss);
        }

        let box_w = 340.0_f32.min(screen.width() - 32.0);
        let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 96.0);

        egui::Area::new(Id::unique("intro_phone_banned_box"))
            .order(Order::Tooltip)
            .fixed_pos(box_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, palette.divider))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_min_width(box_w - 40.0);
                        ui.set_max_width(box_w - 40.0);

                        ui.label(
                            RichText::new("Phone Number Banned")
                                .size(16.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(&self.body_text)
                                .size(14.0)
                                .color(palette.window_fg),
                        );
                        if !self.phone.trim().is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(&self.phone)
                                    .size(13.0)
                                    .color(palette.window_sub_text_fg),
                            );
                        }
                        ui.add_space(16.0);

                        ui.horizontal(|ui| {
                            let help_lbl = tr::lng_signin_banned_help();
                            if ui
                                .button(
                                    RichText::new(help_lbl)
                                        .size(14.0)
                                        .strong()
                                        .color(palette.window_active_text_fg),
                                )
                                .clicked()
                            {
                                let url = self.help_mailto_url();
                                self.last_help_url = Some(url.clone());
                                action = Some(PhoneBannedAction::OpenHelp(url));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .button(
                                        RichText::new(tr::lng_box_ok())
                                            .size(14.0)
                                            .strong()
                                            .color(palette.window_active_text_fg),
                                    )
                                    .clicked()
                                {
                                    action = Some(PhoneBannedAction::Dismiss);
                                }
                            });
                        });
                    });
            });

        action
    }
}

/// Modal box shown when one or more unconfirmed login attempts or bot connections
/// were denied (`Dialogs::ShowAuthDeniedBox`, `unconfirmed_auth_denied_box.cpp`).
///
/// Enforces tdesktop's 5-second countdown lock before the user can dismiss the warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedAuthDeniedBoxState {
    /// Number of denied authorizations or bot connections.
    pub count: u32,
    /// Detailed explanation text describing which session/device was blocked.
    pub message_text: String,
    /// `true` if the denied connection was a bot login.
    pub bot: bool,
    /// `true` if both regular logins and bot connections were denied together.
    pub mixed: bool,
    /// Remaining countdown seconds before the confirm button unlocks (`5..=0`).
    pub remaining_secs: u32,
    /// Accumulated sub-second elapsed time in milliseconds for deterministic stepping.
    elapsed_ms: u32,
}

impl UnconfirmedAuthDeniedBoxState {
    /// Creates an [`UnconfirmedAuthDeniedBoxState`] starting with the 5-second lock timer.
    #[must_use]
    pub fn new(count: u32, message_text: impl Into<String>, bot: bool, mixed: bool) -> Self {
        Self {
            count: count.max(1),
            message_text: message_text.into(),
            bot,
            mixed,
            remaining_secs: UNCONFIRMED_AUTH_TIMER_SECS,
            elapsed_ms: 0,
        }
    }

    /// Returns `true` once the 5-second safety countdown has finished.
    #[must_use]
    pub const fn can_dismiss(&self) -> bool {
        self.remaining_secs == 0
    }

    /// Advances the countdown clock by `delta_ms` milliseconds.
    pub fn tick_ms(&mut self, delta_ms: u32) {
        if self.remaining_secs == 0 {
            return;
        }
        self.elapsed_ms = self.elapsed_ms.saturating_add(delta_ms);
        let whole_secs = self.elapsed_ms / 1_000;
        if whole_secs > 0 {
            self.elapsed_ms %= 1_000;
            self.remaining_secs = self.remaining_secs.saturating_sub(whole_secs);
        }
    }

    /// Returns the localized heading matching `unconfirmed_auth_denied_box.cpp:61-67`.
    #[must_use]
    pub fn title(&self) -> String {
        if (self.bot || self.mixed) && self.count > 1 {
            tr::lng_unconfirmed_auth_connections_denied()
        } else if self.bot {
            tr::lng_unconfirmed_auth_denied_title_bot()
        } else {
            tr::lng_unconfirmed_auth_denied_title(i64::from(self.count))
        }
    }

    /// Returns the highlighted security warning copy matching `unconfirmed_auth_denied_box.cpp:83-85`.
    #[must_use]
    pub fn warning_text(&self) -> String {
        if self.bot {
            tr::lng_unconfirmed_auth_denied_warning_bot()
        } else {
            tr::lng_unconfirmed_auth_denied_warning()
        }
    }

    /// Renders the centered `UnconfirmedAuthDeniedBox` modal overlay.
    ///
    /// Returns `true` when the user dismisses the box after the countdown reaches `0`.
    pub fn show_modal(&mut self, ctx: &egui::Context, palette: &Palette) -> bool {
        let dt_ms = (ctx.input(|i| i.unstable_dt).clamp(0.0, 0.25) * 1_000.0).round() as u32;
        if self.remaining_secs > 0 {
            self.tick_ms(dt_ms);
            if self.remaining_secs > 0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }

        let screen = ctx.content_rect();
        let mut dismissed = false;

        egui::Area::new(Id::unique("intro_unconfirmed_auth_denied_backdrop"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (_, bg_resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, palette.layer_backdrop);
                if bg_resp.clicked() && self.can_dismiss() {
                    dismissed = true;
                }
            });

        if self.can_dismiss() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            dismissed = true;
        }

        let box_w = 390.0_f32.min(screen.width() - 32.0);
        let box_pos = Pos2::new(screen.center().x - box_w * 0.5, screen.center().y - 165.0);

        egui::Area::new(Id::unique("intro_unconfirmed_auth_denied_box"))
            .order(Order::Tooltip)
            .fixed_pos(box_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(10))
                    .stroke(Stroke::new(1.0, palette.divider))
                    .inner_margin(Margin::same(22))
                    .show(ui, |ui| {
                        ui.set_min_width(box_w - 44.0);
                        ui.set_max_width(box_w - 44.0);

                        // Circular attention ban emblem matching `AddLottieIconWithCircle("ban")`
                        let (emblem_rect, _) =
                            ui.allocate_exact_size(Vec2::new(box_w - 44.0, 62.0), Sense::hover());
                        let c = emblem_rect.center();
                        let att_tint = Color32::from_rgba_unmultiplied(
                            palette.attention_fg.r(),
                            palette.attention_fg.g(),
                            palette.attention_fg.b(),
                            36,
                        );
                        ui.painter().circle_filled(c, 28.0, att_tint);
                        ui.painter()
                            .circle_stroke(c, 16.0, Stroke::new(2.5, palette.attention_fg));
                        ui.painter().line_segment(
                            [
                                Pos2::new(c.x - 10.5, c.y - 10.5),
                                Pos2::new(c.x + 10.5, c.y + 10.5),
                            ],
                            Stroke::new(2.5, palette.attention_fg),
                        );

                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(self.title())
                                    .size(17.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                        });

                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(&self.message_text)
                                .size(13.5)
                                .color(palette.window_fg),
                        );

                        ui.add_space(12.0);
                        // Highlighted warning card (`attentionButtonBgOver` + `attentionButtonFg`)
                        let warn_bg = Color32::from_rgba_unmultiplied(
                            palette.attention_fg.r(),
                            palette.attention_fg.g(),
                            palette.attention_fg.b(),
                            26,
                        );
                        Frame::NONE
                            .fill(warn_bg)
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_min_width(box_w - 68.0);
                                ui.label(
                                    RichText::new(self.warning_text())
                                        .size(13.0)
                                        .strong()
                                        .color(palette.attention_fg),
                                );
                            });

                        ui.add_space(16.0);
                        let base_lbl = tr::lng_archive_hint_button();
                        let btn_label = if self.remaining_secs > 0 {
                            format!("{base_lbl}  {}", self.remaining_secs)
                        } else {
                            base_lbl
                        };
                        let (btn_rect, btn_resp) = ui.allocate_exact_size(
                            Vec2::new(box_w - 44.0, 40.0),
                            if self.can_dismiss() {
                                Sense::click()
                            } else {
                                Sense::hover()
                            },
                        );
                        let fill = if self.can_dismiss() {
                            palette.window_bg_active
                        } else {
                            Color32::from_rgba_unmultiplied(
                                palette.window_bg_active.r(),
                                palette.window_bg_active.g(),
                                palette.window_bg_active.b(),
                                130,
                            )
                        };
                        ui.painter()
                            .rect_filled(btn_rect, CornerRadius::same(8), fill);
                        ui.painter().text(
                            btn_rect.center(),
                            Align2::CENTER_CENTER,
                            btn_label,
                            egui::FontId::proportional(14.0),
                            Color32::WHITE,
                        );
                        if self.can_dismiss() && btn_resp.clicked() {
                            dismissed = true;
                        }
                    });
            });

        dismissed
    }
}
