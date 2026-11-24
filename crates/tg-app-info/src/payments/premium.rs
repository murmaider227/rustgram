//! Telegram Premium promo screen and Channel Boosts / Giveaway info boxes.
//!
//! Port origin: `settings/sections/settings_premium.cpp`, `ui/boxes/boost_box.cpp`,
//! and `boxes/gift_premium_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    Ui, Vec2,
};
use tg_lang::tr;
use tg_payments::premium::{BoostStatus, GiveawayState, LevelLimits, PremiumSubscriptionOption};

use crate::payments::backend::{
    BoostsGiveawayBundle, GiveawayInfoItem, PaymentsBackend, PremiumInvoicePreview,
    PremiumPromoBundle,
};
use crate::payments::giveaways_ui::{CreateGiveawayPanel, ReplaceBoostPanel};
use crate::payments::marketplace_backend::StarGiftsMarketplaceBackend;
use crate::payments::ui_helpers::{
    PREMIUM_INDIGO, PREMIUM_PINK, PREMIUM_PURPLE, STAR_GOLD, SUCCESS_GREEN, SUCCESS_SURFACE,
    format_unix_date, paint_star_emblem, pill_button, primary_button, reveal_progress,
    section_divider, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Telegram Premium promo view-model & renderer (`settings/sections/settings_premium.cpp`).
pub struct PremiumView {
    /// Promo bundle with subscription tiers, limits, and features.
    pub bundle: PremiumPromoBundle,
    /// Selected subscription tier index into `bundle.promo.period_options`.
    pub selected_tier: usize,
    /// Currently expanded feature index (if user clicked a feature row to inspect limits).
    pub expanded_feature: Option<usize>,
    /// Read-only payment form preview loaded via `payments.getPaymentForm` when Subscribe is pressed.
    pub invoice_preview: Option<PremiumInvoicePreview>,
    /// Optional error returned when loading the read-only invoice preview.
    pub invoice_error: Option<String>,
}

impl PremiumView {
    /// Creates a [`PremiumView`] from `bundle`.
    #[must_use]
    pub fn new(bundle: PremiumPromoBundle) -> Self {
        Self {
            bundle,
            selected_tier: 0,
            expanded_feature: Some(0),
            invoice_preview: None,
            invoice_error: None,
        }
    }

    /// Returns the currently selected [`PremiumSubscriptionOption`], if any.
    #[must_use]
    pub fn selected_option(&self) -> Option<&PremiumSubscriptionOption> {
        self.bundle.promo.period_options.get(self.selected_tier)
    }

    /// Builds the subscribe button label matching `settings_premium.cpp`.
    #[must_use]
    pub fn subscribe_button_label(&self) -> String {
        match self.selected_option() {
            Some(opt) => tr::lng_premium_subscribe_button(&opt.cost_per_month),
            None => "Subscribe to Telegram Premium".to_owned(),
        }
    }

    /// Renders the Telegram Premium promo screen without a live backend.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette) -> Option<String> {
        self.show_inner(ui, None, palette)
    }

    /// Renders the Telegram Premium promo screen backed by `backend` for read-only
    /// `payments.getPaymentForm` invoice previews (never auto-purchasing).
    pub fn show_with_backend(
        &mut self,
        ui: &mut Ui,
        backend: &dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        self.show_inner(ui, Some(backend), palette)
    }

    fn show_inner(
        &mut self,
        ui: &mut Ui,
        backend: Option<&dyn PaymentsBackend>,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        // Gradient hero header
        let header_w = ui.available_width().max(260.0);
        let (hero_rect, _) = ui.allocate_exact_size(Vec2::new(header_w, 134.0), Sense::hover());
        paint_premium_gradient(ui.painter(), hero_rect);
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
        let status_galley = ui.fonts_mut(|f| {
            f.layout(
                self.bundle.promo.status_text.clone(),
                FontId::proportional(12.0),
                Color32::from_rgba_unmultiplied(255, 255, 255, 225),
                (hero_rect.width() - 32.0).max(160.0),
            )
        });
        ui.painter().galley(
            Pos2::new(
                hero_rect.center().x - status_galley.size().x / 2.0,
                hero_rect.top() + 101.0,
            ),
            status_galley,
            Color32::from_rgba_unmultiplied(255, 255, 255, 225),
        );

        let margin_h = if ui.available_width() < 420.0 { 12 } else { 18 };
        Frame::NONE
            .inner_margin(Margin::symmetric(margin_h, 12))
            .show(ui, |ui| {
                // Subscription period tier selector
                subsection_title(ui, "Choose a Plan", palette);
                ui.add_space(6.0);

                let tiers = self.bundle.promo.period_options.clone();
                for (idx, opt) in tiers.iter().enumerate() {
                    let selected = idx == self.selected_tier;
                    let width = ui.available_width().max(200.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::click());
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
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(10), bg);
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(10),
                        Stroke::new(if selected { 1.5 } else { 1.0 }, stroke_col),
                        egui::StrokeKind::Inside,
                    );

                    // Radio dot
                    let dot_center = Pos2::new(rect.left() + 20.0, rect.center().y);
                    ui.painter().circle_stroke(
                        dot_center,
                        8.0,
                        Stroke::new(1.5, if selected { PREMIUM_PURPLE } else { palette.window_sub_text_fg }),
                    );
                    if selected {
                        ui.painter().circle_filled(dot_center, 4.5, PREMIUM_PURPLE);
                    }

                    let title = match opt.months {
                        12 => "Annual",
                        6 => "6 Months",
                        1 => "Monthly",
                        m => {
                            let _ = m;
                            "Subscription"
                        }
                    };
                    ui.painter().text(
                        Pos2::new(rect.left() + 38.0, rect.top() + 16.0),
                        Align2::LEFT_CENTER,
                        title,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    if !opt.discount.is_empty() {
                        let badge = Rect::from_min_size(
                            Pos2::new(rect.left() + 104.0, rect.top() + 8.0),
                            Vec2::new(42.0, 17.0),
                        );
                        ui.painter()
                            .rect_filled(badge, CornerRadius::same(8), PREMIUM_PURPLE);
                        ui.painter().text(
                            badge.center(),
                            Align2::CENTER_CENTER,
                            &opt.discount,
                            FontId::proportional(10.0),
                            Color32::WHITE,
                        );
                    }
                    let sub_text = if opt.cost_no_discount.is_empty() {
                        format!("{} billed monthly", opt.total)
                    } else {
                        format!("{} total (was {})", opt.total, opt.cost_no_discount)
                    };
                    ui.painter().text(
                        Pos2::new(rect.left() + 38.0, rect.top() + 34.0),
                        Align2::LEFT_CENTER,
                        sub_text,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 12.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{}/mo", opt.cost_per_month),
                        FontId::proportional(13.0),
                        palette.window_bold_fg,
                    );
                    if resp.clicked() {
                        self.selected_tier = idx;
                    }
                    ui.add_space(6.0);
                }

                ui.add_space(4.0);

                // Subscribe button loads a read-only `payments.getPaymentForm` invoice preview
                // (never auto-purchasing).
                let btn_label = self.subscribe_button_label();
                if primary_button(ui, &btn_label, PREMIUM_PURPLE, Color32::WHITE, 40.0).clicked()
                    && let Some(opt) = self.selected_option().cloned()
                {
                    if let Some(be) = backend {
                        match be.preview_premium_invoice(&opt) {
                            Ok(preview) => {
                                toast = Some(format!(
                                    "Loaded read-only invoice preview #{} ({}).",
                                    preview.form_id, preview.total_label
                                ));
                                self.invoice_error = None;
                                self.invoice_preview = Some(preview);
                            }
                            Err(err) => {
                                self.invoice_preview = None;
                                self.invoice_error = Some(err.clone());
                                toast = Some(err);
                            }
                        }
                    } else {
                        let preview = PremiumInvoicePreview {
                            form_id: 0,
                            title: format!("Telegram Premium ({} months)", opt.months),
                            description: format!(
                                "{} total ({}/month). Connect a live session to request a payment form.",
                                opt.total, opt.cost_per_month
                            ),
                            total_label: opt.total.clone(),
                            months: opt.months,
                            provider: "Telegram Payments".to_owned(),
                            checkout_url: opt.bot_url.clone(),
                            is_test: true,
                        };
                        self.invoice_error = None;
                        self.invoice_preview = Some(preview);
                    }
                }

                if let Some(ref err) = self.invoice_error.clone() {
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(with_alpha(palette.attention_fg, 0.08))
                        .stroke(Stroke::new(1.0, palette.attention_fg))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(err)
                                        .size(12.0)
                                        .color(palette.attention_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(6.0);
                            if pill_button(
                                ui,
                                "Dismiss",
                                palette.window_bg,
                                palette.attention_fg,
                                Some(palette.attention_fg),
                            )
                            .clicked()
                            {
                                self.invoice_error = None;
                            }
                        });
                }

                if let Some(ref preview) = self.invoice_preview.clone() {
                    let reveal =
                        reveal_progress(ui, ui.make_persistent_id("premium_invoice_preview"), true);
                    ui.add_space(8.0 + (1.0 - reveal) * 6.0);
                    Frame::NONE
                        .fill(with_alpha(PREMIUM_PURPLE, 0.08))
                        .stroke(Stroke::new(1.2, with_alpha(PREMIUM_PURPLE, reveal.max(0.3))))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(&preview.title)
                                        .size(14.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.label(
                                    egui::RichText::new(format!("•  {}", preview.total_label))
                                        .size(13.0)
                                        .strong()
                                        .color(PREMIUM_PURPLE),
                                );
                            });
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&preview.description)
                                        .size(12.0)
                                        .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "Provider: {}  •  Form #{}{}",
                                        preview.provider,
                                        preview.form_id,
                                        if preview.is_test { " (Test)" } else { "" }
                                    ))
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                                )
                                .wrap(),
                            );
                            if !preview.checkout_url.is_empty() {
                                ui.add_space(2.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&preview.checkout_url)
                                            .size(11.0)
                                            .monospace()
                                            .color(palette.window_sub_text_fg),
                                    )
                                    .wrap(),
                                );
                            }
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                if pill_button(
                                    ui,
                                    "Close Preview",
                                    palette.window_bg,
                                    PREMIUM_PURPLE,
                                    Some(PREMIUM_PURPLE),
                                )
                                .clicked()
                                {
                                    self.invoice_preview = None;
                                }
                            });
                        });
                }
            });

        section_divider(ui, palette);

        // Feature list with colorful icons (`settings_premium.cpp`)
        Frame::NONE
            .inner_margin(Margin::symmetric(margin_h, 6))
            .show(ui, |ui| {
                subsection_title(ui, "What's Included", palette);
                ui.add_space(6.0);

                let features = self.bundle.features.clone();
                for (idx, feat) in features.iter().enumerate() {
                    let expanded = self.expanded_feature == Some(idx);
                    let width = ui.available_width().max(200.0);
                    let text_max_w =
                        (width - 56.0 - if feat.is_new { 46.0 } else { 8.0 }).max(100.0);
                    let desc_galley = ui.fonts_mut(|f| {
                        f.layout(
                            feat.description.clone(),
                            FontId::proportional(11.5),
                            palette.window_sub_text_fg,
                            text_max_w,
                        )
                    });
                    let row_h = (32.0 + desc_galley.size().y + 10.0).max(52.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Button, true, feat.title.clone())
                    });
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
                        Pos2::new(rect.left() + 4.0, rect.top() + 10.0),
                        Vec2::splat(32.0),
                    );
                    ui.painter()
                        .rect_filled(icon_rect, CornerRadius::same(8), icon_col);
                    ui.painter().text(
                        icon_rect.center(),
                        Align2::CENTER_CENTER,
                        &feat.icon_symbol,
                        FontId::proportional(12.5),
                        Color32::WHITE,
                    );

                    ui.painter().text(
                        Pos2::new(icon_rect.right() + 12.0, rect.top() + 17.0),
                        Align2::LEFT_CENTER,
                        &feat.title,
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().galley(
                        Pos2::new(icon_rect.right() + 12.0, rect.top() + 29.0),
                        desc_galley,
                        palette.window_sub_text_fg,
                    );

                    if feat.is_new {
                        let new_rect = Rect::from_min_size(
                            Pos2::new(rect.right() - 42.0, rect.top() + 9.0),
                            Vec2::new(36.0, 16.0),
                        );
                        ui.painter()
                            .rect_filled(new_rect, CornerRadius::same(8), PREMIUM_PINK);
                        ui.painter().text(
                            new_rect.center(),
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
                        Frame::NONE
                            .fill(palette.window_bg_over)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&feat.detail)
                                        .size(11.5)
                                        .color(palette.window_fg),
                                );
                            });
                        ui.add_space(4.0);
                    }
                    thin_divider(ui, palette);
                }
            });

        toast
    }
}

