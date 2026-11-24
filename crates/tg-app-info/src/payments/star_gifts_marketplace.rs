//! Star Gifts Resale Marketplace, Live Auctions, and Crafting Forge UI panels.
//!
//! Port origin: `boxes/star_gift_resale_box.cpp`, `boxes/star_gift_auction_box.cpp`,
//! and `boxes/star_gift_craft_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    TextEdit, Ui, Vec2,
};
use tg_payments::star_gift_auctions::{
    ActiveAuctionsSnapshot, GiftAuctionAcquiredGift, format_auction_countdown,
};
use tg_payments::star_gift_craft::{
    CraftGiftSlot, MAX_CRAFT_SLOTS, compute_craft_plan, format_craft_percent,
};
use tg_payments::star_gifts::{
    GiftAttributeId, ResaleGiftsDescriptor, ResaleGiftsFilter, ResaleGiftsSort, SavedStarGiftId,
};

use crate::payments::backend::ReceivedGiftItem;
use crate::payments::credits::format_stars_count;
use crate::payments::marketplace_backend::{CraftExecutionOutcome, StarGiftsMarketplaceBackend};
use crate::payments::ui_helpers::{
    PREMIUM_PURPLE, STAR_GOLD, STAR_SURFACE, SUCCESS_GREEN, SUCCESS_SURFACE, format_unix_date,
    paint_gift_artwork, paint_star_emblem, pill_button, primary_button, rgb_to_color32,
    section_divider, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Interactive Resale Marketplace state (`boxes/star_gift_resale_box.cpp`).
#[derive(Debug, Clone)]
pub struct ResaleMarketplaceView {
    /// Base catalog gift ID being browsed on resale.
    pub gift_id: u64,
    /// Active sort & attribute filters.
    pub filter: ResaleGiftsFilter,
    /// Last loaded resale descriptor page.
    pub descriptor: ResaleGiftsDescriptor,
    /// Optional recipient override for buying a resale listing.
    pub buyer_recipient: String,
    /// Whether initial data has been loaded from the backend.
    pub loaded: bool,
}

impl Default for ResaleMarketplaceView {
    fn default() -> Self {
        Self {
            gift_id: 10_002,
            filter: ResaleGiftsFilter::default(),
            descriptor: ResaleGiftsDescriptor::default(),
            buyer_recipient: String::new(),
            loaded: false,
        }
    }
}

impl ResaleMarketplaceView {
    /// Reloads resale listings from `backend` using the active [`ResaleGiftsFilter`].
    pub fn reload(&mut self, backend: &dyn StarGiftsMarketplaceBackend) -> Result<(), String> {
        let desc = backend.load_resale_star_gifts(self.gift_id, &self.filter, "")?;
        self.filter.attributes_hash = desc.attributes_hash;
        self.descriptor = desc;
        self.loaded = true;
        Ok(())
    }

    /// Toggles an attribute filter chip and reloads listings.
    pub fn toggle_attribute(
        &mut self,
        id: GiftAttributeId,
        backend: &dyn StarGiftsMarketplaceBackend,
    ) -> Result<(), String> {
        if !self.filter.attributes.remove(&id) {
            self.filter.attributes.insert(id);
        }
        self.reload(backend)
    }

    /// Renders the resale marketplace browser.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        if !self.loaded
            && let Err(err) = self.reload(backend)
        {
            toast = Some(err);
        }

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!("Resale Marketplace ({} listings)", self.descriptor.count),
                    palette,
                );
                ui.add_space(6.0);

                // Sort & currency filters
                let mut filter_changed = false;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for (sort, label) in [
                        (ResaleGiftsSort::Price, "Lowest Price"),
                        (ResaleGiftsSort::Date, "Newest"),
                        (ResaleGiftsSort::Number, "Lowest #"),
                    ] {
                        let active = self.filter.sort == sort;
                        let bg = if active {
                            palette.window_bg_active
                        } else {
                            palette.window_bg_over
                        };
                        let fg = if active {
                            palette.window_fg_active
                        } else {
                            palette.window_fg
                        };
                        if pill_button(ui, label, bg, fg, None).clicked() && !active {
                            self.filter.sort = sort;
                            filter_changed = true;
                        }
                    }
                    let stars_bg = if self.filter.stars_only {
                        STAR_GOLD
                    } else {
                        palette.window_bg_over
                    };
                    let stars_fg = if self.filter.stars_only {
                        Color32::WHITE
                    } else {
                        palette.window_fg
                    };
                    if pill_button(ui, "Stars Only", stars_bg, stars_fg, None).clicked() {
                        self.filter.stars_only = !self.filter.stars_only;
                        filter_changed = true;
                    }
                    if !self.filter.attributes.is_empty()
                        && pill_button(
                            ui,
                            "Clear Attrs",
                            palette.window_bg_over,
                            palette.attention_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                    {
                        self.filter.attributes.clear();
                        filter_changed = true;
                    }
                });

                // Attribute filter chips (Models & Backdrops)
                if !self.descriptor.models.is_empty() || !self.descriptor.backdrops.is_empty() {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                        for (model, count) in self.descriptor.models.clone() {
                            let id = GiftAttributeId::for_model(&model);
                            let selected = self.filter.attributes.contains(&id);
                            let label = format!("Model: {} ({count})", model.attribute.name);
                            let bg = if selected {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            };
                            let fg = if selected {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            };
                            if pill_button(ui, &label, bg, fg, None).clicked() {
                                if !self.filter.attributes.remove(&id) {
                                    self.filter.attributes.insert(id);
                                }
                                filter_changed = true;
                            }
                        }
                        for (backdrop, count) in self.descriptor.backdrops.clone() {
                            let id = GiftAttributeId::for_backdrop(&backdrop);
                            let selected = self.filter.attributes.contains(&id);
                            let label = format!("Backdrop: {} ({count})", backdrop.attribute.name);
                            let bg = if selected {
                                rgb_to_color32(backdrop.center_color)
                            } else {
                                palette.window_bg_over
                            };
                            let fg = if selected {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            };
                            if pill_button(ui, &label, bg, fg, None).clicked() {
                                if !self.filter.attributes.remove(&id) {
                                    self.filter.attributes.insert(id);
                                }
                                filter_changed = true;
                            }
                        }
                    });
                }

                if filter_changed && let Err(err) = self.reload(backend) {
                    toast = Some(err);
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Buy for:")
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.buyer_recipient)
                            .desired_width(170.0)
                            .hint_text("Self or @username"),
                    );
                });

                ui.add_space(8.0);
                if self.descriptor.list.is_empty() {
                    ui.label(
                        egui::RichText::new("No resale listings match the current filters.")
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                }

                for gift in self.descriptor.list.clone() {
                    let Some(ref unique) = gift.unique else {
                        continue;
                    };
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let bg_col = rgb_to_color32(unique.backdrop.center_color);
                                let (art_rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
                                ui.painter()
                                    .rect_filled(art_rect, CornerRadius::same(8), bg_col);
                                paint_gift_artwork(ui.painter(), art_rect, "gem", bg_col);

                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(unique.display_name())
                                            .size(13.5)
                                            .strong()
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} ({})  •  {}  •  {}",
                                            unique.model.attribute.name,
                                            unique.model.attribute.rarity_text(),
                                            unique.backdrop.attribute.name,
                                            unique.pattern.attribute.name
                                        ))
                                        .size(11.0)
                                        .color(palette.window_sub_text_fg),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let asked = unique.resale_asked();
                                        let price_label = if unique.only_accept_ton {
                                            format!("Buy {} TON", asked.whole())
                                        } else {
                                            format!(
                                                "Buy {} Stars",
                                                format_stars_count(unique.stars_for_resale)
                                            )
                                        };
                                        if pill_button(
                                            ui,
                                            &price_label,
                                            if unique.only_accept_ton {
                                                palette.window_bg_active
                                            } else {
                                                STAR_GOLD
                                            },
                                            Color32::WHITE,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            match backend.buy_resale_star_gift(
                                                &unique.slug,
                                                &self.buyer_recipient,
                                                unique.only_accept_ton,
                                                "",
                                            ) {
                                                Ok(msg) => toast = Some(msg),
                                                Err(err) => toast = Some(err),
                                            }
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }
            });

        toast
    }
}

