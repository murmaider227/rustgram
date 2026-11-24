//! Create Giveaway box and Replace Boost box UI panels.
//!
//! Port origin: `info/channel_statistics/boosts/create_giveaway_box.cpp`,
//! `boxes/gift_premium_box.cpp`, and `boxes/peers/replace_boost_box.cpp`.

use egui::{Color32, CornerRadius, Frame, Margin, Stroke, TextEdit, Ui, Vec2};
use tg_lang::tr;
use tg_payments::giveaways::{
    GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN, GiveawayDraft, GiveawayPrizeKind, format_boost_cooldown,
};

use crate::payments::credits::format_stars_count;
use crate::payments::marketplace_backend::{
    GiveawayOptionsBundle, ReplaceBoostBundle, StarGiftsMarketplaceBackend,
};
use crate::payments::ui_helpers::{
    PREMIUM_PURPLE, STAR_GOLD, SUCCESS_GREEN, format_unix_date, pill_button, primary_button,
    subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Interactive Create Giveaway dialog state (`create_giveaway_box.cpp`).
#[derive(Debug, Clone)]
pub struct CreateGiveawayPanel {
    /// Loaded gift code & Stars options for the target channel.
    pub options: GiveawayOptionsBundle,
    /// Whether creating a Stars giveaway (`true`) or Premium giveaway (`false`).
    pub is_stars_mode: bool,
    /// Selected winner count.
    pub quantity: i32,
    /// Selected Premium duration in months (`3`, `6`, `12`).
    pub months: i32,
    /// Selected Stars giveaway option index into `options.stars_options`.
    pub selected_stars_option: usize,
    /// Comma-separated ISO2 country codes (empty = worldwide).
    pub countries_input: String,
    /// Optional custom additional prize description.
    pub prize_description: String,
    /// Duration in days from `now` until draw (`1..=7`).
    pub duration_days: i32,
    /// Restrict entries to new subscribers only.
    pub only_new_subscribers: bool,
    /// Publicly show winners list.
    pub winners_are_visible: bool,
    /// Selected prepaid giveaway ID (if launching a prepaid giveaway instead of creating a new one).
    pub selected_prepaid_id: Option<u64>,
    /// True while awaiting explicit user confirmation before requesting the invoice or launching.
    pub pending_confirm: bool,
    /// Whether options have been loaded from the backend.
    pub loaded: bool,
}

impl Default for CreateGiveawayPanel {
    fn default() -> Self {
        Self {
            options: GiveawayOptionsBundle::default(),
            is_stars_mode: false,
            quantity: 10,
            months: 3,
            selected_stars_option: 0,
            countries_input: String::new(),
            prize_description: String::new(),
            duration_days: 3,
            only_new_subscribers: false,
            winners_are_visible: true,
            selected_prepaid_id: None,
            pending_confirm: false,
            loaded: false,
        }
    }
}

impl CreateGiveawayPanel {
    /// Loads Premium gift code options & Stars giveaway options from `backend`.
    pub fn reload(
        &mut self,
        channel_name: &str,
        backend: &dyn StarGiftsMarketplaceBackend,
    ) -> Result<(), String> {
        self.options = backend.load_giveaway_options(channel_name)?;
        if let Some(&first_preset) = self.options.premium_catalog.available_presets.last() {
            self.quantity = first_preset;
        }
        self.loaded = true;
        Ok(())
    }

    /// Builds a [`GiveawayDraft`] from the current form state at reference timestamp `now`.
    #[must_use]
    pub fn build_draft(&self, now: i32) -> GiveawayDraft {
        let countries_iso2: Vec<String> = self
            .countries_input
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| s.len() == 2)
            .collect();

        if let Some(pid) = self.selected_prepaid_id
            && let Some(pre) = self.options.prepaid_giveaways.iter().find(|p| p.id == pid)
        {
            let prize = if pre.credits > 0 {
                GiveawayPrizeKind::Stars { stars: pre.credits }
            } else {
                GiveawayPrizeKind::Premium { months: pre.months }
            };
            return GiveawayDraft {
                boost_peer: self.options.channel_peer_id,
                prize,
                quantity: pre.quantity,
                additional_peers: Vec::new(),
                countries_iso2,
                prize_description: self.prize_description.clone(),
                until_date: now.saturating_add(self.duration_days.clamp(1, 7) * 86_400),
                only_new_subscribers: self.only_new_subscribers,
                winners_are_visible: self.winners_are_visible,
                prepaid_id: Some(pre.id),
            };
        }

        let prize = if self.is_stars_mode {
            let stars = self
                .options
                .stars_options
                .get(self.selected_stars_option)
                .map_or(1_000, |o| o.credits);
            GiveawayPrizeKind::Stars { stars }
        } else {
            GiveawayPrizeKind::Premium {
                months: self.months,
            }
        };

        GiveawayDraft {
            boost_peer: self.options.channel_peer_id,
            prize,
            quantity: self.quantity,
            additional_peers: Vec::new(),
            countries_iso2,
            prize_description: self.prize_description.clone(),
            until_date: now.saturating_add(self.duration_days.clamp(1, 7) * 86_400),
            only_new_subscribers: self.only_new_subscribers,
            winners_are_visible: self.winners_are_visible,
            prepaid_id: None,
        }
    }

    /// Renders the Create Giveaway / Launch Prepaid Giveaway panel.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        channel_name: &str,
        now: i32,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        if !self.loaded
            && let Err(err) = self.reload(channel_name, backend)
        {
            toast = Some(err);
        }

        Frame::NONE
            .fill(palette.window_bg)
            .stroke(Stroke::new(1.2, PREMIUM_PURPLE))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!("Create Giveaway — {}", self.options.channel_title),
                    palette,
                );
                ui.add_space(6.0);

                // Prepaid giveaways quick-launch selector
                if !self.options.prepaid_giveaways.is_empty() {
                    ui.label(
                        egui::RichText::new("Prepaid Giveaways Ready to Launch:")
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                        let none_sel = self.selected_prepaid_id.is_none();
                        if pill_button(
                            ui,
                            "New Giveaway",
                            if none_sel {
                                palette.window_bg_active
                            } else {
                                palette.window_bg_over
                            },
                            if none_sel {
                                palette.window_fg_active
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.selected_prepaid_id = None;
                        }
                        for pre in self.options.prepaid_giveaways.clone() {
                            let active = self.selected_prepaid_id == Some(pre.id);
                            let lbl = if pre.credits > 0 {
                                format!(
                                    "Prepaid #{} ({} Stars • {} winners)",
                                    pre.id,
                                    format_stars_count(pre.credits as i64),
                                    pre.quantity
                                )
                            } else {
                                format!(
                                    "Prepaid #{} ({}×{}m Premium)",
                                    pre.id, pre.quantity, pre.months
                                )
                            };
                            if pill_button(
                                ui,
                                &lbl,
                                if active {
                                    SUCCESS_GREEN
                                } else {
                                    palette.window_bg_over
                                },
                                if active {
                                    Color32::WHITE
                                } else {
                                    palette.window_fg
                                },
                                None,
                            )
                            .clicked()
                            {
                                self.selected_prepaid_id = Some(pre.id);
                            }
                        }
                    });
                    ui.add_space(6.0);
                }

                if self.selected_prepaid_id.is_none() {
                    // Prize type toggle: Premium vs Stars
                    ui.horizontal(|ui| {
                        if pill_button(
                            ui,
                            "Telegram Premium",
                            if !self.is_stars_mode {
                                PREMIUM_PURPLE
                            } else {
                                palette.window_bg_over
                            },
                            if !self.is_stars_mode {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.is_stars_mode = false;
                        }
                        if pill_button(
                            ui,
                            "Telegram Stars",
                            if self.is_stars_mode {
                                STAR_GOLD
                            } else {
                                palette.window_bg_over
                            },
                            if self.is_stars_mode {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            None,
                        )
                        .clicked()
                        {
                            self.is_stars_mode = true;
                        }
                    });
                    ui.add_space(6.0);

                    if !self.is_stars_mode {
                        let presets = if self.options.premium_catalog.available_presets.is_empty() {
                            vec![1, 5, 10]
                        } else {
                            self.options.premium_catalog.available_presets.clone()
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("Winners:")
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            for qty in presets {
                                let active = self.quantity == qty;
                                if pill_button(
                                    ui,
                                    &format!("{qty}"),
                                    if active {
                                        palette.window_bg_active
                                    } else {
                                        palette.window_bg_over
                                    },
                                    if active {
                                        palette.window_fg_active
                                    } else {
                                        palette.window_fg
                                    },
                                    None,
                                )
                                .clicked()
                                {
                                    self.quantity = qty;
                                }
                            }
                        });
                        ui.add_space(4.0);

                        let price_opts = self
                            .options
                            .premium_catalog
                            .options_for_giveaway(self.quantity);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("Duration:")
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            for opt in price_opts {
                                let active = self.months == opt.months;
                                let lbl = format!("{} mo ({})", opt.months, opt.total);
                                if pill_button(
                                    ui,
                                    &lbl,
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
                                    None,
                                )
                                .clicked()
                                {
                                    self.months = opt.months;
                                }
                            }
                        });
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            for (idx, opt) in self.options.stars_options.iter().enumerate() {
                                let active = self.selected_stars_option == idx;
                                let lbl = format!(
                                    "{} Stars (+{} boosts)",
                                    format_stars_count(opt.credits as i64),
                                    opt.yearly_boosts
                                );
                                if pill_button(
                                    ui,
                                    &lbl,
                                    if active {
                                        STAR_GOLD
                                    } else {
                                        palette.window_bg_over
                                    },
                                    if active {
                                        Color32::WHITE
                                    } else {
                                        palette.window_fg
                                    },
                                    None,
                                )
                                .clicked()
                                {
                                    self.selected_stars_option = idx;
                                    if let Some(w) = opt.winners.first() {
                                        self.quantity = w.users;
                                    }
                                }
                            }
                        });
                    }
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let countries_lbl = ui.label(
                        egui::RichText::new("Countries (ISO2, comma-sep):")
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.countries_input)
                            .desired_width(140.0)
                            .hint_text("Worldwide"),
                    )
                    .labelled_by(countries_lbl.id);
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let prize_lbl = ui.label(
                        egui::RichText::new(format!(
                            "Extra Prize ({}/{GIVEAWAY_ADDITIONAL_PRIZE_MAX_LEN}):",
                            self.prize_description.chars().count()
                        ))
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.prize_description)
                            .desired_width(180.0)
                            .hint_text("Optional custom prize..."),
                    )
                    .labelled_by(prize_lbl.id);
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.only_new_subscribers, "Only new subscribers");
                    ui.checkbox(&mut self.winners_are_visible, "Show winners publicly");
                });

                let draft = self.build_draft(now);
                let stars_boosts = self
                    .options
                    .stars_options
                    .get(self.selected_stars_option)
                    .map_or(0, |o| o.yearly_boosts);
                let expected_boosts = draft.expected_boosts(&self.options.limits, stars_boosts);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Expected channel boosts: +{expected_boosts}  •  Draw date: {}",
                        format_unix_date(draft.until_date)
                    ))
                    .size(11.5)
                    .strong()
                    .color(SUCCESS_GREEN),
                );

                ui.add_space(8.0);
                let submit_label = if draft.prepaid_id.is_some() {
                    tr::lng_giveaway_start()
                } else {
                    "Prepare Giveaway Invoice".to_owned()
                };
                if !self.pending_confirm {
                    if primary_button(ui, &submit_label, PREMIUM_PURPLE, Color32::WHITE, 36.0)
                        .clicked()
                    {
                        self.pending_confirm = true;
                    }
                } else {
                    Frame::NONE
                        .fill(with_alpha(PREMIUM_PURPLE, 0.08))
                        .stroke(Stroke::new(1.0, PREMIUM_PURPLE))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            let confirm_copy = if draft.prepaid_id.is_some() {
                                tr::lng_giveaway_start_sure()
                            } else {
                                format!(
                                    "Request a read-only giveaway payment form for {} winner(s)? No charge is made automatically.",
                                    draft.quantity
                                )
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(confirm_copy)
                                        .size(12.0)
                                        .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                let confirm_btn = if draft.prepaid_id.is_some() {
                                    "Confirm Start Giveaway"
                                } else {
                                    "Confirm & Prepare Invoice"
                                };
                                if pill_button(
                                    ui,
                                    confirm_btn,
                                    PREMIUM_PURPLE,
                                    Color32::WHITE,
                                    None,
                                )
                                .clicked()
                                {
                                    self.pending_confirm = false;
                                    match backend.create_or_launch_giveaway(&draft) {
                                        Ok(msg) => {
                                            toast = Some(msg);
                                            self.selected_prepaid_id = None;
                                            let _ = self.reload(channel_name, backend);
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
                                    self.pending_confirm = false;
                                }
                            });
                        });
                }
            });

        toast
    }
}

