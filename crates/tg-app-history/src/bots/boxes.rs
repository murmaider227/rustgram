//! Modal confirmation and interactive bot flow boxes:
//! - Callback alert box & transient toast banner
//! - External URL confirmation box (`HiddenUrlClickHandler`)
//! - URL Auth (Login) confirmation box with "allow to send messages" checkbox (`boxes/url_auth_box.cpp`)
//! - Request Phone, Request Location, Request Poll, and Request Peer boxes (`api/api_bot.cpp`)
//! - 2FA Callback password confirmation box (`ButtonKind::CallbackWithPassword`)
//! - Attach Menu Bot disclaimer box (`bot_attach_web_view.cpp`)
//! - Share Game Score box (`window/window_peer_menu.cpp` `ShowShareGameBox`)
//!
//! Port origin: `Telegram/SourceFiles/boxes/url_auth_box.cpp`,
//! `Telegram/SourceFiles/boxes/url_auth_box_content.cpp`,
//! `Telegram/SourceFiles/api/api_bot.cpp`, and
//! `Telegram/SourceFiles/window/window_peer_menu.cpp`.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, ScrollArea, Sense, Stroke,
    StrokeKind, TextEdit, Ui, Vec2,
};
use tg_app_core::core_types::platform_call::PlatformCall;
use tg_bots::callback::{CallbackQueryRequest, UrlAuthConfirmationBox, UrlAuthUserChoice};
use tg_bots::markup::{PollRequestRestriction, RequestPeerQuery, RequestPeerType};
use tg_bots::mini_app::{
    AttachMenuBot, GameData, GameHighScore, build_game_share_link, can_share_game_to_peer,
};
use tg_bots::stand_in::{FullMsgId, PeerId, UserId};
use tg_platform::geolocation::{GeoLocation, GeoLocationProvider, GeolocationStatus};

use crate::bots::backend::{BotPeerCandidate, SubmittedPoll};
use crate::bots::ui_helpers::{box_action_button, paint_procedural_art, with_alpha};
use tg_app_core::stand_in::theme::Palette;

// ---------------------------------------------------------------------------
// Active Modal Box State
// ---------------------------------------------------------------------------

