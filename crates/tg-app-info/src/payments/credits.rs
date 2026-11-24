//! Telegram Stars settings screen, top-up options list, transaction history with filters &
//! receipt detail box, active subscriptions list, plus paid media and paid reactions confirmation
//! boxes.
//!
//! Port origin: `settings/sections/settings_credits.cpp`, `settings/settings_credits_graphics.cpp`,
//! and `payments/ui/payments_reaction_box.cpp`.

use std::collections::HashMap;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Slider,
    Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_payments::credits::{
    CreditTopupOption, CreditsAmount, CreditsHistoryEntry, CreditsHistoryPeerType,
    SubscriptionEntry,
};
use tg_payments::currency::fill_amount_and_currency;

use crate::payments::backend::{
    CreditsOverviewBundle, PaidMediaPreview, PaidReactionContext, PaymentsBackend,
};
use crate::payments::ui_helpers::{
    STAR_AMBER, STAR_GOLD, STAR_SURFACE, SUCCESS_GREEN, format_unix_date, format_unix_datetime,
    paint_avatar, paint_star_emblem, pill_button, primary_button, reveal_progress, section_divider,
    subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Transaction history filter tab (`settings_credits.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StarsHistoryFilter {
    /// All transactions.
    #[default]
    All,
    /// Incoming Stars only.
    Incoming,
    /// Outgoing Stars only.
    Outgoing,
}

/// Active modal dialog over the Telegram Stars screen.
#[derive(Debug, Clone, PartialEq)]
pub enum StarsModal {
    /// Detailed transaction receipt box (`ReceiptCreditsBox`).
    Receipt(Box<CreditsHistoryEntry>),
    /// Paid media unlock confirmation box.
    PaidMediaConfirm,
    /// Paid reaction Stars slider & leaderboard confirmation box (`payments_reaction_box.cpp`).
    PaidReactionConfirm {
        /// Chosen Stars amount (clamped by `PaidReactionConfig`).
        stars: i32,
        /// Hide current user from top reactors list.
        anonymous: bool,
    },
    /// Read-only Stars top-up payment form preview (`payments.getPaymentForm`).
    TopUpPreview {
        /// Package title (e.g. `"250 Stars"`).
        title: String,
        /// Server or preview details string.
        body: String,
    },
    /// Recurring Stars subscription management box (`settings_credits_graphics.cpp`),
    /// confirming cancel or renew via `payments.changeStarsSubscription`.
    SubscriptionManage {
        /// Selected subscription entry.
        entry: SubscriptionEntry,
        /// Optional inline RPC error if the last cancel/renew call failed.
        error: Option<String>,
    },
}

/// View-model & renderer for Telegram Stars (`settings_credits.cpp`) and paid confirmations.
pub struct StarsView {
    /// Current Stars balance.
    pub balance: CreditsAmount,
    /// Current TON balance.
    pub ton_balance: CreditsAmount,
    /// Available top-up packages.
    pub topup_options: Vec<CreditTopupOption>,
    /// Whether extended top-up packages are expanded.
    pub show_extended_topups: bool,
    /// Transaction history ledger.
    pub history: Vec<CreditsHistoryEntry>,
    /// Lookup map of transaction ID -> counterparty display name.
    pub peer_names: HashMap<String, String>,
    /// Active history filter tab.
    pub filter: StarsHistoryFilter,
    /// Recurring Stars subscriptions.
    pub subscriptions: Vec<SubscriptionEntry>,
    /// Sample paid media preview for the paid media confirmation box.
    pub sample_paid_media: PaidMediaPreview,
    /// Sample post context for the paid reactions confirmation box.
    pub sample_paid_reaction: PaidReactionContext,
    /// Active modal box, if any.
    pub modal: Option<StarsModal>,
}

impl StarsView {
    /// Creates a [`StarsView`] from a [`CreditsOverviewBundle`].
    #[must_use]
    pub fn new(bundle: CreditsOverviewBundle) -> Self {
        let peer_names = bundle.history_peer_names.into_iter().collect();
        Self {
            balance: bundle.balance,
            ton_balance: bundle.ton_balance,
            topup_options: bundle.topup_options,
            show_extended_topups: false,
            history: bundle.history,
            peer_names,
            filter: StarsHistoryFilter::All,
            subscriptions: bundle.subscriptions,
            sample_paid_media: bundle.sample_paid_media,
            sample_paid_reaction: bundle.sample_paid_reaction,
            modal: None,
        }
    }

    /// Returns transactions matching the active [`StarsHistoryFilter`].
    #[must_use]
    pub fn filtered_history(&self) -> Vec<&CreditsHistoryEntry> {
        self.history
            .iter()
            .filter(|e| match self.filter {
                StarsHistoryFilter::All => true,
                StarsHistoryFilter::Incoming => e.incoming || e.credits.whole() >= 0,
                StarsHistoryFilter::Outgoing => !e.incoming && e.credits.whole() < 0,
            })
            .collect()
    }

    /// Returns top-up options currently visible (core list vs expanded list).
    #[must_use]
    pub fn visible_topup_options(&self) -> Vec<&CreditTopupOption> {
        self.topup_options
            .iter()
            .filter(|o| self.show_extended_topups || !o.extended)
            .collect()
    }

    /// Opens the Paid Media confirmation box.
    pub fn open_paid_media_box(&mut self) {
        self.modal = Some(StarsModal::PaidMediaConfirm);
    }

    /// Opens the Paid Reactions confirmation box with the default clamped Stars amount.
    pub fn open_paid_reaction_box(&mut self) {
        let default_stars = self.sample_paid_reaction.config.default_chosen();
        self.modal = Some(StarsModal::PaidReactionConfirm {
            stars: default_stars,
            anonymous: false,
        });
    }

    /// Refreshes local history and balance from `backend`.
    pub fn refresh_from_backend(&mut self, backend: &dyn PaymentsBackend) {
        if let Ok(bundle) = backend.load_credits_overview() {
            self.balance = bundle.balance;
            self.ton_balance = bundle.ton_balance;
            self.history = bundle.history;
            self.peer_names = bundle.history_peer_names.into_iter().collect();
            self.subscriptions = bundle.subscriptions;
            self.sample_paid_media = bundle.sample_paid_media;
        }
    }

    /// Renders the Telegram Stars screen and any active modal box.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        // Hero balance header
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 16))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let (star_rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                    ui.painter()
                        .circle_filled(star_rect.center(), 30.0, STAR_SURFACE);
                    paint_star_emblem(ui.painter(), star_rect.center(), 24.0, STAR_GOLD);

                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("Telegram Stars")
                            .size(18.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Your Balance: {} Stars",
                            format_stars_count(self.balance.whole())
                        ))
                        .size(22.0)
                        .strong()
                        .color(STAR_GOLD),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "TON Balance: {:.2} TON  •  Use Stars for digital goods, media, reactions & gifts",
                            self.ton_balance.value()
                        ))
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                    );
                });

                ui.add_space(12.0);
                // Quick confirmation box launchers (Paid Media & Paid Reaction)
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                    if pill_button(
                        ui,
                        if self.sample_paid_media.cost.unlocked {
                            "Paid Media (Unlocked)"
                        } else {
                            "Try Paid Media Box..."
                        },
                        palette.window_bg_active,
                        palette.window_fg_active,
                        None,
                    )
                    .clicked()
                    {
                        self.open_paid_media_box();
                    }
                    if pill_button(
                        ui,
                        "Try Paid Reaction Box...",
                        STAR_GOLD,
                        Color32::WHITE,
                        None,
                    )
                    .clicked()
                    {
                        self.open_paid_reaction_box();
                    }
                });
            });

        // Active modal box rendered prominently near the top so it is visible in screenshots too.
        if let Some(modal_toast) = self.show_modal_if_any(ui, backend, palette) {
            toast = Some(modal_toast);
        }

        section_divider(ui, palette);

        // Top-up options list (`settings_credits.cpp`)
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                subsection_title(ui, "Buy More Stars", palette);
                ui.add_space(6.0);

                let visible: Vec<CreditTopupOption> =
                    self.visible_topup_options().into_iter().cloned().collect();
                for opt in visible {
                    let price_str =
                        fill_amount_and_currency(opt.amount as i64, &opt.currency, false);
                    let width = ui.available_width().max(160.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                    }
                    paint_star_emblem(
                        ui.painter(),
                        Pos2::new(rect.left() + 18.0, rect.center().y),
                        10.0,
                        STAR_GOLD,
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 36.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        format!("{} Stars", format_stars_count(opt.credits as i64)),
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    let pill = Rect::from_min_size(
                        Pos2::new(rect.right() - 78.0, rect.center().y - 12.0),
                        Vec2::new(72.0, 24.0),
                    );
                    ui.painter().rect_filled(
                        pill,
                        CornerRadius::same(12),
                        with_alpha(palette.window_bg_active, 0.14),
                    );
                    ui.painter().text(
                        pill.center(),
                        Align2::CENTER_CENTER,
                        &price_str,
                        FontId::proportional(12.0),
                        palette.window_active_text_fg,
                    );
                    if resp.clicked() {
                        match backend.topup_stars(&opt) {
                            Ok(msg) => {
                                self.modal = Some(StarsModal::TopUpPreview {
                                    title: format!(
                                        "{} Stars ({price_str})",
                                        format_stars_count(opt.credits as i64)
                                    ),
                                    body: msg.clone(),
                                });
                                toast = Some(msg);
                            }
                            Err(err) => toast = Some(err),
                        }
                    }
                }

                if self.topup_options.iter().any(|o| o.extended) {
                    ui.add_space(4.0);
                    let toggle_lbl = if self.show_extended_topups {
                        "Show fewer options"
                    } else {
                        "Show more options..."
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(toggle_lbl)
                                    .size(12.5)
                                    .color(palette.window_active_text_fg),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.show_extended_topups = !self.show_extended_topups;
                    }
                }
            });

        // Active & Past Subscriptions list (`Credits::setupSubscriptions`)
        if !self.subscriptions.is_empty() {
            section_divider(ui, palette);
            Frame::NONE
                .inner_margin(Margin::symmetric(18, 6))
                .show(ui, |ui| {
                    subsection_title(ui, &tr::lng_credits_subscription_section(), palette);
                    ui.add_space(6.0);
                    for sub in self.subscriptions.clone() {
                        let width = ui.available_width().max(160.0);
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(width, 50.0), Sense::click());
                        let date_label = format_unix_date(sub.until);
                        let status = if sub.expired {
                            tr::lng_credits_subscription_status_none(&date_label)
                        } else if sub.cancelled {
                            tr::lng_credits_subscription_status_off(&date_label)
                        } else {
                            tr::lng_credits_subscription_status_on(&date_label)
                        };
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                true,
                                format!("{} ({})", sub.title, status),
                            )
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(8),
                                palette.window_bg_over,
                            );
                        }
                        let initials = sub
                            .title
                            .split_whitespace()
                            .take(2)
                            .filter_map(|w| w.chars().next())
                            .collect::<String>();
                        paint_avatar(
                            ui.painter(),
                            Pos2::new(rect.left() + 20.0, rect.center().y),
                            16.0,
                            palette.window_bg_active,
                            &initials,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 46.0, rect.top() + 16.0),
                            Align2::LEFT_CENTER,
                            &sub.title,
                            FontId::proportional(13.5),
                            palette.window_bold_fg,
                        );
                        ui.painter().text(
                            Pos2::new(rect.left() + 46.0, rect.top() + 34.0),
                            Align2::LEFT_CENTER,
                            &status,
                            FontId::proportional(11.5),
                            if sub.cancelled || sub.expired {
                                palette.attention_fg
                            } else {
                                palette.window_sub_text_fg
                            },
                        );
                        ui.painter().text(
                            Pos2::new(rect.right() - 8.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            format!("{} Stars / mo", sub.subscription.credits),
                            FontId::proportional(12.5),
                            STAR_GOLD,
                        );
                        if resp.clicked() {
                            self.modal = Some(StarsModal::SubscriptionManage {
                                entry: sub,
                                error: None,
                            });
                        }
                    }
                });
        }

        section_divider(ui, palette);

        // Transaction history with filter tabs + clickable Receipt box (`settings_credits.cpp`)
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(ui, "Transaction History", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for (tab, label) in [
                            (StarsHistoryFilter::Outgoing, "Outgoing"),
                            (StarsHistoryFilter::Incoming, "Incoming"),
                            (StarsHistoryFilter::All, "All"),
                        ] {
                            let active = self.filter == tab;
                            let bg = if active {
                                palette.window_bg_active
                            } else {
                                palette.window_bg_over
                            };
                            let fg = if active {
                                palette.window_fg_active
                            } else {
                                palette.window_sub_text_fg
                            };
                            if pill_button(ui, label, bg, fg, None).clicked() {
                                self.filter = tab;
                            }
                        }
                    });
                });
                ui.add_space(6.0);

                let entries: Vec<CreditsHistoryEntry> =
                    self.filtered_history().into_iter().cloned().collect();
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new("No transactions match this filter.")
                            .size(12.5)
                            .color(palette.window_sub_text_fg),
                    );
                } else {
                    for entry in entries {
                        let peer_name = self
                            .peer_names
                            .get(&entry.id)
                            .cloned()
                            .unwrap_or_else(|| peer_type_label(entry.peer_type).to_owned());
                        let width = ui.available_width().max(160.0);
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(8),
                                palette.window_bg_over,
                            );
                        }

                        let positive = entry.credits.whole() >= 0;
                        let badge_col = if entry.refunded {
                            palette.window_bg_active
                        } else if positive {
                            SUCCESS_GREEN
                        } else {
                            STAR_AMBER
                        };
                        let icon_text = if entry.stargift {
                            "GF"
                        } else if entry.reaction {
                            "RX"
                        } else if entry.subscription_period > 0 {
                            "SB"
                        } else if entry.refunded {
                            "RF"
                        } else if positive {
                            "+"
                        } else {
                            "*"
                        };
                        let icon_rect = Rect::from_min_size(
                            Pos2::new(rect.left() + 4.0, rect.center().y - 16.0),
                            Vec2::splat(32.0),
                        );
                        ui.painter().rect_filled(
                            icon_rect,
                            CornerRadius::same(16),
                            with_alpha(badge_col, 0.16),
                        );
                        ui.painter().text(
                            icon_rect.center(),
                            Align2::CENTER_CENTER,
                            icon_text,
                            FontId::proportional(11.5),
                            badge_col,
                        );

                        ui.painter().text(
                            Pos2::new(icon_rect.right() + 10.0, rect.top() + 17.0),
                            Align2::LEFT_CENTER,
                            format!("{}  •  {}", entry.title, peer_name),
                            FontId::proportional(13.0),
                            palette.window_bold_fg,
                        );
                        ui.painter().text(
                            Pos2::new(icon_rect.right() + 10.0, rect.top() + 35.0),
                            Align2::LEFT_CENTER,
                            format!(
                                "{}  •  {}",
                                format_unix_datetime(entry.date),
                                entry.description
                            ),
                            FontId::proportional(11.0),
                            palette.window_sub_text_fg,
                        );

                        let amt = entry.credits.whole();
                        let amt_text = if amt >= 0 {
                            format!("+{}", format_stars_count(amt))
                        } else {
                            format_stars_count(amt)
                        };
                        ui.painter().text(
                            Pos2::new(rect.right() - 24.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            amt_text,
                            FontId::proportional(13.5),
                            if positive {
                                SUCCESS_GREEN
                            } else {
                                palette.attention_fg
                            },
                        );
                        paint_star_emblem(
                            ui.painter(),
                            Pos2::new(rect.right() - 12.0, rect.center().y),
                            7.0,
                            STAR_GOLD,
                        );

                        if resp.clicked() {
                            self.modal = Some(StarsModal::Receipt(Box::new(entry)));
                        }
                        thin_divider(ui, palette);
                    }
                }
            });

        toast
    }

    fn show_modal_if_any(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn PaymentsBackend,
        palette: &Palette,
    ) -> Option<String> {
        let modal = self.modal.clone()?;
        let mut toast = None;
        let mut close = false;

        let reveal = reveal_progress(ui, ui.make_persistent_id("stars_modal_reveal"), true);
        let top_pad = (6.0 + (1.0 - reveal) * 6.0).round() as i8;
        let margin_h = if ui.available_width() < 420.0 { 12 } else { 18 };
        Frame::NONE
            .inner_margin(Margin {
                left: margin_h,
                right: margin_h,
                top: top_pad,
                bottom: 6,
            })
            .show(ui, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.5, with_alpha(STAR_GOLD, reveal.max(0.25))))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| match modal {
                        StarsModal::Receipt(entry) => {
                            let peer = self
                                .peer_names
                                .get(&entry.id)
                                .cloned()
                                .unwrap_or_else(|| peer_type_label(entry.peer_type).to_owned());
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Transaction Receipt")
                                        .size(15.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Close",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            let amt = entry.credits.whole();
                            let sign = if amt >= 0 { "+" } else { "" };
                            ui.label(
                                egui::RichText::new(format!("{sign}{amt} Telegram Stars"))
                                    .size(18.0)
                                    .strong()
                                    .color(if amt >= 0 { SUCCESS_GREEN } else { STAR_GOLD }),
                            );
                            ui.add_space(6.0);
                            receipt_kv(ui, "Title", &entry.title, palette);
                            receipt_kv(ui, "Counterparty", &peer, palette);
                            receipt_kv(ui, "Date", &format_unix_datetime(entry.date), palette);
                            receipt_kv(ui, "Transaction ID", &entry.id, palette);
                            receipt_kv(ui, "Details", &entry.description, palette);
                            if entry.refunded {
                                receipt_kv(ui, "Status", "Refunded", palette);
                            }
                        }
                        StarsModal::PaidMediaConfirm => {
                            let pm = self.sample_paid_media.clone();
                            ui.label(
                                egui::RichText::new("Unlock Paid Media")
                                    .size(15.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(6.0);
                            // Blurred preview card
                            let w = ui.available_width().max(200.0);
                            let (prev_rect, _) =
                                ui.allocate_exact_size(Vec2::new(w, 72.0), Sense::hover());
                            ui.painter().rect_filled(
                                prev_rect,
                                CornerRadius::same(8),
                                Color32::from_rgb(0x2A, 0x39, 0x4E),
                            );
                            ui.painter().text(
                                prev_rect.center(),
                                Align2::CENTER_CENTER,
                                format!(
                                    "{} photos, {} video  •  {}",
                                    pm.photo_count,
                                    pm.video_count,
                                    if pm.cost.unlocked {
                                        "UNLOCKED"
                                    } else {
                                        "BLURRED PREVIEW"
                                    }
                                ),
                                FontId::proportional(13.0),
                                Color32::WHITE,
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!("{}: {}", pm.author_title, pm.caption))
                                    .size(12.0)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if !pm.cost.unlocked
                                    && pill_button(
                                        ui,
                                        &format!("Unlock for {} Stars", pm.cost.stars_amount),
                                        STAR_GOLD,
                                        Color32::WHITE,
                                        None,
                                    )
                                    .clicked()
                                {
                                    match backend
                                        .confirm_paid_media(pm.media_id, pm.cost.stars_amount)
                                    {
                                        Ok(new_bal) => {
                                            self.balance = new_bal;
                                            self.refresh_from_backend(backend);
                                            close = true;
                                            toast = Some(format!(
                                                "Unlocked paid media for {} Stars (demo).",
                                                pm.cost.stars_amount
                                            ));
                                        }
                                        Err(err) => toast = Some(err),
                                    }
                                }
                                if pill_button(
                                    ui,
                                    "Close",
                                    palette.window_bg_over,
                                    palette.window_fg,
                                    Some(palette.divider),
                                )
                                .clicked()
                                {
                                    close = true;
                                }
                            });
                        }
                        StarsModal::PaidReactionConfirm {
                            mut stars,
                            mut anonymous,
                        } => {
                            let ctx = self.sample_paid_reaction.clone();
                            ui.label(
                                egui::RichText::new(format!(
                                    "Star Reaction — {}",
                                    ctx.channel_title
                                ))
                                .size(15.0)
                                .strong()
                                .color(palette.window_bold_fg),
                            );
                            ui.label(
                                egui::RichText::new(&ctx.post_excerpt)
                                    .size(12.0)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(8.0);

                            // Top 3 reactors row (`FillTopReactors` in `payments_reaction_box.cpp`)
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 16.0;
                                for (idx, r) in ctx.top_reactors.iter().take(3).enumerate() {
                                    ui.vertical(|ui| {
                                        let (av_rect, _) = ui.allocate_exact_size(
                                            Vec2::new(64.0, 44.0),
                                            Sense::hover(),
                                        );
                                        let col = match idx {
                                            0 => STAR_GOLD,
                                            1 => palette.window_bg_active,
                                            _ => Color32::from_rgb(0x8E, 0x62, 0xE6),
                                        };
                                        paint_avatar(
                                            ui.painter(),
                                            av_rect.center(),
                                            18.0,
                                            col,
                                            &r.initials,
                                        );
                                        let shown = if r.is_self && !anonymous {
                                            r.stars + stars
                                        } else {
                                            r.stars
                                        };
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} ({shown} Stars)",
                                                r.name
                                            ))
                                            .size(11.0)
                                            .color(palette.window_fg),
                                        );
                                    });
                                }
                            });

                            ui.add_space(8.0);
                            let max_stars = ctx.config.max_per_reaction;
                            ui.add(
                                Slider::new(&mut stars, 1..=max_stars)
                                    .text("Stars")
                                    .logarithmic(true),
                            );
                            stars = ctx.config.clamp_amount(stars);
                            ui.horizontal_wrapped(|ui| {
                                for preset in [10, 50, 100, 500, 1_000] {
                                    let clamped = ctx.config.clamp_amount(preset);
                                    let active = stars == clamped;
                                    if pill_button(
                                        ui,
                                        &format!("{clamped} Stars"),
                                        if active { STAR_GOLD } else { STAR_SURFACE },
                                        if active { Color32::WHITE } else { STAR_GOLD },
                                        None,
                                    )
                                    .clicked()
                                    {
                                        stars = clamped;
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            let mut show_me = !anonymous;
                            if ui
                                .checkbox(&mut show_me, "Show me in Top Reactors")
                                .changed()
                            {
                                anonymous = !show_me;
                            }
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if primary_button(
                                    ui,
                                    &format!("Send {stars} Stars Reaction"),
                                    STAR_GOLD,
                                    Color32::WHITE,
                                    36.0,
                                )
                                .clicked()
                                {
                                    match backend.confirm_paid_reaction(
                                        ctx.post_id,
                                        stars,
                                        anonymous,
                                    ) {
                                        Ok(new_bal) => {
                                            self.balance = new_bal;
                                            self.refresh_from_backend(backend);
                                            close = true;
                                            toast = Some(format!(
                                                "Sent {stars} Stars reaction (offline demo)."
                                            ));
                                        }
                                        Err(err) => toast = Some(err),
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            if pill_button(
                                ui,
                                "Cancel",
                                palette.window_bg_over,
                                palette.window_fg,
                                Some(palette.divider),
                            )
                            .clicked()
                            {
                                close = true;
                            }
                            if !close {
                                self.modal =
                                    Some(StarsModal::PaidReactionConfirm { stars, anonymous });
                            }
                        }
                        StarsModal::TopUpPreview { title, body } => {
                            ui.label(
                                egui::RichText::new(&title)
                                    .size(14.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&body)
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(8.0);
                            if pill_button(
                                ui,
                                "Close",
                                palette.window_bg_active,
                                palette.window_fg_active,
                                None,
                            )
                            .clicked()
                            {
                                close = true;
                            }
                        }
                        StarsModal::SubscriptionManage { entry, error } => {
                            let date_label = format_unix_date(entry.until);
                            let to_cancel =
                                !entry.expired && !entry.cancelled && !entry.cancelled_by_bot;
                            let to_renew =
                                !entry.expired && entry.cancelled && !entry.cancelled_by_bot;

                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&entry.title)
                                        .size(15.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if pill_button(
                                            ui,
                                            "Close",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            close = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            receipt_kv(
                                ui,
                                "Plan",
                                &format!(
                                    "{} Stars / month",
                                    format_stars_count(entry.subscription.credits as i64)
                                ),
                                palette,
                            );
                            let next_label = if entry.expired {
                                tr::lng_credits_subscription_row_next_none()
                            } else if entry.cancelled {
                                tr::lng_credits_subscription_row_next_off()
                            } else {
                                tr::lng_credits_subscription_row_next_on()
                            };
                            receipt_kv(ui, &next_label, &date_label, palette);
                            ui.add_space(6.0);

                            let about = if entry.cancelled_by_bot {
                                tr::lng_credits_subscription_off_by_bot_about(&entry.title)
                            } else if to_cancel {
                                tr::lng_credits_subscription_on_about(&date_label)
                            } else if entry.cancelled {
                                tr::lng_credits_subscription_off_about()
                            } else {
                                tr::lng_credits_subscription_status_none(&date_label)
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(about)
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                )
                                .wrap(),
                            );

                            if let Some(ref err_msg) = error {
                                ui.add_space(6.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(err_msg)
                                            .size(12.0)
                                            .color(palette.attention_fg),
                                    )
                                    .wrap(),
                                );
                            }

                            ui.add_space(10.0);
                            if to_cancel {
                                let label = tr::lng_credits_subscription_on_button();
                                if primary_button(
                                    ui,
                                    &label,
                                    palette.attention_fg,
                                    Color32::WHITE,
                                    36.0,
                                )
                                .clicked()
                                {
                                    match backend.change_stars_subscription(&entry.id, true) {
                                        Ok(msg) => {
                                            self.refresh_from_backend(backend);
                                            close = true;
                                            toast = Some(msg);
                                        }
                                        Err(err_msg) => {
                                            self.modal = Some(StarsModal::SubscriptionManage {
                                                entry: entry.clone(),
                                                error: Some(err_msg.clone()),
                                            });
                                            toast = Some(err_msg);
                                        }
                                    }
                                }
                            } else if to_renew {
                                let label = tr::lng_credits_subscription_off_button();
                                if primary_button(
                                    ui,
                                    &label,
                                    palette.window_bg_active,
                                    palette.window_fg_active,
                                    36.0,
                                )
                                .clicked()
                                {
                                    match backend.change_stars_subscription(&entry.id, false) {
                                        Ok(msg) => {
                                            self.refresh_from_backend(backend);
                                            close = true;
                                            toast = Some(msg);
                                        }
                                        Err(err_msg) => {
                                            self.modal = Some(StarsModal::SubscriptionManage {
                                                entry: entry.clone(),
                                                error: Some(err_msg.clone()),
                                            });
                                            toast = Some(err_msg);
                                        }
                                    }
                                }
                            }
                        }
                    });
            });

        if close {
            self.modal = None;
        }
        toast
    }
}

fn receipt_kv(ui: &mut Ui, key: &str, value: &str, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(key)
                .size(12.0)
                .color(palette.window_sub_text_fg),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(12.0)
                    .color(palette.window_fg),
            );
        });
    });
}

fn peer_type_label(pt: CreditsHistoryPeerType) -> &'static str {
    match pt {
        CreditsHistoryPeerType::Peer => "Telegram User",
        CreditsHistoryPeerType::AppStore => "App Store",
        CreditsHistoryPeerType::PlayMarket => "Google Play",
        CreditsHistoryPeerType::Fragment => "Fragment",
        CreditsHistoryPeerType::Unsupported => "External",
        CreditsHistoryPeerType::PremiumBot => "PremiumBot",
        CreditsHistoryPeerType::Ads => "Telegram Ads",
        CreditsHistoryPeerType::Api => "Telegram API",
    }
}

/// Formats an integer Stars count with comma separators (`1450` -> `"1,450"`).
#[must_use]
pub fn format_stars_count(value: i64) -> String {
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3 + usize::from(neg));
    if neg {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