/// Boosts & Giveaway info boxes view-model & renderer (`ui/boxes/boost_box.cpp`).
pub struct BoostsGiveawaysView {
    /// Loaded channel boost status, slots, and sample giveaways.
    pub bundle: BoostsGiveawayBundle,
    /// Currently inspected giveaway index (shows detailed giveaway info box).
    pub selected_giveaway: Option<usize>,
    /// Create Giveaway / Launch Prepaid Giveaway panel (`create_giveaway_box.cpp`).
    pub create_giveaway: CreateGiveawayPanel,
    /// Replace / Reassign Boost slots panel (`replace_boost_box.cpp`).
    pub replace_boost: ReplaceBoostPanel,
    /// Whether the Create Giveaway form is expanded.
    pub show_create_giveaway: bool,
    /// Whether the Replace Boost panel is expanded.
    pub show_replace_boost: bool,
    /// Pending free boost slot awaiting user confirmation before calling `premium.applyBoost`.
    pub pending_boost_slot: Option<i32>,
    /// Optional inline error if `premium.applyBoost` failed.
    pub boost_error: Option<String>,
}

impl BoostsGiveawaysView {
    /// Creates a [`BoostsGiveawaysView`] from `bundle`.
    #[must_use]
    pub fn new(bundle: BoostsGiveawayBundle) -> Self {
        Self {
            bundle,
            selected_giveaway: Some(0),
            create_giveaway: CreateGiveawayPanel::default(),
            replace_boost: ReplaceBoostPanel::default(),
            show_create_giveaway: false,
            show_replace_boost: false,
            pending_boost_slot: None,
            boost_error: None,
        }
    }