/// Active modal dialog box owned by the bot interaction layer.
#[derive(Debug, Clone)]
pub enum BotModalBox {
    /// Callback query modal alert (`CallbackAnswerAction::ShowAlert`).
    CallbackAlert {
        /// Bot name shown in header.
        bot_name: String,
        /// Alert message body.
        message: String,
    },
    /// External URL open confirmation (`BotButtonAction::OpenUrl { require_confirmation: true }`).
    UrlConfirm {
        /// Target URL to inspect before opening.
        url: String,
        /// Optional bot display name.
        bot_name: String,
    },
    /// URL Auth / Login confirmation box (`UrlAuthConfirmationBox`).
    UrlAuth {
        /// Prepared server auth confirmation data.
        data: Box<UrlAuthConfirmationBox>,
        /// Current logged-in user's name.
        account_name: String,
        /// Checkbox: "Allow <Bot> to send you messages" (`write_allowed`).
        allow_write: bool,
        /// Checkbox: "Share your phone number" (`share_phone_number`, when requested).
        share_phone: bool,
        /// Selected verification emoji match code, if any.
        selected_match_code: Option<String>,
    },
    /// 2FA cloud password prompt for `ButtonKind::CallbackWithPassword`.
    CallbackPassword {
        /// Original callback request awaiting 2FA verification.
        request: CallbackQueryRequest,
        /// Button label.
        button_text: String,
        /// Entered password text.
        password_input: String,
        /// Optional validation error message.
        error: Option<String>,
    },
    /// Confirm sharing user's phone number contact (`ButtonKind::RequestPhone`).
    RequestPhone {
        /// Owning message id.
        item_id: FullMsgId,
        /// Bot name requesting the phone number.
        bot_name: String,
        /// Current user's phone number preview.
        phone_number: String,
    },
    /// Confirm sharing user's device location (`ButtonKind::RequestLocation`) via
    /// `tg-platform::geolocation`.
    RequestLocation {
        /// Bot name requesting location.
        bot_name: String,
        /// Resolved device coordinates if already fetched.
        resolved_location: Option<GeoLocation>,
        /// Optional status or error message (e.g. OS location services disabled).
        status_message: Option<String>,
    },
    /// Create Poll box (`ButtonKind::RequestPoll`).
    RequestPoll {
        /// Poll restriction (`Any`, `RegularOnly`, `QuizOnly`).
        restriction: PollRequestRestriction,
        /// Question text input.
        question: String,
        /// Answer options inputs.
        options: Vec<String>,
        /// Whether quiz mode is toggled on.
        is_quiz: bool,
        /// Selected correct option index for quiz mode.
        correct_option: usize,
        /// Validation error message.
        error: Option<String>,
    },
    /// Peer picker box (`ButtonKind::RequestPeer`).
    RequestPeer {
        /// Owning message id.
        item_id: FullMsgId,
        /// Server button id.
        button_id: i64,
        /// Selection filter criteria from `tg-bots`.
        query: RequestPeerQuery,
        /// Currently selected peer ids (up to `query.max_quantity`).
        selected: Vec<PeerId>,
        /// Search filter string.
        search: String,
    },
    /// Share Game Score box (`ShowShareGameBox`).
    ShareGameScore {
        /// Game message id.
        message_id: FullMsgId,
        /// Bot username (for canonical `https://t.me/<bot>?game=<short_name>` share link).
        bot_username: String,
        /// Game metadata.
        game: GameData,
        /// High score leaderboard `(rank, player_name, score)`.
        leaderboard: Vec<(i32, String, i32)>,
        /// Selected recipient peer, if any.
        selected_peer: Option<PeerId>,
        /// Search filter string.
        search: String,
    },
    /// Mini Apps disclaimer confirmation when launching/adding an attach menu bot.
    AttachBotDisclaimer {
        /// Target attach menu bot.
        bot: AttachMenuBot,
        /// Checkbox to grant write access when `bot.request_write_access` is true.
        allow_write: bool,
    },
}

/// Outcome of a worker-thread [`BotModalBox::resolve_location_with`] call:
/// `(resolve_result, modal_status_message)`.
pub type BotLocationResolution = (Result<Option<GeoLocation>, String>, Option<String>);

/// In-flight worker-thread geolocation handle returned by [`BotModalBox::spawn_resolve_location_with`].
pub type PendingBotLocationCall = PlatformCall<BotLocationResolution>;

impl BotModalBox {
    /// Creates a [`BotModalBox::UrlAuth`] with tdesktop's defaults (`allow_write = true` when
    /// requested, matching `url_auth_box_content.cpp`).
    #[must_use]
    pub fn url_auth(data: UrlAuthConfirmationBox, account_name: impl Into<String>) -> Self {
        let allow_write = data.write_access_bot_name.is_some();
        let selected_match_code = data.match_codes.first().cloned();
        Self::UrlAuth {
            data: Box::new(data),
            account_name: account_name.into(),
            allow_write,
            share_phone: false,
            selected_match_code,
        }
    }

    /// Creates a [`BotModalBox::RequestLocation`] confirmation dialog.
    #[must_use]
    pub fn request_location(bot_name: impl Into<String>) -> Self {
        Self::RequestLocation {
            bot_name: bot_name.into(),
            resolved_location: None,
            status_message: None,
        }
    }

