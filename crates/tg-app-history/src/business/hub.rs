//! Telegram Business hub screen.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_business.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    Ui, Vec2,
};

use crate::business::model::{
    BUSINESS_PROMO_ORDER, BusinessFeatureKey, BusinessOverviewSnapshot, business_feature_meta,
    extract_day_intervals, is_full_open, join_intervals,
};
use crate::business::ui_helpers::{
    BUSINESS_BLUE, BUSINESS_TEAL, PREMIUM_INDIGO, PREMIUM_PINK, PREMIUM_PURPLE, SUCCESS_GREEN,
    SUCCESS_SURFACE, paint_business_emblem, paint_horizontal_gradient, paint_star_emblem,
    pill_button, section_divider, settings_toggle_row, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Navigation action emitted by [`BusinessHubView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusinessHubAction {
    /// Open one of the Business feature sub-screens.
    OpenFeature(BusinessFeatureKey),
    /// Toggle the "Hide Sponsored Messages" switch (`BuildSponsoredSection`).
    ToggleSponsored(bool),
    /// Open the Telegram Premium screen.
    OpenPremium,
    /// Open the Premium Limits boxes gallery.
    OpenPremiumLimits,
    /// Open the Emoji Status & Profile Badge picker.
    OpenEmojiStatus,
    /// Display a toast message (e.g. non-Premium preview gate or folder tags info).
    ShowToast(String),
}

/// Stateless renderer for the Telegram Business hub (`settings_business.cpp`).
pub struct BusinessHubView;

impl BusinessHubView {
    /// Renders the Business hub overview and returns any user-triggered [`BusinessHubAction`].
    pub fn show(
        ui: &mut Ui,
        snapshot: &BusinessOverviewSnapshot,
        palette: &Palette,
    ) -> Option<BusinessHubAction> {
        let mut action = None;

        // Top gradient hero cover (`Business::createPinnedToTop` with 3D coin / briefcase logo)
        let header_w = ui.available_width().max(260.0);
        let (hero_rect, _) = ui.allocate_exact_size(Vec2::new(header_w, 138.0), Sense::hover());
        paint_horizontal_gradient(
            ui.painter(),
            hero_rect,
            BUSINESS_BLUE,
            Color32::from_rgb(0x1D, 0xAC, 0xCE),
            BUSINESS_TEAL,
        );
        paint_business_emblem(
            ui.painter(),
            Pos2::new(hero_rect.center().x, hero_rect.top() + 42.0),
            24.0,
        );
        ui.painter().text(
            Pos2::new(hero_rect.center().x, hero_rect.top() + 84.0),
            Align2::CENTER_CENTER,
            "Telegram Business",
            FontId::proportional(20.0),
            Color32::WHITE,
        );
        let hero_sub = if snapshot.is_premium {
            "All Telegram Business tools are unlocked with your Telegram Premium plan."
        } else {
            "Turn your Telegram account into a business workspace with Telegram Premium."
        };
        ui.painter().text(
            Pos2::new(hero_rect.center().x, hero_rect.top() + 111.0),
            Align2::CENTER_CENTER,
            hero_sub,
            FontId::proportional(11.8),
            Color32::from_rgba_unmultiplied(255, 255, 255, 232),
        );

        // Business features list in exact `business_promo_order` (`FallbackOrder()`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(ui, "Business Features", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status_text = if snapshot.is_premium {
                            "Premium Active"
                        } else {
                            "Free Preview"
                        };
                        let (bg, fg) = if snapshot.is_premium {
                            (SUCCESS_SURFACE, SUCCESS_GREEN)
                        } else {
                            (with_alpha(PREMIUM_PURPLE, 0.14), PREMIUM_PURPLE)
                        };
                        let (badge, _) =
                            ui.allocate_exact_size(Vec2::new(102.0, 20.0), Sense::hover());
                        ui.painter().rect_filled(badge, CornerRadius::same(10), bg);
                        ui.painter().text(
                            badge.center(),
                            Align2::CENTER_CENTER,
                            status_text,
                            FontId::proportional(10.5),
                            fg,
                        );
                    });
                });
                ui.add_space(6.0);

                for key in BUSINESS_PROMO_ORDER {
                    let meta = business_feature_meta(key);
                    let summary = feature_status_summary(key, snapshot);
                    let width = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                    }

                    // Colored rounded icon badge
                    let icon_col =
                        Color32::from_rgb(meta.icon_rgb[0], meta.icon_rgb[1], meta.icon_rgb[2]);
                    let icon_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 6.0, rect.center().y - 17.0),
                        Vec2::splat(34.0),
                    );
                    ui.painter()
                        .rect_filled(icon_rect, CornerRadius::same(9), icon_col);
                    ui.painter().text(
                        icon_rect.center(),
                        Align2::CENTER_CENTER,
                        meta.icon_glyph,
                        FontId::proportional(11.5),
                        Color32::WHITE,
                    );

                    // Title + description
                    let text_x = icon_rect.right() + 12.0;
                    ui.painter().text(
                        Pos2::new(text_x, rect.top() + 18.0),
                        Align2::LEFT_CENTER,
                        meta.title,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().text(
                        Pos2::new(text_x, rect.top() + 38.0),
                        Align2::LEFT_CENTER,
                        meta.description,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );

                    // Right status chip + chevron
                    let (chip_text, chip_active) = summary;
                    let chip_col = if chip_active {
                        palette.window_active_text_fg
                    } else {
                        palette.window_sub_text_fg
                    };
                    ui.painter().text(
                        Pos2::new(rect.right() - 20.0, rect.top() + 18.0),
                        Align2::RIGHT_CENTER,
                        chip_text,
                        FontId::proportional(11.0),
                        chip_col,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 6.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        ">",
                        FontId::proportional(13.0),
                        palette.window_sub_text_fg,
                    );

                    if resp.clicked() {
                        // Mirror `settings_business.cpp`: Chatbots is always available;
                        // FilterTags opens Premium preview; others require Premium (or preview).
                        if key == BusinessFeatureKey::FolderTags {
                            action = Some(BusinessHubAction::OpenPremium);
                        } else if !snapshot.is_premium && key != BusinessFeatureKey::BusinessBots {
                            action = Some(BusinessHubAction::ShowToast(format!(
                                "{} requires Telegram Premium — opening Telegram Premium overview.",
                                meta.title
                            )));
                        } else {
                            action = Some(BusinessHubAction::OpenFeature(key));
                        }
                    }

                    thin_divider(ui, palette);
                }
            });

        // Sponsored Messages toggle section (`BuildSponsoredSection`, visible when Premium)
        if snapshot.is_premium {
            section_divider(ui, palette);
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 4))
                .show(ui, |ui| {
                    subsection_title(ui, "Sponsored Messages", palette);
                    ui.add_space(4.0);
                    let mut disabled = snapshot.sponsored_disabled;
                    if settings_toggle_row(
                        ui,
                        "Hide Sponsored Messages",
                        Some("Disable ads in public channels on your Premium Business account."),
                        &mut disabled,
                        palette,
                    ) {
                        action = Some(BusinessHubAction::ToggleSponsored(disabled));
                    }
                });
        }

        section_divider(ui, palette);

        // Premium, Limits & Emoji Status quick-launch strip
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 6))
            .show(ui, |ui| {
                subsection_title(ui, "Telegram Premium & Profile Identity", palette);
                ui.add_space(8.0);

                Frame::NONE
                    .fill(with_alpha(PREMIUM_PURPLE, 0.08))
                    .stroke(Stroke::new(1.0, with_alpha(PREMIUM_PURPLE, 0.35)))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (star_box, _) =
                                ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                            ui.painter().rect_filled(
                                star_box,
                                CornerRadius::same(10),
                                PREMIUM_INDIGO,
                            );
                            paint_star_emblem(
                                ui.painter(),
                                star_box.center(),
                                11.5,
                                Color32::WHITE,
                            );
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Telegram Premium, Limits & Emoji Status")
                                        .size(13.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Compare Free vs Premium tiers, inspect exceeded limit boxes, or pick a profile emoji status.",
                                    )
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                                );
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                            if pill_button(
                                ui,
                                "Telegram Premium",
                                PREMIUM_PURPLE,
                                Color32::WHITE,
                                None,
                            )
                            .clicked()
                            {
                                action = Some(BusinessHubAction::OpenPremium);
                            }
                            if pill_button(
                                ui,
                                "Premium Limit Boxes",
                                palette.window_bg,
                                PREMIUM_INDIGO,
                                Some(with_alpha(PREMIUM_INDIGO, 0.45)),
                            )
                            .clicked()
                            {
                                action = Some(BusinessHubAction::OpenPremiumLimits);
                            }
                            if pill_button(
                                ui,
                                "Badge & Emoji Status",
                                palette.window_bg,
                                PREMIUM_PINK,
                                Some(with_alpha(PREMIUM_PINK, 0.45)),
                            )
                            .clicked()
                            {
                                action = Some(BusinessHubAction::OpenEmojiStatus);
                            }
                        });
                    });
            });

        action
    }
}

fn feature_status_summary(
    key: BusinessFeatureKey,
    snapshot: &BusinessOverviewSnapshot,
) -> (String, bool) {
    match key {
        BusinessFeatureKey::GreetingMessage => {
            if snapshot.greeting.is_enabled() {
                (
                    format!("On ({}d)", snapshot.greeting.no_activity_days),
                    true,
                )
            } else {
                ("Off".to_owned(), false)
            }
        }
        BusinessFeatureKey::AwayMessage => {
            if snapshot.away.is_enabled() {
                ("On".to_owned(), true)
            } else {
                ("Off".to_owned(), false)
            }
        }
        BusinessFeatureKey::QuickReplies => {
            let count = snapshot
                .shortcuts
                .values()
                .filter(|s| !s.messages.is_empty())
                .count();
            (format!("{count} shortcuts"), count > 0)
        }
        BusinessFeatureKey::BusinessHours => {
            if snapshot.details.hours.is_enabled() {
                let mon = extract_day_intervals(&snapshot.details.hours.intervals, 0);
                let label = if is_full_open(&mon) {
                    "24h".to_owned()
                } else if mon.is_non_empty() {
                    join_intervals(&mon)
                } else {
                    "Custom".to_owned()
                };
                (label, true)
            } else {
                ("Off".to_owned(), false)
            }
        }
        BusinessFeatureKey::BusinessLocation => {
            if snapshot.details.location.is_set() {
                ("Set".to_owned(), true)
            } else {
                ("Not set".to_owned(), false)
            }
        }
        BusinessFeatureKey::ChatLinks => {
            let n = snapshot.chat_links.len();
            (format!("{n} links"), n > 0)
        }
        BusinessFeatureKey::ChatIntro => {
            if snapshot.details.intro.is_set() {
                ("Customized".to_owned(), true)
            } else {
                ("Default".to_owned(), false)
            }
        }
        BusinessFeatureKey::BusinessBots => match &snapshot.chatbots.bot {
            Some(bot) => (format!("@{}", bot.username), true),
            None => ("None".to_owned(), false),
        },
        BusinessFeatureKey::FolderTags => (
            if snapshot.is_premium {
                "Included".to_owned()
            } else {
                "Premium".to_owned()
            },
            snapshot.is_premium,
        ),
    }
}
