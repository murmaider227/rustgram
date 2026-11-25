//! Consent boxes shown before a link may launch a Mini App or start a bot.
//!
//! Port origin:
//! - `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp:1148-1236`
//!   (`WebViewInstance::confirmOpen` / `confirmAppOpen`, `FillBotUsepic`)
//! - `Telegram/SourceFiles/history/history_widget.cpp` (`_botStart` / `sendBotStartCommand`):
//!   a `tg://resolve?domain=<bot>&start=<token>` link keeps the token and sends `/start <token>`
//!   only after the user presses START.

use egui::{Color32, Pos2, Sense, Ui, Vec2};
use tg_app_core::stand_in::types::PeerId;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

/// Placeholder used to split a translated phrase around its `{terms}` link.
const LINK_MARKER: &str = "\u{1}";

/// Body click result for [`MiniAppOpenConfirmBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiniAppConfirmBodyAction {
    /// The user clicked the "Terms of Service for Mini Apps" link.
    OpenTerms(String),
}

/// First-open consent box for a Mini App launched from a link (`WebViewInstance::confirmOpen`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiniAppOpenConfirmBoxState {
    /// Bot that owns the Mini App.
    pub bot_id: PeerId,
    /// Bot display name shown next to its userpic.
    pub bot_name: String,
    /// Optional explicit launch URL.
    pub url: Option<String>,
    /// Optional `startapp` / `startattach` parameter.
    pub start_param: Option<String>,
}

impl MiniAppOpenConfirmBoxState {
    /// Creates the consent box for `bot_id`.
    #[must_use]
    pub fn new(
        bot_id: PeerId,
        bot_name: impl Into<String>,
        url: Option<String>,
        start_param: Option<String>,
    ) -> Self {
        Self {
            bot_id,
            bot_name: bot_name.into(),
            url,
            start_param,
        }
    }

    /// Builds the box descriptor: Cancel / Launch (`lng_view_button_bot_app`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "")
            .width(st::box_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "launch",
                tr::lng_view_button_bot_app(),
            ))
    }

    /// Renders the bot userpic + name and `lng_profile_open_app_about` with its terms link.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
    ) -> Option<MiniAppConfirmBodyAction> {
        paint_bot_header(ui, colors, &self.bot_name);
        ui.add_space(12.0);
        let phrase = tr::lng_profile_open_app_about(LINK_MARKER);
        let (before, after) = phrase
            .split_once(LINK_MARKER)
            .unwrap_or((phrase.as_str(), ""));
        let text_fg = colors.color("windowFg");
        let link_fg = colors.color("windowActiveTextFg");
        let font = egui::FontId::new(14.0, family_regular());
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(
                egui::RichText::new(before)
                    .font(font.clone())
                    .color(text_fg),
            );
            let link = ui.add(
                egui::Label::new(
                    egui::RichText::new(tr::lng_profile_open_app_terms())
                        .font(font.clone())
                        .color(link_fg),
                )
                .sense(Sense::click()),
            );
            if link.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if link.clicked() {
                action = Some(MiniAppConfirmBodyAction::OpenTerms(
                    tr::lng_mini_apps_tos_url(),
                ));
            }
            ui.label(egui::RichText::new(after).font(font).color(text_fg));
        });
        action
    }
}

/// START box for a `?start=<token>` bot link: nothing is sent until the user presses START.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotStartConfirmBoxState {
    /// Bot to start.
    pub bot_id: PeerId,
    /// Bot display name.
    pub bot_name: String,
    /// Deep-link start token (sent as `/start <token>` via `messages.startBot`).
    pub start_param: String,
}

impl BotStartConfirmBoxState {
    /// Creates the START box for `bot_id` with the link's `start_param`.
    #[must_use]
    pub fn new(
        bot_id: PeerId,
        bot_name: impl Into<String>,
        start_param: impl Into<String>,
    ) -> Self {
        Self {
            bot_id,
            bot_name: bot_name.into(),
            start_param: start_param.into(),
        }
    }

    /// Builds the box descriptor: Cancel / START (`lng_bot_start`, upper-cased like tdesktop's
    /// `historyComposeButton`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "")
            .width(st::box_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "start",
                tr::lng_bot_start().to_uppercase(),
            ))
    }

    /// Renders the bot userpic + name.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        paint_bot_header(ui, colors, &self.bot_name);
    }
}

/// `FillBotUsepic`: a round userpic with initials and the bot name in semibold.
fn paint_bot_header(ui: &mut Ui, colors: &dyn Colors, name: &str) {
    let size = 56.0;
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), size + 28.0), Sense::hover());
    let center = Pos2::new(rect.center().x, rect.top() + size / 2.0);
    ui.painter()
        .circle_filled(center, size / 2.0, colors.color("windowBgActive"));
    let initials: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        initials,
        egui::FontId::new(20.0, family_semibold()),
        Color32::WHITE,
    );
    ui.painter().text(
        Pos2::new(rect.center().x, rect.top() + size + 16.0),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::new(15.0, family_semibold()),
        colors.color("windowBoldFg"),
    );
}