    /// Resolves device coordinates for a [`BotModalBox::RequestLocation`] via `provider`.
    pub fn resolve_location_with(
        &mut self,
        provider: &dyn GeoLocationProvider,
    ) -> Result<Option<GeoLocation>, String> {
        let Self::RequestLocation {
            resolved_location,
            status_message,
            ..
        } = self
        else {
            return Ok(None);
        };
        let status = provider.status();
        if matches!(
            status,
            GeolocationStatus::Disabled | GeolocationStatus::NotAvailable
        ) {
            let msg = match status {
                GeolocationStatus::Disabled => {
                    "Windows Location Services are disabled in Settings.".to_owned()
                }
                _ => "Device geolocation is unavailable on this machine.".to_owned(),
            };
            *resolved_location = None;
            *status_message = Some(msg);
            return Ok(None);
        }
        match provider.resolve_current_exact_location() {
            Ok(Some(loc)) if loc.point.is_valid() => {
                *resolved_location = Some(loc);
                *status_message = Some(format!(
                    "Resolved coordinates: {:.5}, {:.5}",
                    loc.point.latitude, loc.point.longitude
                ));
                Ok(Some(loc))
            }
            Ok(_) => {
                *resolved_location = None;
                *status_message = Some("Could not determine current device location.".to_owned());
                Ok(None)
            }
            Err(err) => {
                let msg = format!("Location lookup failed: {err}");
                *resolved_location = None;
                *status_message = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Runs [`Self::resolve_location_with`] on a worker thread so the UI frame loop never blocks
    /// on a WinRT geolocation fix or consent prompt.
    ///
    /// Returns a [`PendingBotLocationCall`] yielding `(resolve_result, modal_status_message)`.
    pub fn spawn_resolve_location_with(
        bot_name: impl Into<String>,
        provider: Arc<dyn GeoLocationProvider>,
        repaint: Option<egui::Context>,
    ) -> PendingBotLocationCall {
        let bot_name = bot_name.into();
        PlatformCall::spawn("geolocation-bot", repaint, move || {
            let mut modal = Self::request_location(bot_name);
            let result = modal.resolve_location_with(provider.as_ref());
            let status = match modal {
                Self::RequestLocation { status_message, .. } => status_message,
                _ => None,
            };
            (result, status)
        })
    }

    /// Creates a [`BotModalBox::RequestPoll`] pre-initialized with two blank options.
    #[must_use]
    pub fn request_poll(restriction: PollRequestRestriction) -> Self {
        let is_quiz = matches!(restriction, PollRequestRestriction::QuizOnly);
        Self::RequestPoll {
            restriction,
            question: String::new(),
            options: vec![String::new(), String::new()],
            is_quiz,
            correct_option: 0,
            error: None,
        }
    }

    /// Creates a [`BotModalBox::ShareGameScore`] box.
    #[must_use]
    pub fn share_game_score(
        message_id: FullMsgId,
        bot_username: impl Into<String>,
        game: GameData,
        high_scores: &[GameHighScore],
        resolve_name: impl Fn(UserId) -> String,
    ) -> Self {
        let leaderboard = high_scores
            .iter()
            .map(|h| (h.pos, resolve_name(h.user_id), h.score))
            .collect();
        Self::ShareGameScore {
            message_id,
            bot_username: bot_username.into(),
            game,
            leaderboard,
            selected_peer: None,
            search: String::new(),
        }
    }
}

/// Outcome emitted when the user submits or cancels a [`BotModalBox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotModalOutcome {
    /// Dialog dismissed/cancelled.
    Closed,
    /// User confirmed opening the external URL.
    OpenUrlConfirmed(String),
    /// User accepted or declined URL authorization.
    UrlAuthAnswered {
        /// Confirmation box data.
        data: Box<UrlAuthConfirmationBox>,
        /// User's choice (auth, allow_write, share_phone, match_code).
        choice: UrlAuthUserChoice,
    },
    /// User submitted a 2FA cloud password for a protected callback button.
    CallbackPasswordSubmitted {
        /// Original callback request.
        request: CallbackQueryRequest,
        /// Password entered by user.
        password: String,
    },
    /// User confirmed sharing their phone number contact.
    SharePhoneConfirmed(FullMsgId),
    /// User confirmed sharing their current Windows geolocation with the bot.
    ShareLocationRequested,
    /// User confirmed sharing already-resolved coordinates (`degrees * 1e7`).
    ShareLocationConfirmed {
        /// Latitude scaled by `1e7`.
        lat_e7: i64,
        /// Longitude scaled by `1e7`.
        lon_e7: i64,
    },
    /// User submitted a new poll.
    PollSubmitted(SubmittedPoll),
    /// User selected peer(s) for `ButtonKind::RequestPeer`.
    PeersSelected {
        /// Owning message id.
        item_id: FullMsgId,
        /// Server button id.
        button_id: i64,
        /// Selected peers.
        peers: Vec<PeerId>,
    },
    /// User chose a target chat to share the game score into.
    GameShared {
        /// Game message id.
        message_id: FullMsgId,
        /// Chosen recipient peer.
        target_peer: PeerId,
    },
    /// User copied the game share link.
    CopyGameLink(String),
    /// User accepted the attach menu bot disclaimer.
    AttachDisclaimerAccepted {
        /// Target bot.
        bot: AttachMenuBot,
        /// Whether write access was granted.
        allow_write: bool,
    },
}

/// Renders the active [`BotModalBox`] centered inside `bounds` with a dimmed backdrop scrim.
///
/// Returns `Some(BotModalOutcome)` once the user clicks an action or dismisses the dialog.
pub fn show_bot_modal_box(
    ui: &mut Ui,
    bounds: Rect,
    modal: &mut BotModalBox,
    candidates: &[BotPeerCandidate],
    palette: &Palette,
) -> Option<BotModalOutcome> {
    let mut outcome = None;

    // Dimmed backdrop
    ui.painter()
        .rect_filled(bounds, CornerRadius::ZERO, palette.layer_backdrop);

    let box_w: f32 = match modal {
        BotModalBox::ShareGameScore { .. } | BotModalBox::RequestPeer { .. } => 410.0,
        BotModalBox::UrlAuth { .. } | BotModalBox::RequestPoll { .. } => 390.0,
        _ => 360.0,
    };
    let box_w = box_w.min(bounds.width() - 32.0);

    let est_h: f32 = match modal {
        BotModalBox::CallbackAlert { .. } => 195.0,
        BotModalBox::UrlConfirm { .. } => 210.0,
        BotModalBox::UrlAuth { data, .. } => {
            let mut h = 250.0;
            if data.write_access_bot_name.is_some() {
                h += 28.0;
            }
            if !data.match_codes.is_empty() {
                h += 38.0;
            }
            h
        }
        BotModalBox::CallbackPassword { .. } => 225.0,
        BotModalBox::RequestPhone { .. } => 210.0,
        BotModalBox::RequestLocation {
            resolved_location,
            status_message,
            ..
        } => {
            let mut h = 178.0;
            if resolved_location.is_some() {
                h += 24.0;
            }
            if status_message.is_some() {
                h += 22.0;
            }
            h
        }
        BotModalBox::RequestPoll { .. } => 340.0,
        BotModalBox::RequestPeer { .. } => 370.0,
        BotModalBox::ShareGameScore { .. } => 430.0,
        BotModalBox::AttachBotDisclaimer { .. } => 245.0,
    };
    let est_h = est_h.min(bounds.height() - 32.0);

    let card_rect = Rect::from_center_size(bounds.center(), Vec2::new(box_w, est_h));
    ui.painter().rect_filled(
        card_rect.expand(1.0),
        CornerRadius::same(13),
        palette.divider,
    );
    ui.painter()
        .rect_filled(card_rect, CornerRadius::same(12), palette.window_bg);

    let inner = card_rect.shrink(18.0);
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        match modal {
            BotModalBox::CallbackAlert { bot_name, message } => {
                ui.label(
                    egui::RichText::new(bot_name.as_str())
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(message.as_str())
                        .size(13.5)
                        .color(palette.window_fg),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if box_action_button(
                        ui,
                        "OK",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        88.0,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                });
            }
            BotModalBox::UrlConfirm { url, bot_name } => {
                ui.label(
                    egui::RichText::new("Open this link?")
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{bot_name} wants to open an external website:"
                    ))
                    .size(13.0)
                    .color(palette.window_sub_text_fg),
                );
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(url.as_str())
                                .size(12.5)
                                .color(palette.window_active_text_fg),
                        );
                    });
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Open",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::OpenUrlConfirmed(url.clone()));
                    }
                });
            }
            BotModalBox::UrlAuth {
                data,
                account_name,
                allow_write,
                share_phone,
                selected_match_code,
            } => {
                ui.label(
                    egui::RichText::new(format!("Log in to {}?", data.display_name))
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "You will log in as {account_name}. Your name, username, and profile photo will be shared with {}.",
                        data.display_name
                    ))
                    .size(12.5)
                    .color(palette.window_fg),
                );

                // Device / browser / IP security details card (`url_auth_box.cpp`)
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} • {}",
                                data.browser, data.platform
                            ))
                            .size(12.0)
                            .strong()
                            .color(palette.window_bold_fg),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "IP: {}  •  Region: {}",
                                data.ip, data.region
                            ))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                        );
                    });

                // Optional emoji match codes selector
                if !data.match_codes.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Match code:")
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        for code in &data.match_codes {
                            let active = selected_match_code.as_deref() == Some(code.as_str());
                            let (r, resp) =
                                ui.allocate_exact_size(Vec2::new(34.0, 26.0), Sense::click());
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            ui.painter().rect_filled(
                                r,
                                CornerRadius::same(6),
                                if active {
                                    with_alpha(palette.window_bg_active, 0.22)
                                } else {
                                    palette.window_bg_over
                                },
                            );
                            if active {
                                ui.painter().rect_stroke(
                                    r,
                                    CornerRadius::same(6),
                                    Stroke::new(1.2, palette.window_bg_active),
                                    StrokeKind::Inside,
                                );
                            }
                            ui.painter().text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                code,
                                FontId::proportional(13.0),
                                palette.window_fg,
                            );
                            if resp.clicked() {
                                *selected_match_code = Some(code.clone());
                            }
                        }
                    });
                }

                // "Allow <Bot> to send you messages" checkbox (`request_write_access`)
                if let Some(ref bot_name) = data.write_access_bot_name {
                    ui.add_space(10.0);
                    ui.checkbox(
                        allow_write,
                        egui::RichText::new(format!(
                            "Allow {bot_name} to send you messages"
                        ))
                        .size(13.0)
                        .color(palette.window_fg),
                    );
                }

                if data.request_phone_number {
                    ui.add_space(4.0);
                    ui.checkbox(
                        share_phone,
                        egui::RichText::new("Share your phone number")
                            .size(13.0)
                            .color(palette.window_fg),
                    );
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        36.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::UrlAuthAnswered {
                            data: data.clone(),
                            choice: UrlAuthUserChoice {
                                auth: false,
                                allow_write: false,
                                share_phone: false,
                                match_code: None,
                            },
                        });
                    }
                    if box_action_button(
                        ui,
                        "Log In",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        36.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::UrlAuthAnswered {
                            data: data.clone(),
                            choice: UrlAuthUserChoice {
                                auth: true,
                                allow_write: *allow_write,
                                share_phone: *share_phone,
                                match_code: selected_match_code.clone(),
                            },
                        });
                    }
                });
            }
            BotModalBox::CallbackPassword {
                request,
                button_text,
                password_input,
                error,
            } => {
                ui.label(
                    egui::RichText::new("Cloud Password Required")
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Confirm \"{button_text}\" by entering your 2FA password (demo: any non-empty text)."
                    ))
                    .size(12.5)
                    .color(palette.window_sub_text_fg),
                );
                ui.add_space(10.0);
                ui.add(
                    TextEdit::singleline(password_input)
                        .password(true)
                        .hint_text("Enter cloud password")
                        .desired_width(ui.available_width()),
                );
                if let Some(err) = error.as_deref() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.5)
                            .color(palette.attention_fg),
                    );
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Confirm",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        if password_input.trim().is_empty() {
                            *error = Some("Please enter your password.".to_owned());
                        } else {
                            outcome = Some(BotModalOutcome::CallbackPasswordSubmitted {
                                request: request.clone(),
                                password: password_input.clone(),
                            });
                        }
                    }
                });
            }
            BotModalBox::RequestPhone {
                item_id,
                bot_name,
                phone_number,
            } => {
                ui.label(
                    egui::RichText::new("Share Phone Number?")
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{bot_name} is requesting your phone number ({phone_number})."
                    ))
                    .size(13.0)
                    .color(palette.window_fg),
                );
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Share Contact",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::SharePhoneConfirmed(*item_id));
                    }
                });
            }
            BotModalBox::RequestLocation {
                bot_name,
                resolved_location,
                status_message,
            } => {
                ui.label(
                    egui::RichText::new("Share Location?")
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{bot_name} is requesting your current location via Windows Location Services."
                    ))
                    .size(13.0)
                    .color(palette.window_fg),
                );
                if let Some(loc) = resolved_location {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Pin: {:.5}, {:.5}",
                            loc.point.latitude, loc.point.longitude
                        ))
                        .monospace()
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                    );
                }
                if let Some(msg) = status_message.as_deref() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(msg)
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Share Location",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(match resolved_location {
                            Some(loc) => BotModalOutcome::ShareLocationConfirmed {
                                lat_e7: (loc.point.latitude * 1e7).round() as i64,
                                lon_e7: (loc.point.longitude * 1e7).round() as i64,
                            },
                            None => BotModalOutcome::ShareLocationRequested,
                        });
                    }
                });
            }
            BotModalBox::RequestPoll {
                restriction,
                question,
                options,
                is_quiz,
                correct_option,
                error,
            } => {
                let title = match restriction {
                    PollRequestRestriction::QuizOnly => "Create Quiz Poll",
                    PollRequestRestriction::RegularOnly => "Create Regular Poll",
                    PollRequestRestriction::Any => "Create Poll",
                };
                ui.label(
                    egui::RichText::new(title)
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);
                ui.add(
                    TextEdit::singleline(question)
                        .hint_text("Ask a question…")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(8.0);

                for (idx, opt) in options.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if *is_quiz {
                            ui.radio_value(correct_option, idx, "");
                        }
                        ui.add(
                            TextEdit::singleline(opt)
                                .hint_text(format!("Option {}", idx + 1))
                                .desired_width(ui.available_width()),
                        );
                    });
                }

                if options.len() < 4 && ui.small_button("+ Add option").clicked() {
                    options.push(String::new());
                }

                if matches!(restriction, PollRequestRestriction::Any) {
                    ui.add_space(6.0);
                    ui.checkbox(is_quiz, "Quiz mode (one correct answer)");
                }

                if let Some(err) = error.as_deref() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.5)
                            .color(palette.attention_fg),
                    );
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Create",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        let non_empty: Vec<String> = options
                            .iter()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if question.trim().is_empty() || non_empty.len() < 2 {
                            *error = Some("Enter a question and at least 2 options.".to_owned());
                        } else {
                            let corr = if *is_quiz {
                                Some((*correct_option).min(non_empty.len() - 1))
                            } else {
                                None
                            };
                            outcome = Some(BotModalOutcome::PollSubmitted(SubmittedPoll {
                                question: question.trim().to_owned(),
                                options: non_empty,
                                is_quiz: *is_quiz,
                                correct_option: corr,
                            }));
                        }
                    }
                });
            }
            BotModalBox::RequestPeer {
                item_id,
                button_id,
                query,
                selected,
                search,
            } => {
                let kind_title = match query.peer_type {
                    RequestPeerType::User => "Select User",
                    RequestPeerType::Group => "Select Group",
                    RequestPeerType::Broadcast => "Select Channel",
                };
                let max_qty = query.max_quantity.max(1) as usize;
                ui.label(
                    egui::RichText::new(format!("{kind_title} (up to {max_qty})"))
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.add(
                    TextEdit::singleline(search)
                        .hint_text("Filter peers…")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(8.0);

                let q_lower = search.trim().to_ascii_lowercase();
                let matching: Vec<&BotPeerCandidate> = candidates
                    .iter()
                    .filter(|c| query.matches_candidate(&c.criteria))
                    .filter(|c| {
                        q_lower.is_empty()
                            || c.title.to_ascii_lowercase().contains(&q_lower)
                            || c.subtitle.to_ascii_lowercase().contains(&q_lower)
                    })
                    .collect();

                ScrollArea::vertical()
                    .id_salt("tg_request_peer_list")
                    .max_height(190.0)
                    .show(ui, |ui| {
                        if matching.is_empty() {
                            ui.label(
                                egui::RichText::new("No chats match the bot's criteria.")
                                    .color(palette.window_sub_text_fg),
                            );
                        }
                        for peer in matching {
                            let is_sel = selected.contains(&peer.peer_id);
                            if paint_peer_picker_row(ui, peer, is_sel, palette).clicked() {
                                if is_sel {
                                    selected.retain(|id| *id != peer.peer_id);
                                } else if selected.len() < max_qty {
                                    selected.push(peer.peer_id);
                                } else if max_qty == 1 {
                                    selected.clear();
                                    selected.push(peer.peer_id);
                                }
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    let share_label = if selected.is_empty() {
                        "Share".to_owned()
                    } else {
                        format!("Share ({})", selected.len())
                    };
                    if box_action_button(
                        ui,
                        &share_label,
                        if selected.is_empty() {
                            with_alpha(palette.window_bg_active, 0.45)
                        } else {
                            palette.window_bg_active
                        },
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                        && !selected.is_empty()
                    {
                        outcome = Some(BotModalOutcome::PeersSelected {
                            item_id: *item_id,
                            button_id: *button_id,
                            peers: selected.clone(),
                        });
                    }
                });
            }
            BotModalBox::ShareGameScore {
                message_id,
                bot_username,
                game,
                leaderboard,
                selected_peer,
                search,
            } => {
                ui.label(
                    egui::RichText::new(format!("Share Game — {}", game.title))
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);

                // Game banner + leaderboard summary
                let w = ui.available_width();
                let (banner_rect, _) = ui.allocate_exact_size(Vec2::new(w, 64.0), Sense::hover());
                paint_procedural_art(
                    ui.painter(),
                    banner_rect,
                    game.id.0,
                    CornerRadius::same(8),
                    Some("GAME"),
                );
                ui.painter().rect_filled(
                    banner_rect,
                    CornerRadius::same(8),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 72),
                );
                ui.painter().text(
                    Pos2::new(banner_rect.left() + 62.0, banner_rect.top() + 24.0),
                    Align2::LEFT_CENTER,
                    &game.title,
                    FontId::proportional(14.5),
                    Color32::WHITE,
                );
                ui.painter().text(
                    Pos2::new(banner_rect.left() + 62.0, banner_rect.top() + 44.0),
                    Align2::LEFT_CENTER,
                    build_game_share_link(bot_username, &game.short_name),
                    FontId::proportional(11.0),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 235),
                );

                // Leaderboard strip
                if !leaderboard.is_empty() {
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(palette.window_bg_over)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("HIGH SCORES")
                                    .size(10.0)
                                    .strong()
                                    .color(palette.window_sub_text_fg),
                            );
                            for (pos, name, score) in leaderboard.iter().take(3) {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("#{pos} {name}"))
                                            .size(12.0)
                                            .color(palette.window_fg),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{score} pts"))
                                                    .size(12.0)
                                                    .strong()
                                                    .color(palette.window_active_text_fg),
                                            );
                                        },
                                    );
                                });
                            }
                        });
                }

                ui.add_space(8.0);
                ui.add(
                    TextEdit::singleline(search)
                        .hint_text("Search chat to share with…")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(6.0);

                let q_lower = search.trim().to_ascii_lowercase();
                let eligible: Vec<&BotPeerCandidate> = candidates
                    .iter()
                    .filter(|c| can_share_game_to_peer(c.is_self, c.can_send_games, c.is_forum))
                    .filter(|c| {
                        q_lower.is_empty() || c.title.to_ascii_lowercase().contains(&q_lower)
                    })
                    .collect();

                ScrollArea::vertical()
                    .id_salt("tg_share_game_peers")
                    .max_height(112.0)
                    .show(ui, |ui| {
                        for peer in eligible {
                            let is_sel = *selected_peer == Some(peer.peer_id);
                            if paint_peer_picker_row(ui, peer, is_sel, palette).clicked() {
                                *selected_peer = Some(peer.peer_id);
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let third = (ui.available_width() - 12.0) / 3.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        third,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Copy Link",
                        with_alpha(palette.window_bg_active, 0.14),
                        palette.window_active_text_fg,
                        third,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::CopyGameLink(build_game_share_link(
                            bot_username,
                            &game.short_name,
                        )));
                    }
                    if box_action_button(
                        ui,
                        "Share",
                        if selected_peer.is_some() {
                            palette.window_bg_active
                        } else {
                            with_alpha(palette.window_bg_active, 0.45)
                        },
                        palette.window_fg_active,
                        third,
                        34.0,
                    )
                    .clicked()
                        && let Some(target_peer) = *selected_peer
                    {
                        outcome = Some(BotModalOutcome::GameShared {
                            message_id: *message_id,
                            target_peer,
                        });
                    }
                });
            }
            BotModalBox::AttachBotDisclaimer { bot, allow_write } => {
                ui.label(
                    egui::RichText::new(format!("Launch {}?", bot.short_name))
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Mini Apps are created by third-party developers. By continuing, you agree to the Mini Apps Terms of Service.",
                    )
                    .size(12.5)
                    .color(palette.window_fg),
                );
                if bot.request_write_access {
                    ui.add_space(10.0);
                    ui.checkbox(
                        allow_write,
                        format!("Allow {} to send you messages", bot.short_name),
                    );
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    if box_action_button(
                        ui,
                        "Cancel",
                        palette.window_bg_over,
                        palette.window_fg,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::Closed);
                    }
                    if box_action_button(
                        ui,
                        "Continue",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        half,
                        34.0,
                    )
                    .clicked()
                    {
                        outcome = Some(BotModalOutcome::AttachDisclaimerAccepted {
                            bot: bot.clone(),
                            allow_write: *allow_write,
                        });
                    }
                });
            }
        }
    });

    outcome
}