    /// Computes normalized progress (`0.0..=1.0`) toward the next boost level.
    #[must_use]
    pub fn boost_level_progress(status: &BoostStatus) -> f32 {
        let ov = &status.overview;
        if ov.next_level_boost_count <= ov.current_level_boost_count {
            return 1.0;
        }
        let span = (ov.next_level_boost_count - ov.current_level_boost_count) as f32;
        let done = (ov.boost_count - ov.current_level_boost_count).max(0) as f32;
        (done / span).clamp(0.0, 1.0)
    }

    /// Returns channel perk milestones unlocked at or before `level`.
    #[must_use]
    pub fn unlocked_perks(level: i32, limits: &LevelLimits) -> Vec<(&'static str, i32, bool)> {
        vec![
            (
                "Auto-Translation for Channel",
                limits.channel_auto_translate,
                level >= limits.channel_auto_translate,
            ),
            (
                "Custom Background Emoji Icon",
                limits.channel_bg_icon,
                level >= limits.channel_bg_icon,
            ),
            (
                "Profile Cover Emoji Icon",
                limits.channel_profile_bg_icon,
                level >= limits.channel_profile_bg_icon,
            ),
            (
                "Channel Emoji Status",
                limits.channel_emoji_status,
                level >= limits.channel_emoji_status,
            ),
            (
                "Channel Wallpapers",
                limits.channel_wallpaper,
                level >= limits.channel_wallpaper,
            ),
            (
                "Disable Sponsored Messages",
                limits.channel_restrict_sponsored,
                level >= limits.channel_restrict_sponsored,
            ),
        ]
    }

