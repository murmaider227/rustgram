//! Greeting Messages, Away Messages, Chatbots, and Business Chat Links screens.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/business/settings_greeting.cpp`
//! - `Telegram/SourceFiles/settings/business/settings_away_message.cpp`
//! - `Telegram/SourceFiles/settings/business/settings_chatbots.cpp`
//! - `Telegram/SourceFiles/settings/business/settings_chat_links.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use qrcode::{Color as QrColor, QrCode};

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    AWAY_START_TIME_MIN, AwaySchedule, AwayScheduleType, AwaySettings, BotLookupState,
    BotSearchResult, BusinessChatLink, BusinessRecipientsType, BusinessShortcutId, BusinessUserRef,
    CHAT_LINKS_LIMIT, ChatbotsPermissions, ChatbotsSettings, DEFAULT_NO_ACTIVITY_DAYS,
    GREETING_PERIOD_CHOICES, GreetingSettings, WorkingInterval, chatbots_permission_groups,
};
use crate::business::ui_helpers::{
    BUSINESS_BLUE, BUSINESS_TEAL, PREMIUM_INDIGO, RecipientsPickerUiState, SUCCESS_GREEN,
    business_section_intro, divider_caption, format_unix_datetime, paint_avatar, pill_button,
    primary_button, radio_row, render_recipients_selector, section_divider, settings_toggle_row,
    subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Outgoing action from Greeting or Away screen when jumping into its `/hello` or `/away` shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationAction {
    /// Open the Shortcut Messages editor for the given shortcut ID.
    EditShortcut(BusinessShortcutId),
    /// Show a transient toast message.
    Toast(String),
}

// -----------------------------------------------------------------------------
// Greeting Messages (`settings_greeting.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Greeting Messages (`settings_greeting.cpp`).
pub struct GreetingView {
    /// Draft greeting settings.
    pub settings: GreetingSettings,
    /// Toggle state (`no_activity_days > 0`).
    pub enabled: bool,
    /// Inactivity period in days (`7`, `14`, `21`, `28`).
    pub no_activity_days: i32,
    /// Recipients picker expansion state.
    pub picker_ui: RecipientsPickerUiState,
}

impl GreetingView {
    /// Creates a [`GreetingView`] from saved `settings`.
    #[must_use]
    pub fn new(settings: GreetingSettings) -> Self {
        let enabled = settings.is_enabled();
        let no_activity_days = if enabled {
            settings.no_activity_days
        } else {
            DEFAULT_NO_ACTIVITY_DAYS
        };
        let mut s = settings;
        s.recipients = s.recipients.make_valid();
        Self {
            settings: s,
            enabled,
            no_activity_days,
            picker_ui: RecipientsPickerUiState::default(),
        }
    }

    /// Renders the Greeting Message screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        hello_msg_count: usize,
        available_users: &[BusinessUserRef],
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<AutomationAction> {
        let mut action = None;

        business_section_intro(
            ui,
            "HI",
            BUSINESS_BLUE,
            "Greeting Messages",
            "Automatically send a welcome message to customers who message you for the first time or after a period of inactivity.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if settings_toggle_row(
                    ui,
                    "Send Greeting Message",
                    Some("Automatically greet new private chats."),
                    &mut self.enabled,
                    palette,
                ) && let Some(msg) = self.save_to_backend(backend)
                {
                    action = Some(AutomationAction::Toast(msg));
                }
            });

        if !self.enabled {
            return action;
        }

        section_divider(ui, palette);

        // Create / Edit Greeting Message (`/hello` shortcut)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                let btn_title = if hello_msg_count > 0 {
                    "Edit Greeting Message (/hello)"
                } else {
                    "Create Greeting Message (/hello)"
                };
                let count_label = if hello_msg_count > 0 {
                    format!("{hello_msg_count} message(s)")
                } else {
                    "Empty".to_owned()
                };
                let w = ui.available_width().max(200.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 42.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    btn_title,
                    FontId::proportional(13.5),
                    palette.window_active_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 18.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    count_label,
                    FontId::proportional(12.0),
                    palette.window_sub_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    ">",
                    FontId::proportional(13.0),
                    palette.window_sub_text_fg,
                );
                if resp.clicked() {
                    match backend.emplace_shortcut("hello") {
                        Ok(sc) => {
                            self.settings.shortcut_id = sc.id;
                            action = Some(AutomationAction::EditShortcut(sc.id));
                        }
                        Err(e) => action = Some(AutomationAction::Toast(e)),
                    }
                }
            });

        section_divider(ui, palette);

        // Recipients selector (`BusinessRecipientsType::Messages`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                if render_recipients_selector(
                    ui,
                    "Recipients",
                    &mut self.settings.recipients,
                    BusinessRecipientsType::Messages,
                    available_users,
                    &mut self.picker_ui,
                    palette,
                ) {
                    let _ = self.save_to_backend(backend);
                }
            });

        section_divider(ui, palette);

        // Inactivity period selector (`EditPeriodBox`: 7 / 14 / 21 / 28 days)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                subsection_title(ui, "Period of Inactivity", palette);
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                    for days in GREETING_PERIOD_CHOICES {
                        let active = self.no_activity_days == days;
                        if pill_button(
                            ui,
                            &format!("{days} days"),
                            if active {
                                palette.window_bg_active
                            } else {
                                palette.window_bg_over
                            },
                            if active {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            Some(if active {
                                palette.window_bg_active
                            } else {
                                palette.divider
                            }),
                        )
                        .clicked()
                        {
                            self.no_activity_days = days;
                            if let Some(msg) = self.save_to_backend(backend) {
                                action = Some(AutomationAction::Toast(msg));
                            }
                        }
                    }
                });
                ui.add_space(8.0);
            });

        divider_caption(
            ui,
            "Your greeting message will be sent again to the same chat if there has been no activity for the selected number of days.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if primary_button(
                    ui,
                    "Save Greeting Settings",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                    && let Some(msg) = self.save_to_backend(backend)
                {
                    action = Some(AutomationAction::Toast(msg));
                }
            });

        action
    }

    fn save_to_backend(&mut self, backend: &mut dyn BusinessBackend) -> Option<String> {
        let payload = if self.enabled {
            GreetingSettings {
                recipients: self.settings.recipients.clone(),
                no_activity_days: self.no_activity_days,
                shortcut_id: self.settings.shortcut_id,
            }
        } else {
            GreetingSettings::default()
        };
        match backend.save_greeting_settings(payload) {
            Ok(()) => Some(if self.enabled {
                format!(
                    "Greeting saved ({} days inactivity).",
                    self.no_activity_days
                )
            } else {
                "Greeting message disabled.".to_owned()
            }),
            Err(e) => Some(e),
        }
    }
}

