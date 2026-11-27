//! Layer stack hosting for modal boxes, main menu drawer, and toast notifications.
//!
//! Port origin: `Telegram/SourceFiles/mainwindow.cpp` (`ensureLayerCreated`,
//! `showOrHideBoxOrLayer`, `showMainMenu`, `closeLayerByBackButton`)
//! and `Telegram/SourceFiles/ui/layers/layer_widget.h`.

use std::time::{Duration, Instant};

use egui::{
    Align, Area, CornerRadius, Frame, Id, Key, Layout, Margin, Order, RichText, Stroke, TextEdit,
    Ui, Vec2,
};

use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::ConnectionStatus;

/// Modal dialog box types hosted by [`LayerStack`].
#[derive(Debug, Clone)]
pub enum ModalBox {
    /// Confirmation box (e.g. Log Out confirmation).
    ConfirmLogout,
    /// Simple informational alert box.
    Inform {
        /// Dialog heading.
        title: String,
        /// Body copy.
        body: String,
    },
    /// A Premium limit box (`SimpleLimitBox`, `boxes/premium_limits_box.cpp:400-475`): title,
    /// text, and "Increase Limit" + "Cancel" (free account) or "OK" (Premium).
    PremiumLimit {
        /// Box title.
        title: String,
        /// Box text with its bold counts.
        body: tg_ui::text::RichText,
        /// `ref` of the Premium screen "Increase Limit" opens; `None`: a single "OK".
        increase_ref: Option<String>,
    },
    /// Informational alert whose text has links (`Ui::MakeInformBox` with a
    /// `TextWithEntities`); a clicked link opens and closes the box.
    InformRich {
        /// Dialog heading (empty: none).
        title: String,
        /// Body copy with its link spans.
        body: tg_ui::text::RichText,
    },
    /// Configure or clear the local passcode lock.
    PasscodeSetup {
        /// Current input value.
        draft: String,
        /// Optional validation error.
        error: String,
        /// Whether a passcode is currently set.
        has_existing: bool,
    },
    /// Connection & proxy settings box opened from the bottom-left connection pill.
    ProxySettings,
    /// Night mode cannot change while auto-night mode follows the system
    /// (`ToggleNightModeWithConfirmation`): offers to disable it.
    ConfirmDisableAutoNight,
    /// A theme file or cloud theme from the Theme Studio is shown on trial
    /// (`Window::Theme::WarningWidget`): "Keep this theme?", reverting by itself after 16 s.
    /// Escape reverts; the scrim does not close it.
    KeepTheme {
        /// When the trial started.
        started: Instant,
    },
    /// A new interface scale takes a restart (`SetupInterfaceScale`'s `setScale`):
    /// "You need to restart... Restart now?" with "Restart" and "Cancel".
    ConfirmScaleRestart {
        /// The chosen scale (`SCALE_AUTO` or a percentage).
        scale: i32,
    },
    /// The server dropped this session (`AUTH_KEY_DROP_*`): its explanation and one "Log out"
    /// button (`Application::forceLogOut`, `core/application.cpp:1080-1096`). Escape and an
    /// outside click do not close it; closing it logs the account out.
    ForcedLogout {
        /// The server's text from `updateServiceNotification.message`, with its entities.
        text: tg_ui::text::RichText,
        /// The background account (slot) the box is for; `None`: the window's own account.
        account: Option<i32>,
    },
}

impl ModalBox {
    /// Whether Escape, the back button, an outside click or "hide all layers" may close the
    /// box (`setCloseByEscape(false)` / `setCloseByOutsideClick(false)` in tdesktop).
    #[must_use]
    pub const fn closes_by_escape(&self) -> bool {
        !matches!(self, Self::ForcedLogout { .. } | Self::KeepTheme { .. })
    }
}