    /// Renders Boosts info box, boost slots, and Giveaway info boxes.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette) -> Option<String> {
        self.show_with_backend(ui, None, 1_763_760_000, palette)
    }

    /// Renders Boosts info box, live `premium.applyBoost` confirmation, Replace Boost panel,
    /// Create Giveaway box, and Giveaway info boxes using a [`PaymentsBackend`].
    pub fn show_with_payments_backend(
        &mut self,
        ui: &mut Ui,
        payments: &mut dyn PaymentsBackend,
        now: i32,
        palette: &Palette,
    ) -> Option<String> {
        self.show_inner(ui, Some(payments), None, now, palette)
    }

    /// Renders Boosts info box, Replace Boost panel, Create Giveaway box, and Giveaway info boxes.
    pub fn show_with_backend(
        &mut self,
        ui: &mut Ui,
        marketplace: Option<&mut dyn StarGiftsMarketplaceBackend>,
        now: i32,
        palette: &Palette,
    ) -> Option<String> {
        self.show_inner(ui, None, marketplace, now, palette)
    }

    fn show_inner<'a>(
        &mut self,
        ui: &mut Ui,
        mut payments: Option<&'a mut dyn PaymentsBackend>,
        marketplace: Option<&'a mut dyn StarGiftsMarketplaceBackend>,
        now: i32,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let ov = self.bundle.boost_status.overview.clone();
        let target_progress = Self::boost_level_progress(&self.bundle.boost_status);
        let progress = if ui.style().animation_time <= 0.0 {
            target_progress
        } else {
            ui.ctx().animate_value_with_time(
                ui.make_persistent_id("boost_level_progress"),
                target_progress,
                0.22,
            )
        };
        let margin_h = if ui.available_width() < 420.0 { 12 } else { 18 };

        // Channel Boosts Info Box
        Frame::NONE
            .inner_margin(Margin::symmetric(margin_h, 14))
            .show(ui, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Boost {}",
                                    self.bundle.channel_title
                                ))
                                .size(15.5)
                                .strong()
                                .color(palette.window_bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (badge, _) = ui.allocate_exact_size(
                                        Vec2::new(68.0, 22.0),
                                        Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        badge,
                                        CornerRadius::same(11),
                                        PREMIUM_PURPLE,
                                    );
                                    ui.painter().text(
                                        badge.center(),
                                        Align2::CENTER_CENTER,
                                        format!("Level {}", ov.level),
                                        FontId::proportional(11.0),
                                        Color32::WHITE,
                                    );
                                },
                            );
                        });
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / {} boosts  •  {} more needed for Level {}  •  You contributed {} boost(s)",
                                ov.boost_count,
                                ov.next_level_boost_count,
                                (ov.next_level_boost_count - ov.boost_count).max(0),
                                ov.level + 1,
                                ov.mine
                            ))
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                        );

                        ui.add_space(8.0);
                        let bar_w = ui.available_width().max(180.0);
                        let (bar_rect, _) =
                            ui.allocate_exact_size(Vec2::new(bar_w, 12.0), Sense::hover());
                        ui.painter().rect_filled(
                            bar_rect,
                            CornerRadius::same(6),
                            palette.window_bg_over,
                        );
                        let fill_rect = Rect::from_min_size(
                            bar_rect.min,
                            Vec2::new((bar_w * progress).max(12.0), 12.0),
                        );
                        ui.painter()
                            .rect_filled(fill_rect, CornerRadius::same(6), PREMIUM_PURPLE);

                        ui.add_space(10.0);
                        subsection_title(ui, "Channel Level Perks", palette);
                        ui.add_space(4.0);
                        for (perk, min_lv, unlocked) in
                            Self::unlocked_perks(ov.level, &self.bundle.level_limits)
                        {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(if unlocked { "[OK]" } else { "[--]" })
                                        .size(11.5)
                                        .strong()
                                        .color(if unlocked {
                                            SUCCESS_GREEN
                                        } else {
                                            palette.window_sub_text_fg
                                        }),
                                );
                                ui.label(
                                    egui::RichText::new(format!("Lv.{min_lv}  {perk}"))
                                        .size(12.0)
                                        .color(if unlocked {
                                            palette.window_fg
                                        } else {
                                            palette.window_sub_text_fg
                                        }),
                                );
                            });
                        }

                        ui.add_space(10.0);
                        let free_slot_ids: Vec<i32> = self
                            .bundle
                            .my_slots
                            .iter()
                            .filter(|s| s.peer_id.is_none())
                            .map(|s| s.slot)
                            .collect();
                        let free_slots = free_slot_ids.len();
                        let boost_btn_label = if free_slots == 1 {
                            format!("{} (1 free slot)", tr::lng_boost_channel_button())
                        } else if free_slots > 1 {
                            format!("{} ({free_slots} free slots)", tr::lng_boost_channel_button())
                        } else {
                            tr::lng_boost_channel_button()
                        };
                        if primary_button(
                            ui,
                            &boost_btn_label,
                            PREMIUM_PURPLE,
                            Color32::WHITE,
                            36.0,
                        )
                        .clicked()
                        {
                            self.boost_error = None;
                            if let Some(&slot_id) = free_slot_ids.first() {
                                self.pending_boost_slot = Some(slot_id);
                            } else {
                                self.show_replace_boost = true;
                                toast = Some(
                                    "No free boost slots available. Select an existing slot below to reassign."
                                        .to_owned(),
                                );
                            }
                        }

                        if let Some(slot_id) = self.pending_boost_slot {
                            let reveal = reveal_progress(
                                ui,
                                ui.make_persistent_id("boost_confirm_reveal"),
                                true,
                            );
                            ui.add_space(8.0 + (1.0 - reveal) * 5.0);
                            let channel_title = self.bundle.channel_title.clone();
                            Frame::NONE
                                .fill(with_alpha(PREMIUM_PURPLE, 0.08))
                                .stroke(Stroke::new(1.2, PREMIUM_PURPLE))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "Apply boost slot #{slot_id} to {channel_title}? Reassigning a boost later requires a 24-hour cooldown.",
                                            ))
                                            .size(12.0)
                                            .color(palette.window_fg),
                                        )
                                        .wrap(),
                                    );
                                    if let Some(ref err) = self.boost_error {
                                        ui.add_space(4.0);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(err)
                                                    .size(11.5)
                                                    .color(palette.attention_fg),
                                            )
                                            .wrap(),
                                        );
                                    }
                                    ui.add_space(8.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if pill_button(
                                            ui,
                                            "Confirm Boost",
                                            PREMIUM_PURPLE,
                                            Color32::WHITE,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            if let Some(ref mut be) = payments {
                                                match be.apply_channel_boost(
                                                    &channel_title,
                                                    &[slot_id],
                                                ) {
                                                    Ok(updated) => {
                                                        self.bundle = updated;
                                                        self.pending_boost_slot = None;
                                                        self.boost_error = None;
                                                        toast = Some(format!(
                                                            "Boosted {channel_title} using slot #{slot_id}."
                                                        ));
                                                    }
                                                    Err(err) => {
                                                        self.boost_error = Some(err.clone());
                                                        toast = Some(err);
                                                    }
                                                }
                                            } else {
                                                self.boost_error = Some(
                                                    "Payments backend is unavailable to apply boost."
                                                        .to_owned(),
                                                );
                                            }
                                        }
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            self.pending_boost_slot = None;
                                            self.boost_error = None;
                                        }
                                    });
                                });
                        }
                    });
            });

        section_divider(ui, palette);

        // Action toggles for Create Giveaway & Replace Boost boxes
        let mp_opt = marketplace.or_else(|| payments.and_then(|p| p.as_marketplace_backend()));
        if let Some(mp) = mp_opt {
            let channel_title = self.bundle.channel_title.clone();
            Frame::NONE
                .inner_margin(Margin::symmetric(margin_h, 6))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                        if pill_button(
                            ui,
                            if self.show_create_giveaway {
                                "Hide Create Giveaway"
                            } else {
                                "+ Create Giveaway"
                            },
                            if self.show_create_giveaway {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            },
                            if self.show_create_giveaway {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.show_create_giveaway = !self.show_create_giveaway;
                        }
                        if pill_button(
                            ui,
                            if self.show_replace_boost {
                                "Hide Replace Boosts"
                            } else {
                                "Replace / Reassign Boosts"
                            },
                            if self.show_replace_boost {
                                palette.window_bg_active
                            } else {
                                palette.window_bg_over
                            },
                            if self.show_replace_boost {
                                palette.window_fg_active
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.show_replace_boost = !self.show_replace_boost;
                        }
                    });

                    if self.show_create_giveaway {
                        ui.add_space(8.0);
                        if let Some(msg) =
                            self.create_giveaway
                                .show(ui, &channel_title, now, mp, palette)
                        {
                            toast = Some(msg);
                        }
                    }

                    if self.show_replace_boost {
                        ui.add_space(8.0);
                        if let Some(msg) = self.replace_boost.show(ui, &channel_title, mp, palette)
                        {
                            toast = Some(msg);
                        }
                    }
                });
            section_divider(ui, palette);
        }

        // Giveaway Info Boxes
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                subsection_title(ui, "Giveaway Info Boxes", palette);
                ui.add_space(8.0);

                for (idx, gw) in self.bundle.giveaways.clone().iter().enumerate() {
                    let selected = self.selected_giveaway == Some(idx);
                    if show_giveaway_card(ui, gw, selected, palette) {
                        self.selected_giveaway = Some(idx);
                    }
                    ui.add_space(8.0);
                }
            });

        toast
    }
}

