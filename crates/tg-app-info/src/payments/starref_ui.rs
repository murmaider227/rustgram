//! Telegram Stars Affiliate Program (StarRef) Setup & Join UI screens.
//!
//! Port origin: `info/bot/starref/info_bot_starref_common.cpp`,
//! `info/bot/starref/info_bot_starref_setup_widget.cpp`, and
//! `info/bot/starref/info_bot_starref_join_widget.cpp`.

use egui::{Color32, CornerRadius, Frame, Margin, Stroke, Ui, Vec2};
use tg_payments::starref::{
    STARREF_COMMISSION_DEFAULT, STARREF_COMMISSION_MAX_FALLBACK, STARREF_COMMISSION_MIN_FALLBACK,
    STARREF_DURATION_DEFAULT_MONTHS, SuggestedStarRefSort, format_commission,
    format_program_duration,
};

use crate::payments::credits::format_stars_count;
use crate::payments::marketplace_backend::{StarGiftsMarketplaceBackend, StarRefOverviewBundle};
use crate::payments::ui_helpers::{
    PREMIUM_PURPLE, STAR_GOLD, SUCCESS_GREEN, SUCCESS_SURFACE, format_unix_date, pill_button,
    primary_button, section_divider, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Active tab inside the Stars Affiliate Program screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StarRefTab {
    /// Browse suggested programs and manage joined affiliate links (`info_bot_starref_join_widget.cpp`).
    #[default]
    JoinPrograms,
    /// Configure or terminate an owned bot's affiliate program (`info_bot_starref_setup_widget.cpp`).
    SetupBotProgram,
}

/// Interactive Stars Affiliate Program view-model (`info/bot/starref/*`).
#[derive(Debug, Clone)]
pub struct StarRefView {
    /// Active sub-tab.
    pub active_tab: StarRefTab,
    /// Loaded connected + suggested + managed bot bundle.
    pub bundle: StarRefOverviewBundle,
    /// Selected sort order for suggested affiliate programs.
    pub sort: SuggestedStarRefSort,
    /// Draft commission permille in Setup tab (`1..=900`).
    pub draft_commission_permille: i32,
    /// Draft attribution duration in months (`0` = forever).
    pub draft_duration_months: i32,
    /// Whether initial data has been loaded from the backend.
    pub loaded: bool,
    /// Peer whose affiliate programs are listed (`""` = the signed-in user,
    /// a bot username for `BotStarRef::Join::Make(peer)`).
    pub peer_name: String,
}

impl Default for StarRefView {
    fn default() -> Self {
        Self {
            active_tab: StarRefTab::JoinPrograms,
            bundle: StarRefOverviewBundle::default(),
            sort: SuggestedStarRefSort::Profitability,
            draft_commission_permille: STARREF_COMMISSION_DEFAULT,
            draft_duration_months: STARREF_DURATION_DEFAULT_MONTHS,
            loaded: false,
            peer_name: String::new(),
        }
    }
}

impl StarRefView {
    /// Creates a not-yet-loaded view listing affiliate programs for `peer_name`
    /// (`BotStarRef::Join::Make(peer)` in `info_bot_starref_join_widget.cpp`).
    #[must_use]
    pub fn for_peer(peer_name: impl Into<String>) -> Self {
        Self {
            peer_name: peer_name.into(),
            ..Self::default()
        }
    }

    /// Reloads connected, suggested, and managed bot affiliate state.
    pub fn reload(&mut self, backend: &dyn StarGiftsMarketplaceBackend) -> Result<(), String> {
        let bundle = backend.load_starref_overview(&self.peer_name, self.sort)?;
        if let Some(ref prog) = bundle.managed_program
            && prog.commission_permille > 0
        {
            self.draft_commission_permille = prog.commission_permille;
            self.draft_duration_months = prog.duration_months;
        }
        self.bundle = bundle;
        self.loaded = true;
        Ok(())
    }

    /// Renders the Stars Affiliate Program screen.
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
            .inner_margin(Margin::symmetric(18, 12))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Telegram Stars Affiliate Programs")
                        .size(16.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Earn Stars by sharing referral links for bots & Mini Apps  •  Account: {}",
                        self.bundle.peer_name
                    ))
                    .size(11.5)
                    .color(palette.window_sub_text_fg),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (StarRefTab::JoinPrograms, "Join & My Programs"),
                        (StarRefTab::SetupBotProgram, "Bot Program Setup"),
                    ] {
                        let active = self.active_tab == tab;
                        if pill_button(
                            ui,
                            label,
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
                            self.active_tab = tab;
                        }
                    }
                });
            });

        match self.active_tab {
            StarRefTab::JoinPrograms => {
                if let Some(msg) = self.show_join_tab(ui, backend, palette) {
                    toast = Some(msg);
                }
            }
            StarRefTab::SetupBotProgram => {
                if let Some(msg) = self.show_setup_tab(ui, backend, palette) {
                    toast = Some(msg);
                }
            }
        }

        toast
    }

    fn show_join_tab(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        // Connected affiliate bots
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 4))
            .show(ui, |ui| {
                subsection_title(
                    ui,
                    &format!("My Connected Programs ({})", self.bundle.connected.len()),
                    palette,
                );
                ui.add_space(6.0);
                if self.bundle.connected.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "You haven't joined any affiliate programs yet. Pick one below!",
                        )
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                    );
                }
                for entry in self.bundle.connected.clone() {
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} (@{})",
                                            entry.bot_name, entry.bot_username
                                        ))
                                        .size(13.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} commission  •  {}  •  {} users  •  {} Stars earned",
                                            format_commission(entry.info.commission_permille),
                                            format_program_duration(entry.info.duration_months),
                                            entry.info.participants,
                                            format_stars_count(entry.info.revenue),
                                        ))
                                        .size(11.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(&entry.info.url)
                                            .size(11.0)
                                            .color(palette.window_active_text_fg),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let toggle_revoked = !entry.info.revoked;
                                        let lbl = if entry.info.revoked {
                                            "Restore Link"
                                        } else {
                                            "Revoke Link"
                                        };
                                        let bg = if entry.info.revoked {
                                            SUCCESS_SURFACE
                                        } else {
                                            with_alpha(palette.attention_fg, 0.14)
                                        };
                                        let fg = if entry.info.revoked {
                                            SUCCESS_GREEN
                                        } else {
                                            palette.attention_fg
                                        };
                                        if pill_button(ui, lbl, bg, fg, None).clicked() {
                                            match backend.edit_connected_starref_bot(
                                                &self.bundle.peer_name,
                                                &entry.info.url,
                                                toggle_revoked,
                                            ) {
                                                Ok(_) => {
                                                    toast = Some(if toggle_revoked {
                                                        format!(
                                                            "Revoked affiliate link for @{}.",
                                                            entry.bot_username
                                                        )
                                                    } else {
                                                        format!(
                                                            "Restored affiliate link for @{}.",
                                                            entry.bot_username
                                                        )
                                                    });
                                                    let _ = self.reload(backend);
                                                }
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

        section_divider(ui, palette);

        // Suggested programs with sort bar
        Frame::NONE
            .inner_margin(Margin::symmetric(18, 4))
            .show(ui, |ui| {
                subsection_title(ui, "Suggested Affiliate Programs", palette);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    for (sort, label) in [
                        (SuggestedStarRefSort::Profitability, "Profitability"),
                        (SuggestedStarRefSort::Revenue, "Revenue"),
                        (SuggestedStarRefSort::Date, "Newest"),
                    ] {
                        let active = self.sort == sort;
                        if pill_button(
                            ui,
                            label,
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
                            && !active
                        {
                            self.sort = sort;
                            if let Err(err) = self.reload(backend) {
                                toast = Some(err);
                            }
                        }
                    }
                });
                ui.add_space(6.0);

                for sugg in self.bundle.suggested.clone() {
                    let already_joined = self
                        .bundle
                        .connected
                        .iter()
                        .any(|c| c.bot_username == sugg.bot_username && !c.info.revoked);
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} (@{})",
                                    sugg.bot_name, sugg.bot_username
                                ))
                                .size(13.0)
                                .strong()
                                .color(palette.window_bold_fg),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} commission  •  {}  •  ~{} Stars/user/day",
                                    sugg.program.commission_text(),
                                    sugg.program.duration_text(),
                                    sugg.program.daily_revenue_per_user.whole()
                                ))
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if already_joined {
                                ui.label(
                                    egui::RichText::new("Joined")
                                        .size(12.0)
                                        .strong()
                                        .color(SUCCESS_GREEN),
                                );
                            } else if pill_button(
                                ui,
                                "Join & Get Link",
                                PREMIUM_PURPLE,
                                Color32::WHITE,
                                None,
                            )
                            .clicked()
                            {
                                match backend
                                    .connect_starref_bot(&self.bundle.peer_name, &sugg.bot_username)
                                {
                                    Ok(conn) => {
                                        toast = Some(format!(
                                            "Connected @{}: {}",
                                            sugg.bot_username, conn.url
                                        ));
                                        let _ = self.reload(backend);
                                    }
                                    Err(err) => toast = Some(err),
                                }
                            }
                        });
                    });
                    thin_divider(ui, palette);
                }
            });

        toast
    }

    fn show_setup_tab(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn StarGiftsMarketplaceBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let is_active = self
            .bundle
            .managed_program
            .as_ref()
            .is_some_and(|p| p.is_active());

        Frame::NONE
            .inner_margin(Margin::symmetric(18, 6))
            .show(ui, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.2, STAR_GOLD))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        subsection_title(
                            ui,
                            &format!(
                                "Affiliate Program Setup — {} (@{})",
                                self.bundle.managed_bot_name, self.bundle.managed_bot_username
                            ),
                            palette,
                        );
                        ui.add_space(6.0);

                        if let Some(ref prog) = self.bundle.managed_program {
                            let status = if prog.is_active() {
                                format!(
                                    "Active: {} commission for {}",
                                    prog.commission_text(),
                                    prog.duration_text()
                                )
                            } else if prog.end_date > 0 {
                                format!(
                                    "Ending on {} (no new affiliates)",
                                    format_unix_date(prog.end_date)
                                )
                            } else {
                                "No active program configured.".to_owned()
                            };
                            ui.label(egui::RichText::new(status).size(12.5).strong().color(
                                if prog.is_active() {
                                    SUCCESS_GREEN
                                } else {
                                    palette.window_sub_text_fg
                                },
                            ));
                            ui.add_space(6.0);
                        }

                        // Commission slider (permille)
                        ui.label(
                            egui::RichText::new(format!(
                                "Affiliate Commission: {} ({} permille)",
                                format_commission(self.draft_commission_permille),
                                self.draft_commission_permille
                            ))
                            .size(12.0)
                            .strong()
                            .color(palette.window_bold_fg),
                        );
                        ui.add(egui::Slider::new(
                            &mut self.draft_commission_permille,
                            STARREF_COMMISSION_MIN_FALLBACK..=STARREF_COMMISSION_MAX_FALLBACK,
                        ));

                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Attribution Duration: {}",
                                format_program_duration(self.draft_duration_months)
                            ))
                            .size(12.0)
                            .strong()
                            .color(palette.window_bold_fg),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                            for months in [1, 3, 6, 12, 24, 0] {
                                let active = self.draft_duration_months == months;
                                if pill_button(
                                    ui,
                                    &format_program_duration(months),
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
                                    self.draft_duration_months = months;
                                }
                            }
                        });

                        ui.add_space(10.0);
                        let action_lbl = if is_active {
                            "Update Affiliate Program"
                        } else {
                            "Start Affiliate Program"
                        };
                        if primary_button(ui, action_lbl, PREMIUM_PURPLE, Color32::WHITE, 36.0)
                            .clicked()
                        {
                            match backend.update_starref_program(
                                &self.bundle.managed_bot_username,
                                self.draft_commission_permille,
                                self.draft_duration_months,
                            ) {
                                Ok(updated) => {
                                    toast = Some(format!(
                                        "Saved affiliate program: {} for {}.",
                                        updated.commission_text(),
                                        updated.duration_text()
                                    ));
                                    let _ = self.reload(backend);
                                }
                                Err(err) => toast = Some(err),
                            }
                        }

                        if is_active {
                            ui.add_space(6.0);
                            if pill_button(
                                ui,
                                "End Affiliate Program",
                                with_alpha(palette.attention_fg, 0.14),
                                palette.attention_fg,
                                None,
                            )
                            .clicked()
                            {
                                match backend.update_starref_program(
                                    &self.bundle.managed_bot_username,
                                    0,
                                    0,
                                ) {
                                    Ok(_) => {
                                        toast =
                                            Some("Affiliate program scheduled to end.".to_owned());
                                        let _ = self.reload(backend);
                                    }
                                    Err(err) => toast = Some(err),
                                }
                            }
                        }
                    });
            });

        toast
    }
}
