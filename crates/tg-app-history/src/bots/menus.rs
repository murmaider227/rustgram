//! Bot commands menu button, slash-commands list popup, empty bot chat info header
//! ("What can this bot do?"), and attach menu bots list popup.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/controls/history_view_bot_menu_button.cpp`
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp` & `bot_command.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_empty_list_bubble.cpp` & `history_view_about_view.cpp`
//! - `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Response, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use tg_bots::commands::{
    BotCommand, BotInfo, BotMenuButton, BotStatus, PrivacyPolicyAction,
    resolve_privacy_policy_action, wrap_command_in_chat,
};
use tg_bots::mini_app::{AttachMenuBot, AttachMenuStore};
use tg_bots::stand_in::{PeerId, PeerKind, UserId};

use crate::bots::ui_helpers::{box_action_button, paint_procedural_art, with_alpha};
use tg_app_core::stand_in::theme::Palette;

// ---------------------------------------------------------------------------
// 1. Bot Menu Button (compose bar left: Commands vs WebApp)
// ---------------------------------------------------------------------------

/// Event emitted when the compose-bar [`BotMenuButtonWidget`] is clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotMenuButtonAction {
    /// Toggle the slash-commands list popup (`BotMenuButton::Default` / `Commands`).
    ToggleCommandsPopup,
    /// Open the bot's configured Mini App WebView (`BotMenuButton::WebApp`).
    OpenWebApp {
        /// Visible label on the menu button.
        text: String,
        /// Mini App launch URL.
        url: String,
    },
}

/// Paints the bot menu button beside the compose input field (`history_view_bot_menu_button.cpp`).
///
/// Supports both `BotMenuButton::Commands` / `Default` (menu pill with hamburger/slash icon)
/// and `BotMenuButton::WebApp { text, url }` (accent blue pill with Mini App icon + label).
pub fn paint_bot_menu_button(
    ui: &mut Ui,
    button: &BotMenuButton,
    has_commands: bool,
    popup_open: bool,
    compact: bool,
    palette: &Palette,
) -> Option<BotMenuButtonAction> {
    if !button.is_web_app() && !has_commands {
        return None;
    }

    let is_webapp = button.is_web_app();
    let label = match button {
        BotMenuButton::WebApp { text, .. } if !text.is_empty() => text.as_str(),
        _ => "Menu",
    };

    let font = FontId::proportional(12.5);
    let text_w = if compact {
        0.0
    } else {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let width = if compact {
        34.0
    } else {
        (text_w + 36.0).clamp(68.0, 140.0)
    };

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let (bg, fg) = if is_webapp || popup_open {
        let base = palette.window_bg_active;
        let fill = if resp.is_pointer_button_down_on() {
            with_alpha(base, 0.82)
        } else if resp.hovered() {
            with_alpha(base, 0.92)
        } else {
            base
        };
        (fill, palette.window_fg_active)
    } else {
        let fill = if resp.hovered() {
            with_alpha(palette.window_bg_active, 0.16)
        } else {
            with_alpha(palette.window_bg_active, 0.11)
        };
        (fill, palette.window_active_text_fg)
    };

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(16), bg);

    // Leading icon: hamburger menu lines for Commands, Mini App window glyph for WebApp
    let icon_c = Pos2::new(
        if compact {
            rect.center().x
        } else {
            rect.left() + 16.0
        },
        rect.center().y,
    );
    if is_webapp {
        let box_r = Rect::from_center_size(icon_c, Vec2::new(11.0, 9.5));
        painter.rect_stroke(
            box_r,
            CornerRadius::same(2),
            Stroke::new(1.4, fg),
            StrokeKind::Inside,
        );
        painter.line_segment(
            [
                Pos2::new(box_r.left() + 1.0, box_r.top() + 3.2),
                Pos2::new(box_r.right() - 1.0, box_r.top() + 3.2),
            ],
            Stroke::new(1.2, fg),
        );
    } else {
        for dy in [-3.5_f32, 0.0, 3.5] {
            painter.line_segment(
                [
                    Pos2::new(icon_c.x - 5.5, icon_c.y + dy),
                    Pos2::new(icon_c.x + 5.5, icon_c.y + dy),
                ],
                Stroke::new(1.6, fg),
            );
        }
    }

    if !compact {
        painter.text(
            Pos2::new(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            fg,
        );
    }

    if resp.clicked() {
        match button {
            BotMenuButton::WebApp { text, url } if !text.is_empty() && !url.is_empty() => {
                Some(BotMenuButtonAction::OpenWebApp {
                    text: text.clone(),
                    url: url.clone(),
                })
            }
            _ => Some(BotMenuButtonAction::ToggleCommandsPopup),
        }
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 2. Bot Commands List Popup
// ---------------------------------------------------------------------------

/// Event emitted by [`BotCommandsPopup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommandsPopupEvent {
    /// User chose a command to execute (already formatted with leading `/` and optional `@bot`).
    ExecuteCommand(String),
    /// User closed the popup.
    Dismissed,
}

/// Filters `commands` by an optional slash prefix (e.g. `"/set"` matches `"settings"`).
#[must_use]
pub fn filter_bot_commands<'a>(commands: &'a [BotCommand], filter: &str) -> Vec<&'a BotCommand> {
    let trimmed = filter.trim().trim_start_matches('/').to_ascii_lowercase();
    if trimmed.is_empty() {
        return commands.iter().collect();
    }
    commands
        .iter()
        .filter(|c| {
            c.command.to_ascii_lowercase().starts_with(&trimmed)
                || c.description.to_ascii_lowercase().contains(&trimmed)
        })
        .collect()
}

