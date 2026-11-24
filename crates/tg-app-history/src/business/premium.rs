//! Telegram Premium settings screen, Premium Limit Boxes, and Premium Badge /
//! Emoji Status picker entry.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/sections/settings_premium.cpp`
//! - `Telegram/SourceFiles/boxes/premium_limits_box.cpp`
//! - `Telegram/SourceFiles/data/data_emoji_statuses.h`
//! - `Telegram/SourceFiles/info/profile/info_profile_emoji_status_panel.h`
//! - `Telegram/SourceFiles/menu/menu_emoji_status.cpp`

use std::collections::BTreeSet;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    ActiveEmojiStatus, BusinessOverviewSnapshot, EmojiStatusCategory, EmojiStatusDuration,
    EmojiStatusItem, PremiumFeatureEntry, PremiumLimitKind, PremiumLimitsData,
};
use crate::business::ui_helpers::{
    BUSINESS_GOLD, PREMIUM_INDIGO, PREMIUM_PINK, PREMIUM_PURPLE, SUCCESS_GREEN, SUCCESS_SURFACE,
    paint_avatar, paint_horizontal_gradient, paint_star_emblem, pill_button, primary_button,
    section_divider, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Outgoing navigation action from [`PremiumSettingsView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PremiumSettingsAction {
    /// Open the Premium Limits boxes previewer.
    OpenLimits(PremiumLimitKind),
    /// Open the Emoji Status picker.
    OpenEmojiStatus,
    /// Open the Business hub.
    OpenBusinessHub,
    /// Open `PaymentsScreen::Premium` (read-only invoice & plan preview, never auto-purchasing).
    OpenPaymentsPremium,
    /// Toggle simulated Premium state for testing Free vs Premium UI.
    SetPremium(bool),
    /// Show a toast notification.
    Toast(String),
}

// -----------------------------------------------------------------------------
// Telegram Premium Settings (`settings/sections/settings_premium.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Telegram Premium settings (`settings_premium.cpp`).
pub struct PremiumSettingsView {
    /// Selected billing tier index (`0` = Annual, `1` = 6 Months, `2` = Monthly).
    pub selected_plan: usize,
    /// Currently expanded feature index showing its static animation preview card.
    pub expanded_feature: Option<usize>,
}

impl Default for PremiumSettingsView {
    fn default() -> Self {
        Self::new()
    }
}

impl PremiumSettingsView {
    /// Creates a [`PremiumSettingsView`] with Annual selected and the first feature preview open.
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected_plan: 0,
            expanded_feature: Some(2), // Highlight "Doubled Limits" static preview by default
        }
    }

    /// Formats the Subscribe CTA label for the selected plan.
    #[must_use]
    pub fn subscribe_label(&self, snapshot: &BusinessOverviewSnapshot) -> String {
        if let Some(plan) = snapshot.premium_plans.get(self.selected_plan) {
            format!("Telegram Premium • {} / month", plan.price_per_month)
        } else {
            "Subscribe to Telegram Premium".to_owned()
        }
    }

    /// Renders the Telegram Premium settings screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &BusinessOverviewSnapshot,
        palette: &Palette,
    ) -> Option<PremiumSettingsAction> {
        let mut action = None;

        // Gradient hero header (`settings_premium.cpp`)
        let header_w = ui.available_width().max(260.0);
        let (hero_rect, _) = ui.allocate_exact_size(Vec2::new(header_w, 138.0), Sense::hover());
        paint_horizontal_gradient(
            ui.painter(),
            hero_rect,
            PREMIUM_INDIGO,
            PREMIUM_PURPLE,
            PREMIUM_PINK,
        );
        paint_star_emblem(
            ui.painter(),
            Pos2::new(hero_rect.center().x, hero_rect.top() + 42.0),
            24.0,
            Color32::WHITE,
        );
        ui.painter().text(
            Pos2::new(hero_rect.center().x, hero_rect.top() + 84.0),
            Align2::CENTER_CENTER,
            "Telegram Premium",
            FontId::proportional(20.0),
            Color32::WHITE,
        );
        let subtitle = if snapshot.is_premium {
            "Your Telegram Premium subscription is active (demo preview)."
        } else {
            "Go beyond the limits and unlock dozens of exclusive features."
        };
        ui.painter().text(
            Pos2::new(hero_rect.center().x, hero_rect.top() + 110.0),
            Align2::CENTER_CENTER,
            subtitle,
            FontId::proportional(11.8),
            Color32::from_rgba_unmultiplied(255, 255, 255, 232),
        );

        // Plan selector + Subscribe placeholder button
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(ui, "Subscription Tiers", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let toggle_lbl = if snapshot.is_premium {
                            "Simulate Free Tier"
                        } else {
                            "Simulate Premium Active"
                        };
                        if pill_button(
                            ui,
                            toggle_lbl,
                            palette.window_bg_over,
                            PREMIUM_PURPLE,
                            Some(with_alpha(PREMIUM_PURPLE, 0.4)),
                        )
                        .clicked()
                        {
                            action = Some(PremiumSettingsAction::SetPremium(!snapshot.is_premium));
                        }
                    });
                });
                ui.add_space(6.0);

                for (idx, plan) in snapshot.premium_plans.iter().enumerate() {
                    let selected = idx == self.selected_plan;
                    let w = ui.available_width().max(220.0);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 48.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let bg = if selected {
                        with_alpha(PREMIUM_PURPLE, 0.12)
                    } else if resp.hovered() {
                        palette.window_bg_over
                    } else {
                        palette.window_bg
                    };
                    let stroke_col = if selected {
                        PREMIUM_PURPLE
                    } else {
                        palette.divider
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(10),
                        Stroke::new(if selected { 1.6 } else { 1.0 }, stroke_col),
                        StrokeKind::Inside,
                    );

                    // Radio indicator
                    let dot = Pos2::new(rect.left() + 18.0, rect.center().y);
                    ui.painter().circle_stroke(
                        dot,
                        7.5,
                        Stroke::new(
                            1.5,
                            if selected {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_sub_text_fg
                            },
                        ),
                    );
                    if selected {
                        ui.painter().circle_filled(dot, 4.2, PREMIUM_PURPLE);
                    }

                    ui.painter().text(
                        Pos2::new(rect.left() + 36.0, rect.top() + 16.0),
                        Align2::LEFT_CENTER,
                        &plan.label,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    if !plan.discount_badge.is_empty() {
                        let badge = Rect::from_min_size(
                            Pos2::new(rect.left() + 106.0, rect.top() + 8.0),
                            Vec2::new(42.0, 17.0),
                        );
                        ui.painter()
                            .rect_filled(badge, CornerRadius::same(8), PREMIUM_PURPLE);
                        ui.painter().text(
                            badge.center(),
                            Align2::CENTER_CENTER,
                            &plan.discount_badge,
                            FontId::proportional(10.0),
                            Color32::WHITE,
                        );
                    }
                    let sub = if plan.original_total.is_empty() {
                        format!("{} billed monthly", plan.total_price)
                    } else {
                        format!("{} total (was {})", plan.total_price, plan.original_total)
                    };
                    ui.painter().text(
                        Pos2::new(rect.left() + 36.0, rect.top() + 34.0),
                        Align2::LEFT_CENTER,
                        sub,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 12.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{}/mo", plan.price_per_month),
                        FontId::proportional(13.0),
                        palette.window_bold_fg,
                    );
                    if resp.clicked() {
                        self.selected_plan = idx;
                    }
                    ui.add_space(6.0);
                }

                // Subscribe CTA opens `PaymentsScreen::Premium` read-only invoice preview
                // (never auto-purchasing).
                let label = self.subscribe_label(snapshot);
                let sub_resp = primary_button(ui, &label, PREMIUM_PURPLE, Color32::WHITE, 40.0);
                sub_resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone())
                });
                if sub_resp.clicked() {
                    action = Some(PremiumSettingsAction::OpenPaymentsPremium);
                }
            });

        section_divider(ui, palette);

        // Compare Tiers table (Free vs Telegram Premium)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(ui, "Compare Tiers: Free vs Premium", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Inspect Limit Boxes",
                            with_alpha(PREMIUM_INDIGO, 0.14),
                            PREMIUM_INDIGO,
                            None,
                        )
                        .clicked()
                        {
                            action =
                                Some(PremiumSettingsAction::OpenLimits(PremiumLimitKind::Folders));
                        }
                    });
                });
                ui.add_space(6.0);

                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        let w = ui.available_width().max(220.0);
                        let (hdr, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
                        ui.painter().text(
                            Pos2::new(hdr.left() + 4.0, hdr.center().y),
                            Align2::LEFT_CENTER,
                            "Capability",
                            FontId::proportional(11.5),
                            palette.window_sub_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(hdr.right() - 104.0, hdr.center().y),
                            Align2::RIGHT_CENTER,
                            "Free",
                            FontId::proportional(11.5),
                            palette.window_sub_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(hdr.right() - 4.0, hdr.center().y),
                            Align2::RIGHT_CENTER,
                            "Premium",
                            FontId::proportional(11.5),
                            PREMIUM_PURPLE,
                        );
                        thin_divider(ui, palette);

                        for row in &snapshot.tier_comparison {
                            let (r, _) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::hover());
                            ui.painter().text(
                                Pos2::new(r.left() + 4.0, r.center().y),
                                Align2::LEFT_CENTER,
                                &row.feature,
                                FontId::proportional(12.0),
                                palette.window_fg,
                            );
                            ui.painter().text(
                                Pos2::new(r.right() - 104.0, r.center().y),
                                Align2::RIGHT_CENTER,
                                &row.free_value,
                                FontId::proportional(12.0),
                                palette.window_sub_text_fg,
                            );
                            ui.painter().text(
                                Pos2::new(r.right() - 4.0, r.center().y),
                                Align2::RIGHT_CENTER,
                                &row.premium_value,
                                FontId::proportional(12.0),
                                PREMIUM_PURPLE,
                            );
                            thin_divider(ui, palette);
                        }
                    });
            });

        section_divider(ui, palette);

        // Full Premium Features List with static animation preview cards
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!(
                        "What's Included ({} Features)",
                        snapshot.premium_features.len()
                    ),
                    palette,
                );
                ui.add_space(6.0);

                for (idx, feat) in snapshot.premium_features.iter().enumerate() {
                    let expanded = self.expanded_feature == Some(idx);
                    let w = ui.available_width().max(220.0);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 54.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                    }
                    let icon_col =
                        Color32::from_rgb(feat.icon_rgb[0], feat.icon_rgb[1], feat.icon_rgb[2]);
                    let icon_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 4.0, rect.center().y - 16.0),
                        Vec2::splat(32.0),
                    );
                    ui.painter()
                        .rect_filled(icon_rect, CornerRadius::same(8), icon_col);
                    ui.painter().text(
                        icon_rect.center(),
                        Align2::CENTER_CENTER,
                        feat.icon_glyph,
                        FontId::proportional(11.5),
                        Color32::WHITE,
                    );

                    ui.painter().text(
                        Pos2::new(icon_rect.right() + 12.0, rect.top() + 18.0),
                        Align2::LEFT_CENTER,
                        &feat.title,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().text(
                        Pos2::new(icon_rect.right() + 12.0, rect.top() + 37.0),
                        Align2::LEFT_CENTER,
                        &feat.description,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );

                    if feat.is_new {
                        let badge = Rect::from_min_size(
                            Pos2::new(rect.right() - 42.0, rect.top() + 9.0),
                            Vec2::new(36.0, 16.0),
                        );
                        ui.painter()
                            .rect_filled(badge, CornerRadius::same(8), PREMIUM_PINK);
                        ui.painter().text(
                            badge.center(),
                            Align2::CENTER_CENTER,
                            "NEW",
                            FontId::proportional(9.0),
                            Color32::WHITE,
                        );
                    }

                    if resp.clicked() {
                        self.expanded_feature = if expanded { None } else { Some(idx) };
                    }

                    if expanded {
                        if let Some(sub_act) =
                            render_feature_static_preview(ui, feat, icon_col, palette)
                        {
                            action = Some(sub_act);
                        }
                        ui.add_space(4.0);
                    }

                    thin_divider(ui, palette);
                }
            });

        action
    }
}