// -----------------------------------------------------------------------------
// Away Messages (`settings_away_message.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Away Messages (`settings_away_message.cpp`).
pub struct AwayMessageView {
    /// Draft away settings.
    pub settings: AwaySettings,
    /// Toggle state (`schedule_type != Never`).
    pub enabled: bool,
    /// Active schedule while enabled (`Always`, `OutsideWorkingHours`, `Custom`).
    pub schedule: AwaySchedule,
    /// Recipients picker expansion state.
    pub picker_ui: RecipientsPickerUiState,
}

impl AwayMessageView {
    /// Creates an [`AwayMessageView`] from saved `settings`.
    #[must_use]
    pub fn new(settings: AwaySettings) -> Self {
        let enabled = settings.is_enabled();
        let mut schedule = if enabled {
            settings.schedule
        } else {
            AwaySchedule {
                schedule_type: AwayScheduleType::Always,
                custom_interval: WorkingInterval::new(1_795_300_000, 1_795_386_400),
            }
        };
        if AwaySchedule::is_bad_custom_interval(schedule.custom_interval) {
            schedule.custom_interval = WorkingInterval::new(1_795_300_000, 1_795_386_400);
        }
        let mut s = settings;
        s.recipients = s.recipients.make_valid();
        Self {
            settings: s,
            enabled,
            schedule,
            picker_ui: RecipientsPickerUiState::default(),
        }
    }