/// Interactive Star Gift Auctions state (`boxes/star_gift_auction_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct AuctionsView {
    /// Active auctions snapshot.
    pub snapshot: ActiveAuctionsSnapshot,
    /// Selected auction index.
    pub selected_index: usize,
    /// Acquired gifts for the selected auction.
    pub acquired: Vec<GiftAuctionAcquiredGift>,
    /// User-entered bid amount in Stars.
    pub bid_input: String,
    /// Hide bidder name flag.
    pub hide_name: bool,
    /// Optional gift recipient username.
    pub recipient: String,
    /// Optional message attached to auction bid.
    pub message: String,
    /// Pending bid amount awaiting explicit user confirmation before sending `payments.sendStarsForm`.
    pub pending_bid_confirm: Option<i64>,
    /// Whether initial data has been loaded.
    pub loaded: bool,
}

impl AuctionsView {
    /// Reloads active auctions and acquired gifts for the selected auction.
    pub fn reload(&mut self, backend: &dyn StarGiftsMarketplaceBackend) -> Result<(), String> {
        self.snapshot = backend.load_active_auctions()?;
        if let Some(first) = self.snapshot.auctions.get(self.selected_index) {
            let min_bid = first.effective_min_bid();
            if self.bid_input.is_empty() {
                self.bid_input = min_bid.to_string();
            }
            if let Some(ref g) = first.gift {
                self.acquired = backend
                    .load_auction_acquired_gifts(g.id)
                    .unwrap_or_default();
            }
        }
        self.loaded = true;
        Ok(())
    }