fn render_feature_static_preview(
    ui: &mut Ui,
    feat: &PremiumFeatureEntry,
    accent: Color32,
    palette: &Palette,
) -> Option<PremiumSettingsAction> {
    let mut action = None;
    Frame::NONE
        .fill(with_alpha(accent, 0.08))
        .stroke(Stroke::new(1.0, with_alpha(accent, 0.35)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (thumb, _) = ui.allocate_exact_size(Vec2::new(56.0, 44.0), Sense::hover());
                ui.painter()
                    .rect_filled(thumb, CornerRadius::same(8), accent);
                paint_star_emblem(ui.painter(), thumb.center(), 12.0, Color32::WHITE);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&feat.preview_caption)
                            .size(11.5)
                            .color(palette.window_fg),
                    );
                    ui.add_space(4.0);
                    if feat.key == "double_limits" {
                        if pill_button(ui, "Open Limit Boxes", accent, Color32::WHITE, None)
                            .clicked()
                        {
                            action =
                                Some(PremiumSettingsAction::OpenLimits(PremiumLimitKind::Folders));
                        }
                    } else if feat.key == "emoji_status" || feat.key == "profile_badge" {
                        if pill_button(ui, "Open Emoji Status Picker", accent, Color32::WHITE, None)
                            .clicked()
                        {
                            action = Some(PremiumSettingsAction::OpenEmojiStatus);
                        }
                    } else if feat.key == "business"
                        && pill_button(ui, "Open Business Hub", accent, Color32::WHITE, None)
                            .clicked()
                    {
                        action = Some(PremiumSettingsAction::OpenBusinessHub);
                    }
                });
            });
        });
    action
}