    /// Renders the Away Message screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        away_msg_count: usize,
        available_users: &[BusinessUserRef],
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<AutomationAction> {
        let mut action = None;

        business_section_intro(
            ui,
            "ZZ",
            PREMIUM_INDIGO,
            "Away Messages",
            "Automatically reply to customers when you are away, outside opening hours, or offline.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if settings_toggle_row(
                    ui,
                    "Send Away Message",
                    Some("Automatically reply during your away schedule."),
                    &mut self.enabled,
                    palette,
                ) && let Some(msg) = self.save_to_backend(backend)
                {
                    action = Some(AutomationAction::Toast(msg));
                }
            });

        if !self.enabled {
            return action;
        }

        section_divider(ui, palette);

        // Edit `/away` shortcut
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                let btn_title = if away_msg_count > 0 {
                    "Edit Away Message (/away)"
                } else {
                    "Create Away Message (/away)"
                };
                let count_label = if away_msg_count > 0 {
                    format!("{away_msg_count} message(s)")
                } else {
                    "Empty".to_owned()
                };
                let w = ui.available_width().max(200.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 42.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    btn_title,
                    FontId::proportional(13.5),
                    palette.window_active_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 18.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    count_label,
                    FontId::proportional(12.0),
                    palette.window_sub_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    ">",
                    FontId::proportional(13.0),
                    palette.window_sub_text_fg,
                );
                if resp.clicked() {
                    match backend.emplace_shortcut("away") {
                        Ok(sc) => {
                            self.settings.shortcut_id = sc.id;
                            action = Some(AutomationAction::EditShortcut(sc.id));
                        }
                        Err(e) => action = Some(AutomationAction::Toast(e)),
                    }
                }
            });

        section_divider(ui, palette);

        // Schedule selector (`AddAwayScheduleSelector`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                subsection_title(ui, "Schedule", palette);
                ui.add_space(4.0);
                if radio_row(
                    ui,
                    "Always Send",
                    self.schedule.schedule_type == AwayScheduleType::Always,
                    palette,
                ) {
                    self.schedule.schedule_type = AwayScheduleType::Always;
                }
                if radio_row(
                    ui,
                    "Outside of Business Hours",
                    self.schedule.schedule_type == AwayScheduleType::OutsideWorkingHours,
                    palette,
                ) {
                    self.schedule.schedule_type = AwayScheduleType::OutsideWorkingHours;
                }
                if radio_row(
                    ui,
                    "Custom Schedule",
                    self.schedule.schedule_type == AwayScheduleType::Custom,
                    palette,
                ) {
                    self.schedule.schedule_type = AwayScheduleType::Custom;
                }

                if self.schedule.schedule_type == AwayScheduleType::Custom {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Start:")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                ui.label(
                                    egui::RichText::new(format_unix_datetime(
                                        self.schedule.custom_interval.start,
                                    ))
                                    .size(12.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("+1d").clicked() {
                                            let next = self.schedule.custom_interval.start + 86_400;
                                            if next < self.schedule.custom_interval.end {
                                                self.schedule.custom_interval.start = next;
                                            }
                                        }
                                        if ui.small_button("-1d").clicked() {
                                            let prev = (self.schedule.custom_interval.start
                                                - 86_400)
                                                .max(AWAY_START_TIME_MIN);
                                            self.schedule.custom_interval.start = prev;
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("End:  ")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                ui.label(
                                    egui::RichText::new(format_unix_datetime(
                                        self.schedule.custom_interval.end,
                                    ))
                                    .size(12.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("+1d ").clicked() {
                                            self.schedule.custom_interval.end += 86_400;
                                        }
                                        if ui.small_button("-1d ").clicked() {
                                            let prev = self.schedule.custom_interval.end - 86_400;
                                            if prev > self.schedule.custom_interval.start {
                                                self.schedule.custom_interval.end = prev;
                                            }
                                        }
                                    },
                                );
                            });
                        });
                }

                ui.add_space(8.0);
                thin_divider(ui, palette);
                ui.add_space(4.0);
                settings_toggle_row(
                    ui,
                    "Only When Offline",
                    Some("Skip sending the away message while you are active on Telegram."),
                    &mut self.settings.offline_only,
                    palette,
                );
            });

        section_divider(ui, palette);

        // Recipients selector
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                render_recipients_selector(
                    ui,
                    "Recipients",
                    &mut self.settings.recipients,
                    BusinessRecipientsType::Messages,
                    available_users,
                    &mut self.picker_ui,
                    palette,
                );

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "Save Away Settings",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                    && let Some(msg) = self.save_to_backend(backend)
                {
                    action = Some(AutomationAction::Toast(msg));
                }
            });

        action
    }

    fn save_to_backend(&mut self, backend: &mut dyn BusinessBackend) -> Option<String> {
        let payload = if self.enabled {
            AwaySettings {
                recipients: self.settings.recipients.clone(),
                schedule: self.schedule,
                shortcut_id: self.settings.shortcut_id,
                offline_only: self.settings.offline_only,
            }
        } else {
            AwaySettings::default()
        };
        match backend.save_away_settings(payload) {
            Ok(()) => Some(if self.enabled {
                "Away message settings saved.".to_owned()
            } else {
                "Away message disabled.".to_owned()
            }),
            Err(e) => Some(e),
        }
    }
}

