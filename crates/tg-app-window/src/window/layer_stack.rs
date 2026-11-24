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
        if self.boxes.pop().is_some() {
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
        self.boxes.clear();
    }

    /// Shows a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            expires_at: tg_app_core::clock::now() + Duration::from_secs(3),
        });
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
        ModalBox::Inform { title, body } => {
            ui.label(
                RichText::new(title.as_str())
                    .size(17.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(10.0);
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