fn paint_peer_picker_row(
    ui: &mut Ui,
    peer: &BotPeerCandidate,
    selected: bool,
    palette: &Palette,
) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 44.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if selected {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(8),
            with_alpha(palette.window_bg_active, 0.14),
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), palette.window_bg_over);
    }

    let av = Pos2::new(rect.left() + 22.0, rect.center().y);
    ui.painter()
        .circle_filled(av, 15.0, palette.window_bg_active);
    ui.painter().text(
        av,
        Align2::CENTER_CENTER,
        peer.initials(),
        FontId::proportional(11.5),
        palette.window_fg_active,
    );

    ui.painter().text(
        Pos2::new(rect.left() + 46.0, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        &peer.title,
        FontId::proportional(13.0),
        palette.window_bold_fg,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 46.0, rect.top() + 30.0),
        Align2::LEFT_CENTER,
        &peer.subtitle,
        FontId::proportional(11.0),
        palette.window_sub_text_fg,
    );

    // Selection checkmark badge
    let chk_c = Pos2::new(rect.right() - 18.0, rect.center().y);
    if selected {
        ui.painter()
            .circle_filled(chk_c, 9.0, palette.window_bg_active);
        tg_app_core::ui_icons::paint_check(ui.painter(), chk_c, 11.0, palette.window_fg_active);
    } else {
        ui.painter()
            .circle_stroke(chk_c, 9.0, Stroke::new(1.4, palette.divider));
    }

    resp
}

/// Paints a floating Telegram-style dark pill toast notification inside the chat area of `bounds`.
pub fn paint_bot_toast(painter: &egui::Painter, bounds: Rect, message: &str) {
    let text_w = (message.len() as f32 * 7.0 + 34.0).clamp(160.0, bounds.width() - 40.0);
    let toast_rect = Rect::from_center_size(
        Pos2::new(bounds.center().x, bounds.top() + 114.0),
        Vec2::new(text_w, 34.0),
    );
    painter.rect_filled(
        toast_rect,
        CornerRadius::same(17),
        Color32::from_rgba_unmultiplied(0x1E, 0x28, 0x33, 232),
    );
    painter.text(
        toast_rect.center(),
        Align2::CENTER_CENTER,
        message,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
}