// -----------------------------------------------------------------------------
// Premium Limit Boxes (`boxes/premium_limits_box.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for Telegram Premium limit-exceeded boxes (`boxes/premium_limits_box.cpp`).
pub struct PremiumLimitsView {
    /// Currently selected limit box variant.
    pub active_kind: PremiumLimitKind,
    /// Selected inactive channel IDs in `ChannelsLimitBox`.
    pub selected_inactive_channels: BTreeSet<i64>,
}

impl Default for PremiumLimitsView {
    fn default() -> Self {
        Self::new(PremiumLimitKind::Folders)
    }
}

impl PremiumLimitsView {
    /// Creates a [`PremiumLimitsView`] focused on `kind`.
    #[must_use]
    pub fn new(kind: PremiumLimitKind) -> Self {
        Self {
            active_kind: kind,
            selected_inactive_channels: BTreeSet::new(),
        }
    }

    /// Computes the Free vs Premium split ratio (`0.15..=0.85`) for the limit infographic bar.
    #[must_use]
    pub fn split_ratio(kind: PremiumLimitKind, limits: &PremiumLimitsData) -> f32 {
        let (def, prem, _) = kind.bounds(limits);
        if prem <= 0 {
            return 0.5;
        }
        (def as f32 / prem as f32).clamp(0.22, 0.78)
    }