/// Action emitted by the active modal box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerAction {
    /// Confirmed logout.
    ConfirmLogout,
    /// Save a new local passcode.
    SavePasscode(String),
    /// Clear the configured local passcode.
    ClearPasscode,
    /// Toggle proxy enabled state.
    SetProxyEnabled(bool),
    /// Change simulated connection status.
    SetConnectionStatus(ConnectionStatus),
    /// Disable auto-night mode and make the night-mode switch that was waiting for it.
    DisableAutoNight,
    /// The forced-logout box was closed: log the account out (`Account::forcedLogOut`); a
    /// background account carries its slot.
    ForcedLogout(Option<i32>),
    /// A link in a box's text was clicked.
    OpenLink(String),
    /// "Increase Limit" in a limit box: the Premium screen with this `ref`.
    OpenPremium(String),
    /// "Keep changes" in the theme trial box (`Window::Theme::KeepApplied`).
    KeepTheme,
    /// "Revert", Escape or the countdown in the theme trial box (`Window::Theme::Revert`).
    RevertTheme,
    /// "Restart" in the interface scale box: save this scale and restart (`Core::Restart`).
    RestartWithScale(i32),
}

/// Ephemeral toast notification displayed near the bottom center.
#[derive(Debug, Clone)]
pub struct Toast {
    /// Message text.
    pub text: String,
    /// Expiration timestamp.
    pub expires_at: Instant,
}

/// Manages the main-menu overlay flag, modal box stack, and toasts.
#[derive(Debug, Default)]
pub struct LayerStack {
    main_menu_open: bool,
    boxes: Vec<ModalBox>,
    toast: Option<Toast>,
    live_mode: bool,
}

impl LayerStack {
    /// Creates an empty layer stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures whether modal boxes render live-session copy instead of mock/demo controls.
    pub fn set_live_mode(&mut self, live_mode: bool) {
        self.live_mode = live_mode;
    }

    /// Returns `true` if any layer (main menu or modal box) is shown.
    #[must_use]
    pub fn is_layer_shown(&self) -> bool {
        self.main_menu_open || !self.boxes.is_empty()
    }

    /// Returns `true` if the hamburger main menu drawer is open.
    #[must_use]
    pub const fn is_main_menu_open(&self) -> bool {
        self.main_menu_open
    }

    /// Opens the hamburger main menu drawer.
    pub fn open_main_menu(&mut self) {
        self.main_menu_open = true;
    }

    /// Closes the hamburger main menu drawer.
    pub fn close_main_menu(&mut self) {
        self.main_menu_open = false;
    }

    /// Pushes a modal box onto the layer stack.
    pub fn show_box(&mut self, modal: ModalBox) {
        self.main_menu_open = false;
        self.boxes.push(modal);
    }

    /// Hides the top-most layer (`MainWindow::closeLayerByBackButton`).
    ///
    /// Returns `true` if a layer was closed.
    pub fn close_top_by_back(&mut self) -> bool {
        if let Some(top) = self.boxes.last() {
            if !top.closes_by_escape() {
                return false;
            }
            self.boxes.pop();
            return true;
        }
        if self.main_menu_open {
            self.main_menu_open = false;
            return true;
        }
        false
    }

    /// Hides all layers immediately (`MainWindow::ui_hideSettingsAndLayer`).
    pub fn hide_all(&mut self) {
        self.main_menu_open = false;
        self.boxes.retain(|modal| !modal.closes_by_escape());
    }

    /// Returns `true` while the forced-logout box waits for the user.
    #[must_use]
    pub fn is_forced_logout_shown(&self) -> bool {
        self.boxes
            .iter()
            .any(|modal| matches!(modal, ModalBox::ForcedLogout { .. }))
    }

    /// Returns `true` while a forced-logout box for `account` (`None`: the window's own)
    /// waits for the user.
    #[must_use]
    pub fn is_forced_logout_shown_for(&self, account: Option<i32>) -> bool {
        self.boxes.iter().any(
            |modal| matches!(modal, ModalBox::ForcedLogout { account: shown, .. } if *shown == account),
        )
    }