    /// Renders the live auctions list, leaderboard, bid form, and acquired gifts.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        if !self.loaded
            && let Err(err) = self.reload(backend)
        {
            toast = Some(err);
        }

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(
                        ui,
                        &format!("Star Gift Auctions ({})", self.snapshot.auctions.len()),
                        palette,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Refresh",
                            palette.window_bg_over,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                            && let Err(err) = self.reload(backend)
                        {
                            toast = Some(err);
                        }
                    });
                });

                ui.add_space(6.0);
                let Some(auction) = self.snapshot.auctions.get(self.selected_index).cloned() else {
                    ui.label(
                        egui::RichText::new("No active Star Gift auctions right now.")
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                    return;
                };

                let title = auction
                    .gift
                    .as_ref()
                    .map(|g| {
                        if g.resell_title.is_empty() {
                            format!("Auction Gift #{}", g.id)
                        } else {
                            g.resell_title.clone()
                        }
                    })
                    .unwrap_or_else(|| "Star Gift Auction".to_owned());
                let gift_id = auction.gift.as_ref().map_or(0, |g| g.id);

                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.2, PREMIUM_PURPLE))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&title)
                                    .size(15.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let status = if auction.is_finished() {
                                        "FINISHED".to_owned()
                                    } else {
                                        format!(
                                            "Round {}/{}  •  {} left",
                                            auction.current_round,
                                            auction.total_rounds,
                                            auction.gifts_left
                                        )
                                    };
                                    ui.label(
                                        egui::RichText::new(status)
                                            .size(11.5)
                                            .strong()
                                            .color(PREMIUM_PURPLE),
                                    );
                                },
                            );
                        });
                        ui.add_space(4.0);

                        let countdown = format_auction_countdown(
                            (auction.next_round_at - auction.start_date).max(0),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "Gifts/round: {}  •  Min winning bid: {} Stars  •  Round window: {}",
                                auction.gifts_per_round(),
                                format_stars_count(auction.min_winning_bid()),
                                countdown
                            ))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                        );

                        if auction.my.bid > 0 {
                            ui.add_space(4.0);
                            let pos = auction.my_position();
                            let winning = auction.is_my_bid_winning();
                            let status_col = if winning {
                                SUCCESS_GREEN
                            } else {
                                palette.attention_fg
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "Your Bid: {} Stars  •  Rank #{} ({})",
                                    format_stars_count(auction.my.bid),
                                    pos,
                                    if winning { "Winning" } else { "Outbid" }
                                ))
                                .size(12.5)
                                .strong()
                                .color(status_col),
                            );
                        }

                        ui.add_space(8.0);
                        subsection_title(ui, "Current Bid Levels", palette);
                        ui.add_space(4.0);
                        for lvl in &auction.bid_levels {
                            let is_win = lvl.position <= auction.gifts_per_round();
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("#{}", lvl.position))
                                        .size(12.0)
                                        .strong()
                                        .color(if is_win {
                                            SUCCESS_GREEN
                                        } else {
                                            palette.attention_fg
                                        }),
                                );
                                ui.label(
                                    egui::RichText::new(if is_win {
                                        "Winning tier"
                                    } else {
                                        "Runner-up tier"
                                    })
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} Stars",
                                                format_stars_count(lvl.amount)
                                            ))
                                            .size(12.0)
                                            .strong()
                                            .color(STAR_GOLD),
                                        );
                                    },
                                );
                            });
                            thin_divider(ui, palette);
                        }

                        if !auction.is_finished() {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let bid_lbl = ui.label(
                                    egui::RichText::new("Bid (Stars):")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                ui.add(
                                    TextEdit::singleline(&mut self.bid_input)
                                        .desired_width(110.0)
                                        .hint_text("Stars"),
                                )
                                .labelled_by(bid_lbl.id);
                                ui.checkbox(&mut self.hide_name, "Anonymous");
                            });
                            ui.add_space(6.0);
                            let parsed_bid = self.bid_input.trim().parse::<i64>().ok();
                            let btn_label = if auction.my.bid > 0 {
                                match parsed_bid {
                                    Some(amt) if amt > auction.my.bid => format!(
                                        "Raise Bid to {} Stars (+{} Stars)",
                                        format_stars_count(amt),
                                        format_stars_count(amt - auction.my.bid),
                                    ),
                                    Some(amt) if amt > 0 => format!(
                                        "Raise Bid to {} Stars",
                                        format_stars_count(amt),
                                    ),
                                    _ => "Raise Bid".to_owned(),
                                }
                            } else {
                                match parsed_bid {
                                    Some(amt) if amt > 0 => tg_lang::tr::lng_auction_bid_place(
                                        &format!("{} Stars", format_stars_count(amt)),
                                    ),
                                    _ => tg_lang::tr::lng_auction_bid_title(),
                                }
                            };
                            if let Some(confirm_amount) = self.pending_bid_confirm {
                                Frame::NONE
                                    .fill(with_alpha(PREMIUM_PURPLE, 0.08))
                                    .stroke(Stroke::new(1.0, PREMIUM_PURPLE))
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.set_min_width(ui.available_width());
                                        let confirm_text = if auction.my.bid > 0 {
                                            let delta = (confirm_amount - auction.my.bid).max(0);
                                            format!(
                                                "Raise your bid to {} Stars (+{} additional Stars) on \"{title}\"{}?",
                                                format_stars_count(confirm_amount),
                                                format_stars_count(delta),
                                                if self.hide_name { " anonymously" } else { "" }
                                            )
                                        } else {
                                            format!(
                                                "Place a bid of {} Stars on \"{title}\"{}?",
                                                format_stars_count(confirm_amount),
                                                if self.hide_name { " anonymously" } else { "" }
                                            )
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(confirm_text)
                                                    .size(12.0)
                                                    .color(palette.window_fg),
                                            )
                                            .wrap(),
                                        );
                                        ui.add_space(8.0);
                                        ui.horizontal_wrapped(|ui| {
                                            if pill_button(
                                                ui,
                                                &format!(
                                                    "Confirm {} Stars",
                                                    format_stars_count(confirm_amount)
                                                ),
                                                PREMIUM_PURPLE,
                                                Color32::WHITE,
                                                None,
                                            )
                                            .clicked()
                                            {
                                                self.pending_bid_confirm = None;
                                                match backend.place_auction_bid(
                                                    gift_id,
                                                    confirm_amount,
                                                    auction.my.bid > 0,
                                                    self.hide_name,
                                                    &self.recipient,
                                                    &self.message,
                                                ) {
                                                    Ok(msg) => {
                                                        toast = Some(msg);
                                                        let _ = self.reload(backend);
                                                    }
                                                    Err(err) => toast = Some(err),
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
                                                self.pending_bid_confirm = None;
                                            }
                                        });
                                    });
                            } else if primary_button(
                                ui,
                                &btn_label,
                                PREMIUM_PURPLE,
                                Color32::WHITE,
                                36.0,
                            )
                            .clicked()
                            {
                                match self.bid_input.trim().parse::<i64>() {
                                    Ok(amount) if amount > 0 => {
                                        self.pending_bid_confirm = Some(amount);
                                    }
                                    _ => {
                                        toast = Some("Enter a valid numeric Star bid.".to_owned());
                                    }
                                }
                            }
                        }

                        if !self.acquired.is_empty() {
                            ui.add_space(8.0);
                            subsection_title(
                                ui,
                                &format!("Acquired in This Auction ({})", self.acquired.len()),
                                palette,
                            );
                            for acq in &self.acquired {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Round {} (#{}, rank #{}) — {} Stars on {}",
                                        acq.round,
                                        acq.number,
                                        acq.position,
                                        format_stars_count(acq.bid_amount),
                                        format_unix_date(acq.date)
                                    ))
                                    .size(11.5)
                                    .color(palette.window_fg),
                                );
                            }
                        }
                    });
            });

        toast
    }
}