/// Parameters passed to [`show_bot_commands_popup`].
#[derive(Debug, Clone, Copy)]
pub struct BotCommandsPopupParams<'a> {
    /// Commands advertised in this chat.
    pub commands: &'a [BotCommand],
    /// Optional slash filter prefix from the compose input (e.g. `"/set"`).
    pub filter_prefix: &'a str,
    /// Peer kind (`PeerKind::Group` enables `@bot` suffix wrapping).
    pub peer_kind: PeerKind,
    /// Chat bot-presence status for [`wrap_command_in_chat`].
    pub peer_bot_status: BotStatus,
    /// Target bot username without leading `@`.
    pub bot_username: &'a str,
    /// Maximum popup width in logical pixels.
    pub max_width: f32,
}

/// Renders the floating bot commands popup above the compose bar.
///
/// Automatically applies `tg_bots::commands::wrap_command_in_chat` when used in a group chat
/// containing bots (`peer_kind == PeerKind::Group`).
pub fn show_bot_commands_popup(
    ui: &mut Ui,
    params: BotCommandsPopupParams<'_>,
    palette: &Palette,
) -> Option<BotCommandsPopupEvent> {
    let BotCommandsPopupParams {
        commands,
        filter_prefix,
        peer_kind,
        peer_bot_status,
        bot_username,
        max_width,
    } = params;
    let filtered = filter_bot_commands(commands, filter_prefix);
    let width = max_width.clamp(280.0, 420.0);
    let row_h = 44.0;
    let header_h = 34.0;
    let content_h = (filtered.len().max(1) as f32 * row_h).min(240.0);
    let total_h = header_h + content_h + 6.0;

    let (panel_rect, _) = ui.allocate_exact_size(Vec2::new(width, total_h), Sense::hover());
    ui.painter().rect_filled(
        panel_rect.expand(1.5),
        CornerRadius::same(11),
        with_alpha(palette.divider, 0.9),
    );
    ui.painter()
        .rect_filled(panel_rect, CornerRadius::same(10), palette.window_bg);

    // Header
    let hdr = Rect::from_min_size(panel_rect.min, Vec2::new(width, header_h));
    ui.painter().rect_filled(
        hdr,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        palette.window_bg_over,
    );
    ui.painter().line_segment(
        [hdr.left_bottom(), hdr.right_bottom()],
        Stroke::new(1.0, palette.divider),
    );
    ui.painter().text(
        Pos2::new(hdr.left() + 12.0, hdr.center().y),
        Align2::LEFT_CENTER,
        format!("Bot Commands (@{bot_username})"),
        FontId::proportional(12.5),
        palette.window_bold_fg,
    );

    let mut emitted = None;

    let close_rect = Rect::from_min_size(
        Pos2::new(hdr.right() - 28.0, hdr.top() + 6.0),
        Vec2::splat(22.0),
    );
    let close_resp = ui.interact(close_rect, Id::unique("tg_bot_cmds_close"), Sense::click());
    if close_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().text(
        close_rect.center(),
        Align2::CENTER_CENTER,
        "×",
        FontId::proportional(14.5),
        palette.window_sub_text_fg,
    );
    if close_resp.clicked() {
        emitted = Some(BotCommandsPopupEvent::Dismissed);
    }

    let list_rect = Rect::from_min_size(
        Pos2::new(panel_rect.left(), hdr.bottom() + 2.0),
        Vec2::new(width, content_h),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
        ScrollArea::vertical()
            .id_salt("tg_bot_commands_scroll")
            .max_height(content_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filtered.is_empty() {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        "No matching commands",
                        FontId::proportional(12.5),
                        palette.window_sub_text_fg,
                    );
                    return;
                }

                for (i, cmd) in filtered.iter().enumerate() {
                    let (row_rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width - 8.0, row_h), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            row_rect.shrink2(Vec2::new(4.0, 1.0)),
                            CornerRadius::same(6),
                            palette.window_bg_over,
                        );
                    }

                    // Slash badge circle
                    let badge_c = Pos2::new(row_rect.left() + 22.0, row_rect.center().y);
                    ui.painter().circle_filled(
                        badge_c,
                        12.0,
                        with_alpha(palette.window_bg_active, 0.14),
                    );
                    ui.painter().text(
                        badge_c,
                        Align2::CENTER_CENTER,
                        "/",
                        FontId::proportional(13.5),
                        palette.window_active_text_fg,
                    );

                    let cmd_text = format!("/{}", cmd.command);
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Button, true, cmd_text.clone())
                    });
                    ui.painter().text(
                        Pos2::new(row_rect.left() + 44.0, row_rect.top() + 14.0),
                        Align2::LEFT_CENTER,
                        &cmd_text,
                        FontId::proportional(13.0),
                        palette.window_active_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(row_rect.left() + 44.0, row_rect.top() + 31.0),
                        Align2::LEFT_CENTER,
                        &cmd.description,
                        FontId::proportional(11.5),
                        palette.window_sub_text_fg,
                    );

                    if i + 1 < filtered.len() {
                        ui.painter().line_segment(
                            [
                                Pos2::new(row_rect.left() + 44.0, row_rect.bottom() - 0.5),
                                Pos2::new(row_rect.right() - 8.0, row_rect.bottom() - 0.5),
                            ],
                            Stroke::new(1.0, with_alpha(palette.divider, 0.55)),
                        );
                    }

                    if resp.clicked() {
                        let wrapped = if peer_kind == PeerKind::Group {
                            wrap_command_in_chat(peer_bot_status, &cmd_text, bot_username)
                        } else {
                            cmd_text
                        };
                        emitted = Some(BotCommandsPopupEvent::ExecuteCommand(wrapped));
                    }
                }
            });
    });

    emitted
}