    /// Renders the Premium Limits gallery & active limit-exceeded box preview.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &mut BusinessOverviewSnapshot,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<PremiumSettingsAction> {
        let mut action = None;

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    "Exceeded-Limit Boxes (boxes/premium_limits_box.cpp)",
                    palette,
                );
                ui.add_space(6.0);

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for &kind in PremiumLimitKind::ALL {
                        let active = self.active_kind == kind;
                        if pill_button(
                            ui,
                            kind.label(),
                            if active {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            },
                            if active {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            Some(if active {
                                PREMIUM_PURPLE
                            } else {
                                palette.divider
                            }),
                        )
                        .clicked()
                        {
                            self.active_kind = kind;
                        }
                    }
                });

                ui.add_space(12.0);

                // Active Limit Box Card (matches `SimpleLimitBox` in `premium_limits_box.cpp`)
                let kind = self.active_kind;
                let (def_lim, prem_lim, unit) = kind.bounds(&snapshot.premium_limits);
                let current_val = if snapshot.is_premium {
                    prem_lim
                } else {
                    def_lim
                };
                let ratio = Self::split_ratio(kind, &snapshot.premium_limits);

                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.5, with_alpha(PREMIUM_PURPLE, 0.55)))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(kind.box_title())
                                    .size(16.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "ref: double_limits__{}",
                                            kind.ref_addition()
                                        ))
                                        .size(10.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                },
                            );
                        });

                        ui.add_space(12.0);

                        // Top floating bubble pill (`Ui::Premium::AddBubbleRow`)
                        let bar_w = ui.available_width().max(220.0);
                        let (bubble_area, _) =
                            ui.allocate_exact_size(Vec2::new(bar_w, 34.0), Sense::hover());
                        let bubble_x = if snapshot.is_premium {
                            bubble_area.right() - 56.0
                        } else {
                            (bubble_area.left() + bar_w * ratio)
                                .clamp(bubble_area.left() + 56.0, bubble_area.right() - 56.0)
                        };
                        let bubble_rect = Rect::from_center_size(
                            Pos2::new(bubble_x, bubble_area.center().y - 2.0),
                            Vec2::new(98.0, 28.0),
                        );
                        ui.painter().rect_filled(
                            bubble_rect,
                            CornerRadius::same(14),
                            if snapshot.is_premium {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_active
                            },
                        );
                        ui.painter().text(
                            bubble_rect.center(),
                            Align2::CENTER_CENTER,
                            format!("{current_val}{unit}"),
                            FontId::proportional(13.0),
                            Color32::WHITE,
                        );

                        // Split Free | Premium progress bar (`Ui::Premium::AddLimitRow`)
                        let (bar_rect, _) =
                            ui.allocate_exact_size(Vec2::new(bar_w, 32.0), Sense::hover());
                        let split_x = bar_rect.left() + bar_w * ratio;
                        let left_rect =
                            Rect::from_min_max(bar_rect.min, Pos2::new(split_x, bar_rect.bottom()));
                        let right_rect =
                            Rect::from_min_max(Pos2::new(split_x, bar_rect.top()), bar_rect.max);
                        ui.painter().rect_filled(
                            left_rect,
                            CornerRadius {
                                nw: 8,
                                sw: 8,
                                ne: 0,
                                se: 0,
                            },
                            palette.window_bg_over,
                        );
                        ui.painter().rect_filled(
                            right_rect,
                            CornerRadius {
                                nw: 0,
                                sw: 0,
                                ne: 8,
                                se: 8,
                            },
                            PREMIUM_PURPLE,
                        );
                        ui.painter().text(
                            Pos2::new(left_rect.left() + 10.0, left_rect.center().y),
                            Align2::LEFT_CENTER,
                            "Free",
                            FontId::proportional(12.0),
                            palette.window_sub_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(left_rect.right() - 8.0, left_rect.center().y),
                            Align2::RIGHT_CENTER,
                            format!("{def_lim}{unit}"),
                            FontId::proportional(12.0),
                            palette.window_bold_fg,
                        );
                        ui.painter().text(
                            Pos2::new(right_rect.left() + 10.0, right_rect.center().y),
                            Align2::LEFT_CENTER,
                            "Premium",
                            FontId::proportional(12.0),
                            Color32::WHITE,
                        );
                        ui.painter().text(
                            Pos2::new(right_rect.right() - 10.0, right_rect.center().y),
                            Align2::RIGHT_CENTER,
                            format!("{prem_lim}{unit}"),
                            FontId::proportional(12.5),
                            Color32::WHITE,
                        );

                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(
                                kind.description(&snapshot.premium_limits, snapshot.is_premium),
                            )
                            .size(12.5)
                            .color(palette.window_fg),
                        );

                        // ChannelsLimitBox extra: inactive channels multi-select leave list
                        if kind == PremiumLimitKind::Channels {
                            ui.add_space(10.0);
                            thin_divider(ui, palette);
                            ui.add_space(8.0);
                            subsection_title(ui, "Inactive Channels to Leave", palette);
                            ui.add_space(4.0);
                            if snapshot.inactive_channels.is_empty() {
                                ui.label(
                                    egui::RichText::new("No inactive channels remaining.")
                                        .size(12.0)
                                        .color(SUCCESS_GREEN),
                                );
                            } else {
                                for ch in &snapshot.inactive_channels {
                                    let mut checked =
                                        self.selected_inactive_channels.contains(&ch.id);
                                    ui.horizontal(|ui| {
                                        if ui.checkbox(&mut checked, "").changed() {
                                            if checked {
                                                self.selected_inactive_channels.insert(ch.id);
                                            } else {
                                                self.selected_inactive_channels.remove(&ch.id);
                                            }
                                        }
                                        let (av, _) = ui
                                            .allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                                        paint_avatar(
                                            ui.painter(),
                                            av.center(),
                                            14.0,
                                            ch.avatar_rgb,
                                            if ch.is_broadcast { "CH" } else { "GR" },
                                        );
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(&ch.title)
                                                    .size(12.5)
                                                    .strong()
                                                    .color(palette.window_bold_fg),
                                            );
                                            ui.label(
                                                egui::RichText::new(&ch.inactive_since)
                                                    .size(11.0)
                                                    .color(palette.window_sub_text_fg),
                                            );
                                        });
                                    });
                                    ui.add_space(3.0);
                                }
                            }
                        }

                        // PublicLinksLimitBox extra: admined public channels revoke list
                        if kind == PremiumLimitKind::PublicLinks {
                            ui.add_space(10.0);
                            thin_divider(ui, palette);
                            ui.add_space(8.0);
                            subsection_title(ui, "Your Public Channels & Groups", palette);
                            ui.add_space(4.0);
                            let mut revoke_id = None;
                            if snapshot.public_link_channels.is_empty() {
                                ui.label(
                                    egui::RichText::new("All sample public links revoked.")
                                        .size(12.0)
                                        .color(SUCCESS_GREEN),
                                );
                            } else {
                                for ch in &snapshot.public_link_channels {
                                    ui.horizontal(|ui| {
                                        let (av, _) = ui
                                            .allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                                        paint_avatar(
                                            ui.painter(),
                                            av.center(),
                                            14.0,
                                            ch.avatar_rgb,
                                            "@",
                                        );
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(&ch.title)
                                                    .size(12.5)
                                                    .strong()
                                                    .color(palette.window_bold_fg),
                                            );
                                            ui.label(
                                                egui::RichText::new(&ch.public_link)
                                                    .size(11.0)
                                                    .color(palette.window_active_text_fg),
                                            );
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if pill_button(
                                                    ui,
                                                    "Revoke",
                                                    with_alpha(palette.attention_fg, 0.14),
                                                    palette.attention_fg,
                                                    None,
                                                )
                                                .clicked()
                                                {
                                                    revoke_id = Some(ch.id);
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(3.0);
                                }
                            }
                            if let Some(cid) = revoke_id
                                && backend.revoke_public_link(cid).is_ok()
                            {
                                snapshot.public_link_channels.retain(|c| c.id != cid);
                                action = Some(PremiumSettingsAction::Toast(
                                    "Public link revoked.".to_owned(),
                                ));
                            }
                        }

                        ui.add_space(12.0);
                        if kind == PremiumLimitKind::Channels
                            && !self.selected_inactive_channels.is_empty()
                        {
                            let n = self.selected_inactive_channels.len();
                            if primary_button(
                                ui,
                                &format!("Leave {n} Inactive Channel(s)"),
                                palette.attention_fg,
                                Color32::WHITE,
                                36.0,
                            )
                            .clicked()
                            {
                                let ids: Vec<i64> =
                                    self.selected_inactive_channels.iter().copied().collect();
                                if let Ok(left) = backend.leave_inactive_channels(&ids) {
                                    snapshot
                                        .inactive_channels
                                        .retain(|ch| !ids.contains(&ch.id));
                                    self.selected_inactive_channels.clear();
                                    action = Some(PremiumSettingsAction::Toast(format!(
                                        "Left {left} inactive channel(s)."
                                    )));
                                }
                            }
                        } else if !snapshot.is_premium {
                            if primary_button(
                                ui,
                                "Double Limits with Telegram Premium",
                                PREMIUM_PURPLE,
                                Color32::WHITE,
                                36.0,
                            )
                            .clicked()
                            {
                                action = Some(PremiumSettingsAction::OpenPaymentsPremium);
                            }
                        } else if primary_button(
                            ui,
                            "OK (Premium Limit Active)",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            36.0,
                        )
                        .clicked()
                        {
                            action = Some(PremiumSettingsAction::Toast(format!(
                                "{} limit acknowledged.",
                                kind.label()
                            )));
                        }
                    });
            });

        action
    }
}