    /// Shows a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, text: impl Into<String>) {
        let Some(text) = tg_app_core::user_error::visible_toast_text(text.into()) else {
            return;
        };
        self.toast = Some(Toast {
            text,
            expires_at: tg_app_core::clock::now() + Duration::from_secs(3),
        });
    }

    /// Shows `text` as is, skipping the raw-error filter: renders of the old behaviour only.
    #[cfg(test)]
    pub(crate) fn show_raw_toast_for_test(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            expires_at: tg_app_core::clock::now() + Duration::from_secs(3),
        });
    }

    /// The top-most modal box, if any.
    #[cfg(test)]
    pub(crate) fn top_box(&self) -> Option<&ModalBox> {
        self.boxes.last()
    }

    /// Returns the text of the currently active toast notification, if any.
    #[must_use]
    pub fn toast_text(&self) -> Option<&str> {
        self.toast.as_ref().map(|t| t.text.as_str())
    }

    /// Renders any active modal box and toast over the main window.
    pub fn show_boxes_and_toasts(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        strings: &dyn Strings,
        connection_status: ConnectionStatus,
        proxy_enabled: bool,
    ) -> Option<LayerAction> {
        let mut action = None;

        if !self.boxes.is_empty() {
            let screen = ctx.content_rect();
            // Dimming scrim.
            let mut close_requested = false;
            Area::new(Id::unique("tg_layer_backdrop"))
                .order(Order::Middle)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                    ui.painter().rect_filled(rect, 0.0, palette.layer_backdrop);
                    if resp.clicked() {
                        close_requested = true;
                    }
                });

            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                close_requested = true;
            }
            // Escape and the scrim never close a box that refuses it; its own button does.
            if self.boxes.last().is_some_and(|top| !top.closes_by_escape()) {
                close_requested = false;
            }

            let box_width = 360.0_f32.min(screen.width() - 32.0).max(260.0);
            Area::new(Id::unique("tg_modal_box"))
                .order(Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(20))
                        .show(ui, |ui| {
                            ui.set_width(box_width);
                            if let Some(top) = self.boxes.last_mut() {
                                let (emitted, dismiss) = render_modal_box(
                                    ui,
                                    top,
                                    palette,
                                    strings,
                                    connection_status,
                                    proxy_enabled,
                                    self.live_mode,
                                );
                                if emitted.is_some() {
                                    action = emitted;
                                }
                                if dismiss {
                                    close_requested = true;
                                }
                            }
                        });
                });

            if close_requested {
                self.boxes.pop();
            }
        }

        // Render active toast if unexpired.
        let now = tg_app_core::clock::now();
        if let Some(toast) = &self.toast {
            if now >= toast.expires_at {
                self.toast = None;
            } else {
                Area::new(Id::unique("tg_toast"))
                    .order(Order::Tooltip)
                    .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -28.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        Frame::NONE
                            .fill(egui::Color32::from_rgba_unmultiplied(30, 35, 42, 232))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(16, 10))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&toast.text)
                                        .size(13.0)
                                        .color(egui::Color32::WHITE),
                                );
                            });
                    });
                ctx.request_repaint_after(Duration::from_millis(120));
            }
        }

        action
    }
}

/// Draws a box text with clickable links; a click sets [`LayerAction::OpenLink`] and returns
/// `true`.
fn box_rich_text(
    ui: &mut Ui,
    id: &str,
    text: &tg_ui::text::RichText,
    palette: &Palette,
    action: &mut Option<LayerAction>,
) -> bool {
    let colors = box_text_palette(palette);
    let style = box_text_style();
    let width = ui.available_width();
    let response = tg_ui::text::render_rich_text(
        ui,
        Id::unique(id),
        text,
        &style,
        &colors,
        palette.window_fg,
        width,
    );
    match response.clicked_link {
        Some(url) => {
            *action = Some(LayerAction::OpenLink(url));
            true
        }
        None => false,
    }
}

/// `st::boxLabel`'s palette over the window palette: links in the active text colour.
fn box_text_palette(palette: &Palette) -> tg_ui::style::TextPalette {
    tg_ui::style::TextPalette {
        link_fg: palette.window_active_text_fg,
        mono_fg: palette.window_fg,
        mark_bg: palette.layer_backdrop,
        spoiler_fg: palette.window_sub_text_fg,
        select_bg: palette.layer_backdrop,
        select_fg: palette.window_fg,
        select_link_fg: palette.window_active_text_fg,
        select_mono_fg: palette.window_fg,
        select_spoiler_fg: palette.window_sub_text_fg,
        select_overlay: palette.layer_backdrop,
        link_always_active: false,
    }
}

/// `st::boxLabel`'s text style (`Ui::MakeInformBox`).
fn box_text_style() -> tg_ui::style::TextStyle {
    tg_ui::style::TextStyle::box_label()
}