// -----------------------------------------------------------------------------
// Chatbots (`settings_chatbots.cpp` + `data_business_chatbots.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Connected Chatbots (`settings_chatbots.cpp`).
pub struct ChatbotsView {
    /// Current chatbot configuration.
    pub settings: ChatbotsSettings,
    /// Username/link search box value (`@bot` or `https://t.me/bot`).
    pub username_query: String,
    /// Latest bot search result.
    pub search_result: BotSearchResult,
    /// Recipients picker expansion state (`BusinessRecipientsType::Bots`).
    pub picker_ui: RecipientsPickerUiState,
}

impl ChatbotsView {
    /// Creates a [`ChatbotsView`] from saved `settings`.
    #[must_use]
    pub fn new(mut settings: ChatbotsSettings) -> Self {
        settings.recipients = settings.recipients.make_valid();
        settings
            .permissions
            .set(ChatbotsPermissions::VIEW_MESSAGES, true);
        Self {
            settings,
            username_query: String::new(),
            search_result: BotSearchResult::default(),
            picker_ui: RecipientsPickerUiState::default(),
        }
    }

    /// Renders the Chatbots management screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        available_users: &[BusinessUserRef],
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        business_section_intro(
            ui,
            "BOT",
            Color32::from_rgb(0x0E, 0xA5, 0xE9),
            "Chatbots",
            "Connect third-party Telegram bots to read and process messages in selected chats on your behalf.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if let Some(bot) = self.settings.bot.clone() {
                    subsection_title(ui, "Connected Bot", palette);
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (av, _) =
                                    ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                                paint_avatar(ui.painter(), av.center(), 20.0, bot.avatar_rgb, "BT");
                                ui.add_space(6.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(&bot.name)
                                            .size(14.0)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!("@{}", bot.username))
                                            .size(12.0)
                                            .color(palette.window_active_text_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(&bot.about)
                                            .size(11.0)
                                            .color(palette.window_sub_text_fg),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Disconnect",
                                            with_alpha(palette.attention_fg, 0.14),
                                            palette.attention_fg,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            self.settings.bot = None;
                                            self.settings.permissions =
                                                ChatbotsPermissions::default();
                                            self.username_query.clear();
                                            self.search_result = BotSearchResult::default();
                                            if backend.save_chatbots(self.settings.clone()).is_ok()
                                            {
                                                toast = Some(format!(
                                                    "Disconnected @{}",
                                                    bot.username
                                                ));
                                            }
                                        }
                                    },
                                );
                            });
                        });
                } else {
                    subsection_title(ui, "Connect a Business Bot", palette);
                    ui.add_space(6.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.username_query)
                            .hint_text("Enter bot @username or https://t.me/bot")
                            .desired_width(ui.available_width()),
                    );
                    if resp.changed() {
                        self.search_result = backend.search_business_bots(&self.username_query);
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Try:")
                                .size(11.0)
                                .color(palette.window_sub_text_fg),
                        );
                        for sample in ["@DeskAssistBot", "@BookingFlowBot", "@BasicPingBot"] {
                            if pill_button(
                                ui,
                                sample,
                                palette.window_bg_over,
                                palette.window_active_text_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                self.username_query = sample.to_owned();
                                self.search_result = backend.search_business_bots(sample);
                            }
                        }
                    });
                    ui.add_space(8.0);

                    match self.search_result.state {
                        BotLookupState::Empty => {}
                        BotLookupState::Unsupported => {
                            ui.label(
                                egui::RichText::new(
                                    "This bot does not support Telegram Business mode.",
                                )
                                .size(12.0)
                                .color(palette.attention_fg),
                            );
                        }
                        BotLookupState::Ready if self.search_result.bots.is_empty() => {
                            ui.label(
                                egui::RichText::new("No business bots found matching that query.")
                                    .size(12.0)
                                    .color(palette.window_sub_text_fg),
                            );
                        }
                        BotLookupState::Ready => {
                            let candidates = self.search_result.bots.clone();
                            for cand in candidates {
                                Frame::NONE
                                    .fill(palette.window_bg_over)
                                    .stroke(Stroke::new(1.0, palette.divider))
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            let (av, _) = ui.allocate_exact_size(
                                                Vec2::splat(34.0),
                                                Sense::hover(),
                                            );
                                            paint_avatar(
                                                ui.painter(),
                                                av.center(),
                                                17.0,
                                                cand.avatar_rgb,
                                                "BT",
                                            );
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&cand.name)
                                                        .size(13.0)
                                                        .strong()
                                                        .color(palette.window_bold_fg),
                                                );
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "@{}",
                                                        cand.username
                                                    ))
                                                    .size(11.5)
                                                    .color(palette.window_sub_text_fg),
                                                );
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if pill_button(
                                                        ui,
                                                        "Add",
                                                        palette.window_bg_active,
                                                        Color32::WHITE,
                                                        None,
                                                    )
                                                    .clicked()
                                                    {
                                                        let name = cand.name.clone();
                                                        self.settings.bot = Some(cand);
                                                        if backend
                                                            .save_chatbots(self.settings.clone())
                                                            .is_ok()
                                                        {
                                                            toast = Some(format!(
                                                                "{name} connected as your business bot."
                                                            ));
                                                        }
                                                    }
                                                },
                                            );
                                        });
                                    });
                                ui.add_space(4.0);
                            }
                        }
                    }
                }
            });

        section_divider(ui, palette);

        // Bot chat access (`BusinessRecipientsType::Bots`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                render_recipients_selector(
                    ui,
                    "Access to Chats",
                    &mut self.settings.recipients,
                    BusinessRecipientsType::Bots,
                    available_users,
                    &mut self.picker_ui,
                    palette,
                );
            });

        section_divider(ui, palette);

        // Grouped bot permissions (`ChatbotsPermissionsLabels` in `data_business_chatbots.cpp`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!(
                        "Bot Permissions ({} enabled)",
                        self.settings.permissions.enabled_count()
                    ),
                    palette,
                );
                ui.add_space(6.0);

                for group in chatbots_permission_groups() {
                    if let Some(title) = group.title {
                        ui.label(
                            egui::RichText::new(title)
                                .size(12.0)
                                .strong()
                                .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(2.0);
                    }
                    for &(flag, label, locked) in group.items {
                        let mut enabled = self.settings.permissions.contains(flag);
                        ui.add_enabled_ui(!locked, |ui| {
                            if settings_toggle_row(
                                ui,
                                label,
                                if locked {
                                    Some("Required for connected business bots.")
                                } else {
                                    None
                                },
                                &mut enabled,
                                palette,
                            ) {
                                self.settings.permissions.set(flag, enabled);
                            }
                        });
                    }
                    ui.add_space(6.0);
                    thin_divider(ui, palette);
                    ui.add_space(6.0);
                }

                if primary_button(
                    ui,
                    "Save Chatbot Settings",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    match backend.save_chatbots(self.settings.clone()) {
                        Ok(()) => toast = Some("Chatbot permissions and access saved.".to_owned()),
                        Err(e) => toast = Some(e),
                    }
                }
            });

        toast
    }
}