/// Interactive Replace / Reassign Boost panel (`boxes/peers/replace_boost_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct ReplaceBoostPanel {
    /// Loaded boost slots partitioned for the target channel.
    pub bundle: ReplaceBoostBundle,
    /// Selected slot IDs to apply or reassign.
    pub selected_slots: Vec<i32>,
    /// Whether slot data has been loaded.
    pub loaded: bool,
}

impl ReplaceBoostPanel {
    /// Reloads boost slots for `channel_name`.
    pub fn reload(
        &mut self,
        channel_name: &str,
        backend: &dyn StarGiftsMarketplaceBackend,
    ) -> Result<(), String> {
        self.bundle = backend.load_boost_slots_for_channel(channel_name)?;
        self.selected_slots.clear();
        self.loaded = true;
        Ok(())
    }

    fn peer_name_for(&self, peer_id: tg_data::PeerId) -> String {
        self.bundle
            .peer_names
            .iter()
            .find(|(id, _)| *id == peer_id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| format!("Channel #{}", peer_id.0))
    }

    /// Renders the Replace Boost panel.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        channel_name: &str,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        if !self.loaded
            && let Err(err) = self.reload(channel_name, backend)
        {
            toast = Some(err);
        }

        Frame::NONE
            .fill(palette.window_bg)
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!("Manage / Replace Boosts — {}", self.bundle.channel_title),
                    palette,
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Already boosting here: {}  •  Free slots: {}  •  Other channels: {}",
                        self.bundle.slots.already.len(),
                        self.bundle.slots.free.len(),
                        self.bundle.slots.other.len()
                    ))
                    .size(11.5)
                    .color(palette.window_sub_text_fg),
                );
                ui.add_space(6.0);

                // Free slots
                for &slot_id in &self.bundle.slots.free.clone() {
                    let mut checked = self.selected_slots.contains(&slot_id);
                    if ui
                        .checkbox(
                            &mut checked,
                            format!("Slot #{slot_id} — Free (ready to apply)"),
                        )
                        .changed()
                    {
                        if checked {
                            self.selected_slots.push(slot_id);
                        } else {
                            self.selected_slots.retain(|&s| s != slot_id);
                        }
                    }
                }

                // Taken slots on other channels
                for taken in self.bundle.slots.other.clone() {
                    let waiting = taken.is_waiting(self.bundle.now);
                    let peer_label = self.peer_name_for(taken.peer_id);
                    let status = if waiting {
                        format!(
                            "Cooldown {}",
                            format_boost_cooldown(taken.remaining_cooldown_secs(self.bundle.now))
                        )
                    } else {
                        format!(
                            "Ready to move (expires {})",
                            format_unix_date(taken.expires)
                        )
                    };
                    ui.horizontal(|ui| {
                        let mut checked = self.selected_slots.contains(&taken.id);
                        let resp = ui.add_enabled(
                            !waiting,
                            egui::Checkbox::new(
                                &mut checked,
                                format!("Slot #{} — {} ({})", taken.id, peer_label, status),
                            ),
                        );
                        if resp.changed() {
                            if checked {
                                self.selected_slots.push(taken.id);
                            } else {
                                self.selected_slots.retain(|&s| s != taken.id);
                            }
                        }
                    });
                    thin_divider(ui, palette);
                }

                ui.add_space(8.0);
                let btn_label = if self.selected_slots.is_empty() {
                    "Select Boost Slot(s) to Apply or Reassign".to_owned()
                } else {
                    format!(
                        "Apply / Replace {} Boost Slot(s)",
                        self.selected_slots.len()
                    )
                };
                if primary_button(
                    ui,
                    &btn_label,
                    if self.selected_slots.is_empty() {
                        with_alpha(PREMIUM_PURPLE, 0.4)
                    } else {
                        PREMIUM_PURPLE
                    },
                    Color32::WHITE,
                    36.0,
                )
                .clicked()
                    && !self.selected_slots.is_empty()
                {
                    match backend.apply_boost_slots(channel_name, &self.selected_slots) {
                        Ok(_) => {
                            toast = Some(format!(
                                "Applied {} boost slot(s) to {}.",
                                self.selected_slots.len(),
                                self.bundle.channel_title
                            ));
                            let _ = self.reload(channel_name, backend);
                        }
                        Err(err) => toast = Some(err),
                    }
                }
            });

        toast
    }
}