fn show_giveaway_card(
    ui: &mut Ui,
    gw: &GiveawayInfoItem,
    expanded: bool,
    palette: &Palette,
) -> bool {
    let mut clicked = false;
    let (status_label, status_bg, status_fg) = match gw.info.state {
        GiveawayState::Running => ("RUNNING", SUCCESS_SURFACE, SUCCESS_GREEN),
        GiveawayState::Preparing => ("PREPARING", with_alpha(STAR_GOLD, 0.16), STAR_GOLD),
        GiveawayState::Finished => (
            "FINISHED",
            with_alpha(palette.window_bg_active, 0.14),
            palette.window_active_text_fg,
        ),
        GiveawayState::Refunded => (
            "REFUNDED",
            with_alpha(palette.attention_fg, 0.14),
            palette.attention_fg,
        ),
        GiveawayState::Invalid => (
            "INVALID",
            palette.window_bg_over,
            palette.window_sub_text_fg,
        ),
    };

    Frame::NONE
        .fill(palette.window_bg)
        .stroke(Stroke::new(
            if expanded { 1.5 } else { 1.0 },
            if expanded {
                palette.window_bg_active
            } else {
                palette.divider
            },
        ))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&gw.title)
                        .size(14.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (badge, _) = ui.allocate_exact_size(Vec2::new(72.0, 20.0), Sense::hover());
                    ui.painter()
                        .rect_filled(badge, CornerRadius::same(10), status_bg);
                    ui.painter().text(
                        badge.center(),
                        Align2::CENTER_CENTER,
                        status_label,
                        FontId::proportional(9.5),
                        status_fg,
                    );
                });
            });

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Prizes: {}", gw.prize_summary))
                    .size(12.5)
                    .strong()
                    .color(STAR_GOLD),
            );
            if !gw.prize_description.is_empty() {
                ui.label(
                    egui::RichText::new(&gw.prize_description)
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Host: {}  •  Region: {}  •  Draw: {}",
                    gw.channel_title,
                    gw.countries_summary,
                    format_unix_date(gw.info.finish_date)
                ))
                .size(11.5)
                .color(palette.window_sub_text_fg),
            );

            if gw.info.state == GiveawayState::Running && gw.info.participating {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("You are participating in this giveaway.")
                        .size(12.0)
                        .strong()
                        .color(SUCCESS_GREEN),
                );
            } else if gw.info.state == GiveawayState::Finished {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} winners chosen  •  {} gift links activated{}",
                        gw.info.winners_count,
                        gw.info.activated_count,
                        if gw.info.credits > 0 {
                            format!("  •  You won {} Stars (demo)!", gw.info.credits)
                        } else {
                            String::new()
                        }
                    ))
                    .size(12.0)
                    .color(palette.window_active_text_fg),
                );
            }

            if !expanded {
                ui.add_space(4.0);
                if pill_button(
                    ui,
                    "Inspect Rules",
                    palette.window_bg_over,
                    palette.window_active_text_fg,
                    Some(palette.divider),
                )
                .clicked()
                {
                    clicked = true;
                }
            }
        });

    clicked
}

fn paint_premium_gradient(painter: &egui::Painter, rect: Rect) {
    // Smooth 3-stop horizontal gradient strip (Indigo -> Purple -> Pink)
    let bands = 48usize;
    let step = rect.width() / bands as f32;
    for i in 0..bands {
        let t = i as f32 / (bands - 1) as f32;
        let col = if t < 0.5 {
            lerp_color(PREMIUM_INDIGO, PREMIUM_PURPLE, t * 2.0)
        } else {
            lerp_color(PREMIUM_PURPLE, PREMIUM_PINK, (t - 0.5) * 2.0)
        };
        let band = Rect::from_min_max(
            Pos2::new(rect.left() + i as f32 * step, rect.top()),
            Pos2::new(rect.left() + (i + 1) as f32 * step + 0.5, rect.bottom()),
        );
        painter.rect_filled(band, CornerRadius::ZERO, col);
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let r = (f32::from(a.r()) * (1.0 - t) + f32::from(b.r()) * t).round() as u8;
    let g = (f32::from(a.g()) * (1.0 - t) + f32::from(b.g()) * t).round() as u8;
    let bl = (f32::from(a.b()) * (1.0 - t) + f32::from(b.b()) * t).round() as u8;
    Color32::from_rgb(r, g, bl)
}