// -----------------------------------------------------------------------------
// Premium Badge & Emoji Status Picker (`data_emoji_statuses.h`,
// `info_profile_emoji_status_panel.h`, `menu_emoji_status.cpp`)
// -----------------------------------------------------------------------------

/// Interactive view-model for the Premium Badge & Emoji Status picker.
pub struct EmojiStatusView {
    /// Selected category tab (`Recent`, `Default`, `Colored`, `Collectibles`).
    pub category: EmojiStatusCategory,
    /// Selected expiration duration preset.
    pub duration: EmojiStatusDuration,
    /// Search filter string.
    pub search_query: String,
    /// Previous status saved for one-click Undo (matching `ShowDoneToast` in `menu_emoji_status.cpp`).
    pub previous_status: Option<ActiveEmojiStatus>,
}

impl Default for EmojiStatusView {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiStatusView {
    /// Creates an [`EmojiStatusView`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            category: EmojiStatusCategory::Recent,
            duration: EmojiStatusDuration::Forever,
            search_query: String::new(),
            previous_status: None,
        }
    }

    /// Filters emoji statuses by active category (or all categories when searching).
    #[must_use]
    pub fn visible_items<'a>(&self, all: &'a [EmojiStatusItem]) -> Vec<&'a EmojiStatusItem> {
        let q = self.search_query.trim().to_lowercase();
        all.iter()
            .filter(|item| {
                if !q.is_empty() {
                    item.name.to_lowercase().contains(&q) || item.glyph.to_lowercase().contains(&q)
                } else {
                    item.category == self.category
                }
            })
            .collect()
    }

    /// Renders the Premium Badge & Emoji Status picker screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &mut BusinessOverviewSnapshot,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        // Live Profile Badge Preview header
        Frame::NONE
            .fill(palette.window_bg_over)
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (av, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                    paint_avatar(ui.painter(), av.center(), 24.0, [0x2A, 0x9D, 0xF4], "NS");
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&snapshot.account_name)
                                    .size(16.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            // Active Emoji Status or default Premium star badge
                            let (badge_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                            if let Some(active_id) = snapshot.emoji_status.status_id
                                && let Some(item) =
                                    snapshot.emoji_statuses.iter().find(|i| i.id == active_id)
                            {
                                paint_emoji_status_glyph(ui.painter(), badge_rect, item);
                            } else {
                                paint_star_emblem(
                                    ui.painter(),
                                    badge_rect.center(),
                                    10.0,
                                    PREMIUM_PURPLE,
                                );
                            }
                        });
                        let status_desc = match snapshot.emoji_status.status_id {
                            Some(id) => {
                                let name = snapshot
                                    .emoji_statuses
                                    .iter()
                                    .find(|i| i.id == id)
                                    .map_or("Custom Status", |i| i.name.as_str());
                                format!(
                                    "Active status: {} ({})",
                                    name,
                                    snapshot.emoji_status.duration.label()
                                )
                            }
                            None => "Default Telegram Premium Star Badge".to_owned(),
                        };
                        ui.label(
                            egui::RichText::new(status_desc)
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if snapshot.emoji_status.status_id.is_some()
                            && pill_button(
                                ui,
                                "Reset Badge",
                                palette.window_bg,
                                palette.attention_fg,
                                Some(with_alpha(palette.attention_fg, 0.4)),
                            )
                            .clicked()
                        {
                            self.previous_status = Some(snapshot.emoji_status.clone());
                            let next = ActiveEmojiStatus {
                                status_id: None,
                                duration: EmojiStatusDuration::Forever,
                            };
                            if backend.set_emoji_status(next.clone()).is_ok() {
                                snapshot.emoji_status = next;
                                toast =
                                    Some("Emoji status removed. Default star restored.".to_owned());
                            }
                        }
                        if let Some(prev) = self.previous_status.clone()
                            && pill_button(ui, "Undo", SUCCESS_SURFACE, SUCCESS_GREEN, None)
                                .clicked()
                            && backend.set_emoji_status(prev.clone()).is_ok()
                        {
                            snapshot.emoji_status = prev;
                            self.previous_status = None;
                            toast = Some("Previous emoji status restored.".to_owned());
                        }
                    });
                });
            });

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                // Duration selector (`Set Emoji Status For...`)
                subsection_title(ui, "Status Duration", palette);
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for &dur in EmojiStatusDuration::ALL {
                        let active = self.duration == dur;
                        if pill_button(
                            ui,
                            dur.label(),
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
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.duration = dur;
                        }
                    }
                });

                ui.add_space(10.0);
                thin_divider(ui, palette);
                ui.add_space(8.0);

                // Category tabs & search
                ui.horizontal(|ui| {
                    for &cat in EmojiStatusCategory::ALL {
                        let active = self.category == cat && self.search_query.trim().is_empty();
                        if pill_button(
                            ui,
                            cat.label(),
                            if active {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            },
                            if active {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.category = cat;
                            self.search_query.clear();
                        }
                    }
                });
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Search emoji statuses...")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(10.0);

                // Status grid
                let items: Vec<EmojiStatusItem> = self
                    .visible_items(&snapshot.emoji_statuses)
                    .into_iter()
                    .cloned()
                    .collect();

                if items.is_empty() {
                    ui.label(
                        egui::RichText::new("No emoji statuses match your filter.")
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                } else {
                    let cols = 3usize;
                    let gap = 8.0;
                    let avail = ui.available_width().max(240.0);
                    let card_w = ((avail - gap * (cols - 1) as f32) / cols as f32).floor();
                    for chunk in items.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for item in chunk {
                                let selected = snapshot.emoji_status.status_id == Some(item.id);
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(card_w, 76.0),
                                    Sense::click(),
                                );
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                }
                                let bg = if selected {
                                    with_alpha(PREMIUM_PURPLE, 0.14)
                                } else if resp.hovered() {
                                    palette.window_bg_over
                                } else {
                                    palette.window_bg
                                };
                                ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
                                ui.painter().rect_stroke(
                                    rect,
                                    CornerRadius::same(10),
                                    Stroke::new(
                                        if selected { 1.6 } else { 1.0 },
                                        if selected {
                                            PREMIUM_PURPLE
                                        } else {
                                            palette.divider
                                        },
                                    ),
                                    StrokeKind::Inside,
                                );

                                let glyph_rect = Rect::from_center_size(
                                    Pos2::new(rect.center().x, rect.top() + 26.0),
                                    Vec2::splat(30.0),
                                );
                                paint_emoji_status_glyph(ui.painter(), glyph_rect, item);

                                ui.painter().text(
                                    Pos2::new(rect.center().x, rect.bottom() - 22.0),
                                    Align2::CENTER_CENTER,
                                    &item.name,
                                    FontId::proportional(11.5),
                                    palette.window_bold_fg,
                                );
                                if let Some(ref slug) = item.collectible_slug {
                                    ui.painter().text(
                                        Pos2::new(rect.center().x, rect.bottom() - 9.0),
                                        Align2::CENTER_CENTER,
                                        slug,
                                        FontId::proportional(9.0),
                                        BUSINESS_GOLD,
                                    );
                                } else {
                                    ui.painter().text(
                                        Pos2::new(rect.center().x, rect.bottom() - 9.0),
                                        Align2::CENTER_CENTER,
                                        item.category.label(),
                                        FontId::proportional(9.5),
                                        palette.window_sub_text_fg,
                                    );
                                }

                                if resp.clicked() {
                                    if !snapshot.is_premium {
                                        toast = Some(
                                            "Emoji Status requires Telegram Premium (toggle Premium to preview)."
                                                .to_owned(),
                                        );
                                    } else {
                                        self.previous_status = Some(snapshot.emoji_status.clone());
                                        let next = ActiveEmojiStatus {
                                            status_id: Some(item.id),
                                            duration: self.duration,
                                        };
                                        if backend.set_emoji_status(next.clone()).is_ok() {
                                            snapshot.emoji_status = next;
                                            toast = Some(format!(
                                                "Emoji status set to \"{}\" ({}).",
                                                item.name,
                                                self.duration.label()
                                            ));
                                        }
                                    }
                                }
                            }
                        });
                        ui.add_space(gap);
                    }
                }
            });

        toast
    }
}

fn paint_emoji_status_glyph(painter: &egui::Painter, rect: Rect, item: &EmojiStatusItem) {
    let col = Color32::from_rgb(item.color_rgb[0], item.color_rgb[1], item.color_rgb[2]);
    let accent = Color32::from_rgb(item.accent_rgb[0], item.accent_rgb[1], item.accent_rgb[2]);
    if item.is_collectible {
        painter.rect_filled(rect, CornerRadius::same(8), col);
        painter.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.5, accent),
            StrokeKind::Inside,
        );
    } else {
        painter.circle_filled(rect.center(), rect.width() * 0.48, with_alpha(col, 0.2));
        painter.circle_filled(rect.center(), rect.width() * 0.40, col);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        &item.glyph,
        FontId::proportional(9.5),
        Color32::WHITE,
    );
}