/// Interactive Star Gift Crafting Forge state (`boxes/star_gift_craft_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct CraftForgeView {
    /// Base catalog gift ID being crafted (`0` = any eligible owned unique gifts).
    pub base_gift_id: u64,
    /// Eligible owned unique gifts fetched via `payments.getCraftStarGifts`.
    pub eligible: Vec<ReceivedGiftItem>,
    /// Selected gift IDs slotted into the forge (up to [`MAX_CRAFT_SLOTS`]).
    pub selected_ids: Vec<u64>,
    /// Outcome banner from the most recent craft attempt.
    pub last_outcome: Option<CraftExecutionOutcome>,
    /// Whether eligible gifts have been loaded.
    pub loaded: bool,
}

impl CraftForgeView {
    /// Loads eligible gifts from `backend`.
    pub fn reload(&mut self, backend: &dyn StarGiftsMarketplaceBackend) -> Result<(), String> {
        self.eligible = backend.load_craft_star_gifts(self.base_gift_id, "")?;
        self.selected_ids
            .retain(|id| self.eligible.iter().any(|g| g.saved.info.id == *id));
        self.loaded = true;
        Ok(())
    }

    /// Toggles a gift in/out of the crafting slots.
    pub fn toggle_slot(&mut self, gift_id: u64) -> Option<String> {
        if let Some(pos) = self.selected_ids.iter().position(|&id| id == gift_id) {
            self.selected_ids.remove(pos);
            return None;
        }
        if self.selected_ids.len() >= MAX_CRAFT_SLOTS {
            return Some(format!(
                "Forge holds at most {MAX_CRAFT_SLOTS} unique gifts."
            ));
        }
        self.selected_ids.push(gift_id);
        None
    }