// ---------------------------------------------------------------------------
// 3. Empty Bot Chat Info Header ("What can this bot do?")
// ---------------------------------------------------------------------------

/// Event emitted by [`show_bot_info_header`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotInfoHeaderEvent {
    /// User clicked the "START" button to send `/start`.
    StartBot,
    /// User clicked the bot's Privacy Policy link.
    OpenPrivacyPolicy(PrivacyPolicyAction),
    /// User clicked "Open App" when `bot_info.has_main_app` is true.
    OpenMainApp,
}

/// Renders the centered empty-chat bot intro card (`history_view_empty_list_bubble.cpp` /
/// `history_view_about_view.cpp`): description photo banner, "What can this bot do?" heading,
/// bot description, monthly active users and privacy policy link. The START button is the
/// history's bottom bar ([`crate::history::bars::paint_bot_start_bar`]).
pub fn show_bot_info_header(
    ui: &mut Ui,
    bot_name: &str,
    bot_username: &str,
    bot_info: &BotInfo,
    palette: &Palette,
) -> Option<BotInfoHeaderEvent> {
    let mut emitted = None;
    let card_w = (ui.available_width() - 48.0).clamp(280.0, 380.0);

    ui.vertical_centered(|ui| {
        ui.allocate_ui(Vec2::new(card_w, 0.0), |ui| {
            let frame = egui::Frame::new()
                .fill(palette.msg_in_bg)
                .corner_radius(CornerRadius::same(14))
                .stroke(Stroke::new(1.0, palette.divider))
                .inner_margin(egui::Margin::same(0));

            frame.show(ui, |ui| {
                // Optional description banner photo (`bot_info.description_photo_id`)
                if let Some(photo_id) = bot_info.description_photo_id {
                    let (banner_rect, _) =
                        ui.allocate_exact_size(Vec2::new(card_w, 138.0), Sense::hover());
                    paint_procedural_art(
                        ui.painter(),
                        banner_rect,
                        photo_id.0,
                        CornerRadius {
                            nw: 14,
                            ne: 14,
                            sw: 0,
                            se: 0,
                        },
                        Some("BOT PREVIEW"),
                    );
                    // Overlay bot avatar badge in bottom-left of banner
                    let av_c = Pos2::new(banner_rect.left() + 34.0, banner_rect.bottom() - 28.0);
                    ui.painter().circle_filled(av_c, 22.0, palette.window_bg);
                    ui.painter()
                        .circle_filled(av_c, 19.5, palette.window_bg_active);
                    let init = bot_name
                        .chars()
                        .next()
                        .unwrap_or('B')
                        .to_ascii_uppercase()
                        .to_string();
                    ui.painter().text(
                        av_c,
                        Align2::CENTER_CENTER,
                        init,
                        FontId::proportional(17.0),
                        palette.window_fg_active,
                    );
                    ui.painter().text(
                        Pos2::new(av_c.x + 30.0, av_c.y - 6.0),
                        Align2::LEFT_CENTER,
                        bot_name,
                        FontId::proportional(15.0),
                        Color32::WHITE,
                    );
                    ui.painter().text(
                        Pos2::new(av_c.x + 30.0, av_c.y + 10.0),
                        Align2::LEFT_CENTER,
                        format!("@{bot_username}"),
                        FontId::proportional(11.5),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                    );
                }

                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("What can this bot do?")
                                .size(14.5)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(6.0);
                        let desc = if bot_info.description.is_empty() {
                            "Press Start below to begin interacting with this bot."
                        } else {
                            &bot_info.description
                        };
                        ui.label(
                            egui::RichText::new(desc)
                                .size(13.0)
                                .color(palette.window_fg),
                        );

                        if bot_info.active_users > 0 {
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} monthly active users",
                                    format_thousands(bot_info.active_users)
                                ))
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                            );
                        }

                        // START lives in the bottom bar in place of the composer
                        // (`HistoryWidget::_botStart`), not inside this card.
                        let inner_w = ui.available_width();
                        if bot_info.has_main_app {
                            ui.add_space(6.0);
                            if box_action_button(
                                ui,
                                "Open Mini App",
                                with_alpha(palette.window_bg_active, 0.14),
                                palette.window_active_text_fg,
                                inner_w,
                                32.0,
                            )
                            .clicked()
                            {
                                emitted = Some(BotInfoHeaderEvent::OpenMainApp);
                            }
                        }

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::Label::new(
                                    egui::RichText::new("Privacy Policy")
                                        .size(11.5)
                                        .color(palette.window_active_text_fg),
                                )
                                .sense(Sense::click()),
                            );
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                emitted = Some(BotInfoHeaderEvent::OpenPrivacyPolicy(
                                    resolve_privacy_policy_action(bot_info),
                                ));
                            }
                        });
                    });
            });
        });
    });

    emitted
}