fn render_modal_box(
    ui: &mut Ui,
    modal: &mut ModalBox,
    palette: &Palette,
    strings: &dyn Strings,
    connection_status: ConnectionStatus,
    proxy_enabled: bool,
    live_mode: bool,
) -> (Option<LayerAction>, bool) {
    let mut action = None;
    let mut dismiss = false;

    match modal {
        ModalBox::ConfirmLogout => {
            ui.label(
                RichText::new(strings.menu_logout())
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);
            let logout_prompt = if live_mode {
                "Are you sure you want to log out of your Telegram account?"
            } else {
                "Are you sure you want to log out of the local mock session?"
            };
            ui.label(
                RichText::new(logout_prompt)
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let logout = ui.add(
                    egui::Button::new(
                        RichText::new(strings.menu_logout())
                            .strong()
                            .color(palette.attention_fg),
                    )
                    .frame(false),
                );
                if logout.clicked() {
                    action = Some(LayerAction::ConfirmLogout);
                    dismiss = true;
                }
                ui.add_space(12.0);
                let cancel = ui.add(
                    egui::Button::new(
                        RichText::new(strings.cancel()).color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if cancel.clicked() {
                    dismiss = true;
                }
            });
        }
        ModalBox::ConfirmDisableAutoNight => {
            ui.label(
                RichText::new(tg_lang::tr::lng_settings_auto_night_warning())
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let disable = ui.add(
                    egui::Button::new(
                        RichText::new(tg_lang::tr::lng_settings_auto_night_disable())
                            .strong()
                            .color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if disable.clicked() {
                    action = Some(LayerAction::DisableAutoNight);
                    dismiss = true;
                }
                ui.add_space(12.0);
                let cancel = ui.add(
                    egui::Button::new(
                        RichText::new(strings.cancel()).color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if cancel.clicked() {
                    dismiss = true;
                }
            });
        }
        ModalBox::KeepTheme { started } => {
            let left = keep_theme_seconds_left(
                tg_app_core::clock::now().saturating_duration_since(*started),
            );
            if left <= 0 || ui.input(|i| i.key_pressed(Key::Escape)) {
                return (Some(LayerAction::RevertTheme), true);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            ui.label(
                RichText::new(tg_lang::tr::lng_theme_sure_keep())
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);
            ui.label(
                RichText::new(tg_lang::tr::lng_theme_reverting(i64::from(left)))
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let keep = ui.add(
                    egui::Button::new(
                        RichText::new(tg_lang::tr::lng_theme_keep_changes())
                            .strong()
                            .color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if keep.clicked() {
                    action = Some(LayerAction::KeepTheme);
                    dismiss = true;
                }
                ui.add_space(12.0);
                let revert = ui.add(
                    egui::Button::new(
                        RichText::new(tg_lang::tr::lng_theme_revert())
                            .color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if revert.clicked() {
                    action = Some(LayerAction::RevertTheme);
                    dismiss = true;
                }
            });
        }
        ModalBox::ConfirmScaleRestart { scale } => {
            ui.label(
                RichText::new(tg_lang::tr::lng_settings_need_restart())
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let restart = ui.add(
                    egui::Button::new(
                        RichText::new(tg_lang::tr::lng_settings_restart_now())
                            .strong()
                            .color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if restart.clicked() {
                    action = Some(LayerAction::RestartWithScale(*scale));
                    dismiss = true;
                }
                ui.add_space(12.0);
                let cancel = ui.add(
                    egui::Button::new(
                        RichText::new(strings.cancel()).color(palette.window_active_text_fg),
                    )
                    .frame(false),
                );
                if cancel.clicked() {
                    dismiss = true;
                }
            });
        }
        ModalBox::ForcedLogout { text, account } => {
            // The box stays: the account logs out only through its button.
            let _ = box_rich_text(ui, "tg_forced_logout_text", text, palette, &mut action);
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(tg_lang::tr::lng_passcode_logout())
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    action = Some(LayerAction::ForcedLogout(*account));
                    dismiss = true;
                }
            });
        }
        ModalBox::Inform { title, body } => {
            // `Ui::MakeInformBox` has no title; an empty one is not drawn.
            if !title.is_empty() {
                ui.label(
                    RichText::new(title.as_str())
                        .size(17.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(10.0);
            }
            ui.label(
                RichText::new(body.as_str())
                    .size(13.5)
                    .color(palette.window_fg),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(strings.close())
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    dismiss = true;
                }
            });
        }
        ModalBox::PremiumLimit {
            title,
            body,
            increase_ref,
        } => {
            ui.label(
                RichText::new(title.as_str())
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);
            let _ = box_rich_text(ui, "tg_premium_limit_text", body, palette, &mut action);
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let primary = increase_ref
                    .as_ref()
                    .map_or_else(tg_lang::tr::lng_box_ok, |_| {
                        tg_lang::tr::lng_limits_increase()
                    });
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(primary)
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    if let Some(referral) = increase_ref.clone() {
                        action = Some(LayerAction::OpenPremium(referral));
                    }
                    dismiss = true;
                }
                if increase_ref.is_some() {
                    ui.add_space(12.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(tg_lang::tr::lng_cancel())
                                    .color(palette.window_active_text_fg),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        dismiss = true;
                    }
                }
            });
        }
        ModalBox::InformRich { title, body } => {
            if !title.is_empty() {
                ui.label(
                    RichText::new(title.as_str())
                        .size(17.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(10.0);
            }
            if box_rich_text(ui, "tg_inform_rich_text", body, palette, &mut action) {
                dismiss = true;
            }
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(strings.close())
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    dismiss = true;
                }
            });
        }
        ModalBox::PasscodeSetup {
            draft,
            error,
            has_existing,
        } => {
            ui.label(
                RichText::new("Local Passcode Lock")
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);
            let passcode_copy = if live_mode {
                "Set a local passcode to lock Rustgram (Ctrl+L or Main Menu -> Lock)."
            } else {
                "Set a local passcode to test the window_lock_widgets screen (Ctrl+L or Main Menu -> Lock)."
            };
            ui.label(
                RichText::new(passcode_copy)
                    .size(13.0)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(12.0);
            ui.add_sized(
                Vec2::new(ui.available_width(), 34.0),
                TextEdit::singleline(draft)
                    .password(true)
                    .hint_text("Enter new passcode (e.g. 1234)"),
            );
            if !error.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(error.as_str())
                        .size(12.5)
                        .color(palette.attention_fg),
                );
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Save")
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    if draft.trim().is_empty() {
                        *error = "Passcode cannot be empty.".to_owned();
                    } else {
                        action = Some(LayerAction::SavePasscode(draft.clone()));
                        dismiss = true;
                    }
                }
                if *has_existing {
                    ui.add_space(10.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Disable").color(palette.attention_fg))
                                .frame(false),
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::ClearPasscode);
                        dismiss = true;
                    }
                }
                ui.add_space(10.0);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(strings.cancel()).color(palette.window_sub_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    dismiss = true;
                }
            });
        }
        ModalBox::ProxySettings => {
            ui.label(
                RichText::new("Connection & Proxy Settings")
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);
            let mut proxy_checked = proxy_enabled;
            if ui
                .checkbox(
                    &mut proxy_checked,
                    "Use proxy (shows bottom-left shield pill)",
                )
                .changed()
            {
                action = Some(LayerAction::SetProxyEnabled(proxy_checked));
            }
            if !live_mode {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Simulate connection state:")
                        .size(13.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            matches!(connection_status, ConnectionStatus::Connected),
                            "Connected",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::SetConnectionStatus(
                            ConnectionStatus::Connected,
                        ));
                    }
                    if ui
                        .selectable_label(
                            matches!(connection_status, ConnectionStatus::Connecting),
                            "Connecting",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::SetConnectionStatus(
                            ConnectionStatus::Connecting,
                        ));
                    }
                    if ui
                        .selectable_label(
                            matches!(connection_status, ConnectionStatus::Waiting { .. }),
                            "Waiting (8s)",
                        )
                        .clicked()
                    {
                        action = Some(LayerAction::SetConnectionStatus(
                            ConnectionStatus::Waiting { remaining_ms: 8000 },
                        ));
                    }
                });
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(strings.close())
                                .strong()
                                .color(palette.window_active_text_fg),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    dismiss = true;
                }
            });
        }
    }

    (action, dismiss)
}

/// `kWaitBeforeRevertMs` of `window_theme_warning.cpp`.
const KEEP_THEME_WAIT_MS: u128 = 15_999;

/// Whole seconds left before a theme trial reverts (`WarningWidget::handleTimer`); `0` or
/// less means revert now.
#[must_use]
pub fn keep_theme_seconds_left(elapsed: Duration) -> i32 {
    let left_ms = KEEP_THEME_WAIT_MS.saturating_sub(elapsed.as_millis());
    i32::try_from(left_ms / 1000).unwrap_or(i32::MAX)
}