    /// Computes the current [`CraftGiftSlot`] list from selected IDs.
    #[must_use]
    pub fn active_slots(&self) -> Vec<CraftGiftSlot> {
        self.selected_ids
            .iter()
            .filter_map(|&gid| {
                let item = self.eligible.iter().find(|g| g.saved.info.id == gid)?;
                let unique = item.saved.info.unique.clone()?;
                Some(CraftGiftSlot {
                    unique,
                    manage_id: item.saved.manage_id.unwrap_or(SavedStarGiftId::Slug),
                })
            })
            .collect()
    }

    /// Renders the crafting forge (slots, success gauge, backdrop/pattern chances, and craft button).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        if !self.loaded
            && let Err(err) = self.reload(backend)
        {
            toast = Some(err);
        }

        let slots = self.active_slots();
        let plan = compute_craft_plan(&slots, None);

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!(
                        "Star Gift Crafting Forge ({}/{MAX_CRAFT_SLOTS} slots)",
                        slots.len()
                    ),
                    palette,
                );
                ui.add_space(6.0);

                // Success chance summary card
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.2, palette.window_bg_active))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Success Chance: {}",
                                    plan.success_percent_text
                                ))
                                .size(14.5)
                                .strong()
                                .color(
                                    if plan.success_permille >= 500 {
                                        SUCCESS_GREEN
                                    } else {
                                        STAR_GOLD
                                    },
                                ),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !self.selected_ids.is_empty()
                                        && pill_button(
                                            ui,
                                            "Clear Slots",
                                            palette.window_bg_over,
                                            palette.window_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                    {
                                        self.selected_ids.clear();
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                        let bar_w = ui.available_width().max(160.0);
                        let (bar_rect, _) =
                            ui.allocate_exact_size(Vec2::new(bar_w, 10.0), Sense::hover());
                        ui.painter().rect_filled(
                            bar_rect,
                            CornerRadius::same(5),
                            palette.window_bg_over,
                        );
                        let frac = (plan.success_permille as f32 / 1000.0).clamp(0.0, 1.0);
                        if frac > 0.0 {
                            ui.painter().rect_filled(
                                Rect::from_min_size(bar_rect.min, Vec2::new(bar_w * frac, 10.0)),
                                CornerRadius::same(5),
                                if plan.success_permille >= 500 {
                                    SUCCESS_GREEN
                                } else {
                                    STAR_GOLD
                                },
                            );
                        }

                        if !plan.backdrop_chances.is_empty() {
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                                for ch in &plan.backdrop_chances {
                                    let bg = ch
                                        .backdrop
                                        .as_ref()
                                        .map(|b| rgb_to_color32(b.center_color))
                                        .unwrap_or(palette.window_bg_over);
                                    let _ = pill_button(
                                        ui,
                                        &format!(
                                            "{} {}",
                                            ch.name,
                                            format_craft_percent(ch.permille)
                                        ),
                                        bg,
                                        Color32::WHITE,
                                        None,
                                    );
                                }
                                for ch in &plan.pattern_chances {
                                    let _ = pill_button(
                                        ui,
                                        &format!(
                                            "Symbol: {} {}",
                                            ch.name,
                                            format_craft_percent(ch.permille)
                                        ),
                                        palette.window_bg_over,
                                        palette.window_fg,
                                        Some(palette.divider),
                                    );
                                }
                            });
                        }

                        ui.add_space(8.0);
                        let craft_btn_label = if slots.is_empty() {
                            "Select 1–4 Unique Gifts Below to Craft".to_owned()
                        } else {
                            format!(
                                "Craft Unique Gift ({} chance • {} input(s))",
                                plan.success_percent_text,
                                slots.len()
                            )
                        };
                        if primary_button(
                            ui,
                            &craft_btn_label,
                            if slots.is_empty() {
                                palette.window_bg_over
                            } else {
                                PREMIUM_PURPLE
                            },
                            if slots.is_empty() {
                                palette.window_sub_text_fg
                            } else {
                                Color32::WHITE
                            },
                            38.0,
                        )
                        .clicked()
                            && !slots.is_empty()
                        {
                            match backend.craft_star_gift(&self.selected_ids) {
                                Ok(outcome) => {
                                    let notice = match &outcome {
                                        CraftExecutionOutcome::Succeeded { notice, .. }
                                        | CraftExecutionOutcome::Failed { notice } => {
                                            notice.clone()
                                        }
                                    };
                                    self.last_outcome = Some(outcome);
                                    self.selected_ids.clear();
                                    let _ = self.reload(backend);
                                    toast = Some(notice);
                                }
                                Err(err) => toast = Some(err),
                            }
                        }

                        if let Some(ref outcome) = self.last_outcome {
                            ui.add_space(6.0);
                            let (bg, fg, msg) = match outcome {
                                CraftExecutionOutcome::Succeeded { notice, .. } => {
                                    (SUCCESS_SURFACE, SUCCESS_GREEN, notice.as_str())
                                }
                                CraftExecutionOutcome::Failed { notice } => (
                                    with_alpha(palette.attention_fg, 0.14),
                                    palette.attention_fg,
                                    notice.as_str(),
                                ),
                            };
                            Frame::NONE
                                .fill(bg)
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(8))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(msg).size(12.0).strong().color(fg),
                                    );
                                });
                        }
                    });

                section_divider(ui, palette);
                subsection_title(ui, "Eligible Unique Gifts", palette);
                ui.add_space(6.0);

                for item in self.eligible.clone() {
                    let gid = item.saved.info.id;
                    let selected = self.selected_ids.contains(&gid);
                    let chance_txt = item
                        .saved
                        .info
                        .unique
                        .as_ref()
                        .map(|u| format_craft_percent(u.craft_chance_permille))
                        .unwrap_or_else(|| "0%".to_owned());
                    let width = ui.available_width().max(200.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(8),
                        if selected {
                            with_alpha(PREMIUM_PURPLE, 0.14)
                        } else if resp.hovered() {
                            palette.window_bg_over
                        } else {
                            palette.window_bg
                        },
                    );
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(8),
                        Stroke::new(
                            if selected { 1.5 } else { 1.0 },
                            if selected {
                                PREMIUM_PURPLE
                            } else {
                                palette.divider
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 12.0, rect.top() + 18.0),
                        Align2::LEFT_CENTER,
                        &item.title,
                        FontId::proportional(13.0),
                        palette.window_bold_fg,
                    );
                    let subtitle = item
                        .saved
                        .info
                        .unique
                        .as_ref()
                        .map(|u| {
                            format!(
                                "{}  •  {}  •  +{} craft chance",
                                u.backdrop.attribute.name, u.pattern.attribute.name, chance_txt
                            )
                        })
                        .unwrap_or_default();
                    ui.painter().text(
                        Pos2::new(rect.left() + 12.0, rect.top() + 37.0),
                        Align2::LEFT_CENTER,
                        subtitle,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 12.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        if selected { "Slotted" } else { "+ Add" },
                        FontId::proportional(12.0),
                        if selected {
                            PREMIUM_PURPLE
                        } else {
                            palette.window_active_text_fg
                        },
                    );
                    if resp.clicked()
                        && let Some(msg) = self.toggle_slot(gid)
                    {
                        toast = Some(msg);
                    }
                    ui.add_space(6.0);
                }
            });

        let _ = (STAR_SURFACE, paint_star_emblem);
        toast
    }
}