// ---------------------------------------------------------------------------
// 4. Attach Menu Bots Popup
// ---------------------------------------------------------------------------

/// Event emitted by [`show_attach_menu_bots_popup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachMenuPopupEvent {
    /// Standard attachment action (`"Photo or Video"`, `"File"`, `"Poll"`, `"Location"`).
    StandardAttach(&'static str),
    /// User clicked an active attach-menu bot to open its Mini App.
    LaunchBot(AttachMenuBot),
    /// User clicked "+ Add" on an inactive attach-menu bot.
    AddBot(UserId),
    /// User clicked "Remove" on an active attach-menu bot.
    RemoveBot(UserId),
    /// Popup closed.
    Dismissed,
}

/// Renders the chat attachment popup including standard attachments and the attach-menu bots list
/// filtered by `AttachMenuStore::visible_in_attach_menu(peer_id, peer_kind)`.
pub fn show_attach_menu_bots_popup(
    ui: &mut Ui,
    store: &AttachMenuStore,
    peer_id: PeerId,
    peer_kind: PeerKind,
    palette: &Palette,
) -> Option<AttachMenuPopupEvent> {
    let width = 264.0;
    let visible_bots = store.visible_in_attach_menu(peer_id, peer_kind);
    let inactive_bots: Vec<&AttachMenuBot> = store
        .bots
        .iter()
        .filter(|b| b.inactive && b.in_attach_menu)
        .collect();

    let mut emitted = None;
    let frame = egui::Frame::new()
        .fill(palette.window_bg)
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::new(1.0, palette.divider))
        .inner_margin(egui::Margin::same(8));

    frame.show(ui, |ui| {
        ui.set_width(width);

        // Header
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Attach")
                    .size(13.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new("×")
                                .size(15.0)
                                .color(palette.window_sub_text_fg),
                        )
                        .sense(Sense::click()),
                    )
                    .clicked()
                {
                    emitted = Some(AttachMenuPopupEvent::Dismissed);
                }
            });
        });

        ui.add_space(4.0);
        for &(label, tag, col) in &[
            ("Photo or Video", "PIC", Color32::from_rgb(0x40, 0xA7, 0xE3)),
            ("File", "DOC", Color32::from_rgb(0x52, 0x88, 0xC1)),
            ("Poll", "BAR", Color32::from_rgb(0xF5, 0xA6, 0x23)),
            ("Checklist", "CHK", Color32::from_rgb(0x2E, 0xA6, 0x52)),
            ("Music", "MUS", Color32::from_rgb(0x9B, 0x59, 0xB6)),
        ] {
            if attach_menu_row(ui, width, label, tag, col, None, palette).clicked() {
                emitted = Some(AttachMenuPopupEvent::StandardAttach(label));
            }
        }

        // Divider before Attach Menu Bots section
        ui.add_space(4.0);
        let (sep, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
        ui.painter()
            .rect_filled(sep, CornerRadius::ZERO, palette.divider);
        ui.add_space(4.0);

        ui.label(
            egui::RichText::new("MINI APP BOTS")
                .size(10.5)
                .strong()
                .color(palette.window_sub_text_fg),
        );
        ui.add_space(2.0);

        if visible_bots.is_empty() {
            ui.label(
                egui::RichText::new("No bots enabled for this chat type.")
                    .size(12.0)
                    .color(palette.window_sub_text_fg),
            );
        } else {
            for (idx, bot) in visible_bots.iter().enumerate() {
                let badge = if store.show_main_menu_new_badge(bot) {
                    Some("NEW")
                } else {
                    None
                };
                let color = match idx % 3 {
                    0 => Color32::from_rgb(0x7C, 0x5C, 0xFA),
                    1 => Color32::from_rgb(0x2E, 0xA6, 0x52),
                    _ => Color32::from_rgb(0xE0, 0x5B, 0x9E),
                };
                let short_tag: String = bot
                    .short_name
                    .chars()
                    .take(2)
                    .collect::<String>()
                    .to_ascii_uppercase();
                ui.horizontal(|ui| {
                    let row_w = width - 54.0;
                    if attach_menu_row(
                        ui,
                        row_w,
                        &bot.short_name,
                        &short_tag,
                        color,
                        badge,
                        palette,
                    )
                    .clicked()
                    {
                        emitted = Some(AttachMenuPopupEvent::LaunchBot((*bot).clone()));
                    }
                    let rem = ui.add(
                        egui::Label::new(
                            egui::RichText::new("Remove")
                                .size(11.0)
                                .color(palette.attention_fg),
                        )
                        .sense(Sense::click()),
                    );
                    if rem.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if rem.clicked() {
                        emitted = Some(AttachMenuPopupEvent::RemoveBot(bot.bot_id));
                    }
                });
            }
        }

        if !inactive_bots.is_empty() {
            ui.add_space(4.0);
            for bot in inactive_bots {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&bot.short_name)
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let add = ui.add(
                            egui::Label::new(
                                egui::RichText::new("+ Add")
                                    .size(11.5)
                                    .strong()
                                    .color(palette.window_active_text_fg),
                            )
                            .sense(Sense::click()),
                        );
                        if add.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if add.clicked() {
                            emitted = Some(AttachMenuPopupEvent::AddBot(bot.bot_id));
                        }
                    });
                });
            }
        }
    });

    emitted
}

fn attach_menu_row(
    ui: &mut Ui,
    width: f32,
    label: &str,
    icon_tag: &str,
    icon_bg: Color32,
    badge: Option<&str>,
    palette: &Palette,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
    }
    let ic = Pos2::new(rect.left() + 16.0, rect.center().y);
    ui.painter().circle_filled(ic, 12.0, icon_bg);
    ui.painter().text(
        ic,
        Align2::CENTER_CENTER,
        icon_tag,
        FontId::proportional(9.0),
        Color32::WHITE,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 36.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        palette.window_fg,
    );
    if let Some(b) = badge {
        let br = Rect::from_min_size(
            Pos2::new(rect.right() - 42.0, rect.center().y - 8.5),
            Vec2::new(36.0, 17.0),
        );
        ui.painter()
            .rect_filled(br, CornerRadius::same(8), palette.window_bg_active);
        ui.painter().text(
            br.center(),
            Align2::CENTER_CENTER,
            b,
            FontId::proportional(9.5),
            palette.window_fg_active,
        );
    }
    resp
}

fn format_thousands(n: i32) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        out.push('-');
    }
    out.chars().rev().collect()
}