// -----------------------------------------------------------------------------
// Business Chat Links (`settings_chat_links.cpp`)
// -----------------------------------------------------------------------------

/// Draft state for Create / Edit Chat Link box (`EditChatLinkBox`).
#[derive(Debug, Clone)]
pub struct ChatLinkDraft {
    /// Existing URL when editing (`None` when creating a new link).
    pub editing_url: Option<String>,
    /// Optional private link name.
    pub title: String,
    /// Prefilled text message sent by customers who tap the link.
    pub message: String,
}

/// Interactive view-model for Business Chat Links (`settings_chat_links.cpp`).
pub struct ChatLinksView {
    /// Current business chat links.
    pub links: Vec<BusinessChatLink>,
    /// Active create/edit modal draft, if open.
    pub draft: Option<ChatLinkDraft>,
    /// Active QR code preview link, if open.
    pub qr_preview_link: Option<String>,
    /// Account username for footer direct-link note.
    pub account_username: String,
}

impl ChatLinksView {
    /// Creates a [`ChatLinksView`] from saved `links`.
    #[must_use]
    pub fn new(links: Vec<BusinessChatLink>, account_username: String) -> Self {
        Self {
            links,
            draft: None,
            qr_preview_link: None,
            account_username,
        }
    }

    /// Renders the Business Chat Links screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        business_section_intro(
            ui,
            "URL",
            BUSINESS_TEAL,
            "Links to Chat",
            "Create special links to your chat with prefilled text and track how many customers open each link.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(
                        ui,
                        &format!("Business Links ({}/{CHAT_LINKS_LIMIT})", self.links.len()),
                        palette,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.links.len() < CHAT_LINKS_LIMIT
                            && pill_button(
                                ui,
                                "+ Create Link",
                                palette.window_bg_active,
                                palette.window_fg_active,
                                None,
                            )
                            .clicked()
                        {
                            self.draft = Some(ChatLinkDraft {
                                editing_url: None,
                                title: String::new(),
                                message: String::new(),
                            });
                            self.qr_preview_link = None;
                        }
                    });
                });

                // Create / Edit dialog card (`EditChatLinkBox`)
                if let Some(mut draft) = self.draft.clone() {
                    let mut keep_open = true;
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.window_bg_active))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            let heading = if draft.editing_url.is_some() {
                                "Edit Chat Link"
                            } else {
                                "New Chat Link"
                            };
                            ui.label(
                                egui::RichText::new(heading)
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new("Prefilled Message")
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add(
                                egui::TextEdit::multiline(&mut draft.message)
                                    .desired_rows(2)
                                    .hint_text(
                                        "Message text prefilled when a customer opens this link",
                                    )
                                    .desired_width(ui.available_width()),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new("Link Name (optional, visible only to you)")
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.title)
                                    .hint_text("e.g. Instagram Bio, Packaging QR")
                                    .desired_width(ui.available_width()),
                            );
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if pill_button(
                                    ui,
                                    "Save Link",
                                    palette.window_bg_active,
                                    palette.window_fg_active,
                                    None,
                                )
                                .clicked()
                                {
                                    let res = match &draft.editing_url {
                                        Some(url) => backend.edit_chat_link(
                                            url,
                                            &draft.title,
                                            &draft.message,
                                        ),
                                        None => {
                                            backend.create_chat_link(&draft.title, &draft.message)
                                        }
                                    };
                                    match res {
                                        Ok(saved) => {
                                            if let Some(existing) =
                                                self.links.iter_mut().find(|l| l.link == saved.link)
                                            {
                                                *existing = saved;
                                            } else {
                                                self.links.insert(0, saved);
                                            }
                                            keep_open = false;
                                            toast = Some("Chat link saved.".to_owned());
                                        }
                                        Err(e) => toast = Some(e),
                                    }
                                }
                                if pill_button(
                                    ui,
                                    "Cancel",
                                    palette.window_bg,
                                    palette.window_sub_text_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    keep_open = false;
                                }
                            });
                        });
                    self.draft = if keep_open { Some(draft) } else { None };
                }

                // QR code preview card (`InviteLinkQrBox`)
                if let Some(ref qr_url) = self.qr_preview_link.clone() {
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("QR Code Preview")
                                        .size(13.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.add_space(6.0);
                                let (qr_rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(132.0), Sense::hover());
                                paint_qr_preview(ui.painter(), qr_rect, qr_url);
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(qr_url)
                                        .size(11.0)
                                        .color(palette.window_active_text_fg),
                                );
                                ui.add_space(6.0);
                                if pill_button(
                                    ui,
                                    "Close QR",
                                    palette.window_bg,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    self.qr_preview_link = None;
                                }
                            });
                        });
                }

                ui.add_space(8.0);

                // Links list
                let mut to_delete = None;
                let snapshot_links = self.links.clone();
                for item in &snapshot_links {
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon, _) =
                                    ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                                ui.painter().circle_filled(
                                    icon.center(),
                                    17.0,
                                    with_alpha(BUSINESS_TEAL, 0.16),
                                );
                                ui.painter().text(
                                    icon.center(),
                                    Align2::CENTER_CENTER,
                                    "URL",
                                    FontId::proportional(10.0),
                                    BUSINESS_TEAL,
                                );
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(item.display_name())
                                            .size(13.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    if !item.message.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("\"{}\"", item.message))
                                                .size(11.5)
                                                .color(palette.window_sub_text_fg),
                                        );
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(item.clicks_label())
                                                .size(11.5)
                                                .strong()
                                                .color(if item.clicks > 0 {
                                                    SUCCESS_GREEN
                                                } else {
                                                    palette.window_sub_text_fg
                                                }),
                                        );
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                                if pill_button(
                                    ui,
                                    "Copy",
                                    with_alpha(palette.window_bg_active, 0.14),
                                    palette.window_active_text_fg,
                                    None,
                                )
                                .clicked()
                                {
                                    ui.ctx().copy_text(item.link.clone());
                                    toast = Some(format!("Copied {}", item.link));
                                }
                                if pill_button(
                                    ui,
                                    "QR",
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    self.qr_preview_link = Some(item.link.clone());
                                }
                                if pill_button(
                                    ui,
                                    "Edit",
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    self.draft = Some(ChatLinkDraft {
                                        editing_url: Some(item.link.clone()),
                                        title: item.title.clone(),
                                        message: item.message.clone(),
                                    });
                                }
                                if pill_button(
                                    ui,
                                    "Delete",
                                    with_alpha(palette.attention_fg, 0.14),
                                    palette.attention_fg,
                                    None,
                                )
                                .clicked()
                                {
                                    to_delete = Some(item.link.clone());
                                }
                            });
                        });
                    ui.add_space(6.0);
                }

                if let Some(url) = to_delete
                    && backend.delete_chat_link(&url).is_ok()
                {
                    self.links.retain(|l| l.link != url);
                    toast = Some("Chat link deleted.".to_owned());
                }
            });

        divider_caption(
            ui,
            &format!(
                "You can also share your standard direct chat link https://t.me/{} anytime.",
                self.account_username
            ),
            palette,
        );

        toast
    }
}

fn paint_qr_preview(painter: &egui::Painter, rect: Rect, url: &str) {
    painter.rect_filled(rect, CornerRadius::same(8), Color32::WHITE);
    if let Ok(code) = QrCode::new(url.as_bytes()) {
        let dim = code.width();
        let pad = 10.0;
        let cell = (rect.width() - pad * 2.0) / dim as f32;
        for y in 0..dim {
            for x in 0..dim {
                if code[(x, y)] == QrColor::Dark {
                    let r = Rect::from_min_size(
                        Pos2::new(
                            rect.left() + pad + x as f32 * cell,
                            rect.top() + pad + y as f32 * cell,
                        ),
                        Vec2::splat(cell + 0.35),
                    );
                    painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgb(0x11, 0x18, 0x27));
                }
            }
        }
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, Color32::from_rgb(0xD1, 0xD5, 0xDB)),
        StrokeKind::Inside,
    );
}
