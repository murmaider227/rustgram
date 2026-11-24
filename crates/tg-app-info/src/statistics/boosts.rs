//! Channel and Supergroup Boosts screen (overview, level progress, giveaways, boosts list, boost link).
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/channel_statistics/boosts/info_boosts_inner_widget.{h,cpp}`
//! - `Telegram/SourceFiles/info/channel_statistics/boosts/giveaway/boost_badge.{h,cpp}`
//! - `Telegram/SourceFiles/info/channel_statistics/boosts/giveaway/giveaway_type_row.{h,cpp}`
//! - `Telegram/SourceFiles/info/statistics/info_statistics_list_controllers.cpp` (`BoostRow`)

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_stats::{
    Boost, BoostPrepaidGiveaway, BoostsOverview, ChartPalette, boosts_overview_block,
    lang_day_month,
};
use tg_ui::tg_style::st;

use crate::statistics::backend::{BoostStatusBundle, PeerDisplayInfo, StatsBackend};
use crate::statistics::chart_widget::{to_color32, to_color32_alpha};
use crate::statistics::sections::{
    paint_avatar, render_overview_block, render_section_divider, render_section_header,
    render_show_more_button,
};
use crate::statistics::{paint_stats_icon, strings};

/// Active tab inside the Boosts list section (`Boosts` vs `Gifts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoostsListTab {
    /// All channel/group boosts.
    #[default]
    Boosts,
    /// Gift and giveaway boosts only.
    Gifts,
}

/// Computes normalized level fill progress in `0.0..=1.0` between current and next boost levels.
#[must_use]
pub fn compute_boost_level_progress(overview: &BoostsOverview) -> f32 {
    if overview.next_level_boost_count <= overview.current_level_boost_count {
        return 1.0;
    }
    let span = (overview.next_level_boost_count - overview.current_level_boost_count) as f32;
    let done = (overview.boost_count - overview.current_level_boost_count).max(0) as f32;
    (done / span).clamp(0.0, 1.0)
}

/// Formats a boost row's display title matching `BoostRow::generateName`
/// (`info_statistics_list_controllers.cpp:571-582`).
#[must_use]
pub fn format_boost_title(boost: &Boost, user: Option<&PeerDisplayInfo>) -> String {
    if let Some(u) = user {
        u.title.clone()
    } else if boost.credits > 0 {
        tr::lng_credits_summary_options_credits(boost.credits as i64)
    } else if boost.is_unclaimed {
        tr::lng_boosts_list_unclaimed()
    } else {
        tr::lng_boosts_list_pending()
    }
}

/// Formats a boost row's status subtitle matching `BoostRow::init`
/// (`info_statistics_list_controllers.cpp:554-564`).
#[must_use]
pub fn format_boost_status(boost: &Boost, has_user: bool) -> String {
    if has_user || boost.credits > 0 {
        tr::lng_boosts_list_status(&lang_day_month(boost.expires_at))
    } else {
        let months_label = tr::lng_months(boost.expires_after_months);
        format!("{months_label} • {}", lang_day_month(boost.date))
    }
}

/// Formats a prepaid giveaway row's title and subtitle (`info_boosts_inner_widget.cpp:353-387`).
#[must_use]
pub fn format_prepaid_giveaway_labels(
    giveaway: &BoostPrepaidGiveaway,
    multiplier: i32,
) -> (String, String, i32) {
    let boosts = if giveaway.boosts > 0 {
        giveaway.boosts
    } else {
        giveaway.quantity * multiplier
    };
    if giveaway.credits > 0 {
        let title = tr::lng_boosts_prepaid_giveaway_single();
        let amount = tr::lng_credits_summary_options_credits(giveaway.credits as i64);
        let sub = tr::lng_boosts_prepaid_giveaway_credits_status(giveaway.quantity, &amount);
        (title, sub, boosts)
    } else {
        let title = tr::lng_boosts_prepaid_giveaway_quantity(giveaway.quantity);
        let duration = tr::lng_boosts_prepaid_giveaway_moths(giveaway.months);
        let sub = tr::lng_boosts_prepaid_giveaway_status(giveaway.quantity, &duration);
        (title, sub, boosts)
    }
}

/// Stateful Boosts screen (`Info::Boosts::InnerWidget`).
#[derive(Debug, Clone)]
pub struct BoostsView {
    /// Loaded boost status, slices, prepaid giveaways, and share link.
    pub bundle: BoostStatusBundle,
    /// Active list tab (`Boosts` or `Gifts`).
    pub active_tab: BoostsListTab,
    /// Status toast/feedback message (e.g. link copied or read-only giveaway notice).
    pub notice: Option<String>,
}

impl BoostsView {
    /// Creates a new boosts screen view from `bundle`.
    #[must_use]
    pub fn new(bundle: BoostStatusBundle) -> Self {
        let has_boosts = bundle.status.first_slice_boosts.multiplied_total > 0;
        let has_gifts = bundle.status.first_slice_gifts.multiplied_total > 0;
        let active_tab = if !has_boosts && has_gifts {
            BoostsListTab::Gifts
        } else {
            BoostsListTab::Boosts
        };
        Self {
            bundle,
            active_tab,
            notice: None,
        }
    }

    /// Requests the next page of boosts or gifts from `backend` and appends it in place.
    pub fn load_more(&mut self, backend: &dyn StatsBackend) {
        let slice = match self.active_tab {
            BoostsListTab::Boosts => &mut self.bundle.status.first_slice_boosts,
            BoostsListTab::Gifts => &mut self.bundle.status.first_slice_gifts,
        };
        if slice.all_loaded {
            return;
        }
        if let Ok(next) = backend.load_more_boosts(&slice.token) {
            slice.list.extend(next.list);
            slice.all_loaded = next.all_loaded;
            slice.token = next.token;
        }
    }

    /// Renders the complete Boosts screen and returns an optional toast message.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
    ) -> Option<String> {
        let mut toast = None;

        // 1. Boost Level Progress Bar (`Ui::FillBoostLimit`).
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 14))
            .show(ui, |ui| {
                render_boost_level_bar(ui, &self.bundle.status.overview, palette);
            });
        render_section_divider(ui, palette);

        // 2. 2x2 Boosts Overview (`boosts_overview_block`).
        let overview_block = boosts_overview_block(&self.bundle.status.overview, palette);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                render_overview_block(ui, &overview_block, palette);
            });
        render_section_divider(ui, palette);

        // 3. Prepaid Giveaways list (when present).
        if !self.bundle.status.prepaid_giveaway.is_empty() {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    render_section_header(
                        ui,
                        &tr::lng_boosts_prepaid_giveaway_title(),
                        "",
                        palette,
                    );
                    ui.add_space(6.0);
                    let mult = self.bundle.giveaway_boosts_per_premium;
                    for g in &self.bundle.status.prepaid_giveaway {
                        let (title, sub, boosts) = format_prepaid_giveaway_labels(g, mult);
                        if render_giveaway_row(ui, &title, &sub, boosts, g.credits > 0, palette) {
                            toast = Some(strings::GIVEAWAY_READ_ONLY_NOTICE.to_owned());
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(tr::lng_boosts_prepaid_giveaway_title_subtext())
                            .size(11.5)
                            .color(to_color32(palette.window_sub_text_fg)),
                    );
                });
            render_section_divider(ui, palette);
        }

        // 4. Boosts / Gifts tabbed list.
        let boosts_total = self.bundle.status.first_slice_boosts.multiplied_total;
        let gifts_total = self.bundle.status.first_slice_gifts.multiplied_total;
        if boosts_total > 0 || gifts_total > 0 {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    if boosts_total > 0 && gifts_total > 0 {
                        render_boosts_tab_bar(
                            ui,
                            &mut self.active_tab,
                            boosts_total,
                            gifts_total,
                            palette,
                        );
                        ui.add_space(6.0);
                    } else {
                        let heading = if boosts_total > 0 {
                            tr::lng_boosts_list_title(boosts_total)
                        } else {
                            tr::lng_boosts_list_tab_gifts(gifts_total)
                        };
                        render_section_header(ui, &heading, "", palette);
                        ui.add_space(6.0);
                    }

                    let (items, all_loaded, remaining_total) = match self.active_tab {
                        BoostsListTab::Boosts => (
                            self.bundle.status.first_slice_boosts.list.clone(),
                            self.bundle.status.first_slice_boosts.all_loaded,
                            (boosts_total
                                - self.bundle.status.first_slice_boosts.list.len() as i32)
                                .max(1),
                        ),
                        BoostsListTab::Gifts => (
                            self.bundle.status.first_slice_gifts.list.clone(),
                            self.bundle.status.first_slice_gifts.all_loaded,
                            (gifts_total - self.bundle.status.first_slice_gifts.list.len() as i32)
                                .max(1),
                        ),
                    };

                    for boost in &items {
                        let user = if boost.user_id != 0 {
                            self.bundle.users.get(&boost.user_id)
                        } else {
                            None
                        };
                        if render_boost_row(ui, boost, user, palette) {
                            let label = if !boost.gift_code_link.link.is_empty() {
                                format!(
                                    "{}{}",
                                    strings::GIFT_LINK_PREFIX,
                                    boost.gift_code_link.link
                                )
                            } else if boost.is_unclaimed {
                                strings::GIVEAWAY_UNCLAIMED_NOTICE.to_owned()
                            } else {
                                format!("{}{}", strings::BOOST_LABEL_PREFIX, boost.id)
                            };
                            toast = Some(label);
                        }
                    }

                    if !all_loaded {
                        ui.add_space(4.0);
                        let more_label = match self.active_tab {
                            BoostsListTab::Boosts => {
                                tr::lng_boosts_show_more_boosts(remaining_total)
                            }
                            BoostsListTab::Gifts => tr::lng_boosts_show_more_gifts(remaining_total),
                        };
                        if render_show_more_button(ui, &more_label, palette) {
                            self.load_more(backend);
                        }
                    }
                });
            render_section_divider(ui, palette);
        }

        // 5. Shareable Boost Link section (`FillShareLink`).
        if !self.bundle.status.link.is_empty() {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    render_section_header(ui, &tr::lng_boosts_link_title(), "", palette);
                    ui.add_space(8.0);

                    let display_link = self
                        .bundle
                        .status
                        .link
                        .strip_prefix("https://")
                        .unwrap_or(&self.bundle.status.link);
                    let avail_w = ui.available_width().max(200.0);
                    let (link_rect, link_resp) =
                        ui.allocate_exact_size(Vec2::new(avail_w, 38.0), Sense::click());
                    ui.painter().rect_filled(
                        link_rect,
                        CornerRadius::same(8),
                        to_color32(palette.box_divider_bg),
                    );
                    ui.painter().text(
                        link_rect.center(),
                        Align2::CENTER_CENTER,
                        display_link,
                        FontId::proportional(13.5),
                        to_color32(palette.box_text_fg),
                    );
                    if link_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    ui.add_space(8.0);
                    let btn_w = (avail_w - 10.0) / 2.0;
                    let mut copy_clicked = link_resp.clicked();
                    let mut share_clicked = false;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        if render_pill_action_button(
                            ui,
                            &tr::lng_group_invite_context_copy(),
                            btn_w,
                            to_color32(palette.window_active_text_fg),
                            Color32::WHITE,
                        ) {
                            copy_clicked = true;
                        }
                        if render_pill_action_button(
                            ui,
                            &tr::lng_group_invite_context_share(),
                            btn_w,
                            to_color32_alpha(palette.window_active_text_fg, 0.14),
                            to_color32(palette.window_active_text_fg),
                        ) {
                            share_clicked = true;
                        }
                    });

                    if copy_clicked {
                        ui.ctx().copy_text(self.bundle.status.link.clone());
                        toast = Some(tr::lng_channel_public_link_copied());
                    } else if share_clicked {
                        toast = Some(format!(
                            "{}{}{}",
                            strings::SHARE_LINK_READ_ONLY_PREFIX,
                            self.bundle.status.link,
                            strings::SHARE_LINK_READ_ONLY_SUFFIX
                        ));
                    }

                    ui.add_space(8.0);
                    let subtext = if self.bundle.status.overview.group {
                        tr::lng_boosts_link_subtext_group()
                    } else {
                        tr::lng_boosts_link_subtext()
                    };
                    ui.label(
                        egui::RichText::new(subtext)
                            .size(11.5)
                            .color(to_color32(palette.window_sub_text_fg)),
                    );
                });
        }

        // 6. Read-only "Get Boosts via Gifts" row (`FillGetBoostsButton`).
        render_section_divider(ui, palette);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 8))
            .show(ui, |ui| {
                let avail_w = ui.available_width().max(200.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 38.0), Sense::click());
                let accent = to_color32(palette.window_active_text_fg);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(6),
                        to_color32_alpha(palette.window_active_text_fg, 0.08),
                    );
                }
                let icon_rect = Rect::from_center_size(
                    Pos2::new(rect.left() + 18.0, rect.center().y),
                    Vec2::splat(18.0),
                );
                paint_stats_icon(
                    ui.painter(),
                    icon_rect,
                    st::boosts_list_gift_mini_icon,
                    palette,
                    Some(accent),
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 34.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    tr::lng_boosts_get_boosts(),
                    FontId::proportional(13.5),
                    accent,
                );
                if resp.clicked() {
                    toast = Some(strings::GIVEAWAY_CREATE_READ_ONLY_NOTICE.to_owned());
                }
            });

        if let Some(ref msg) = toast {
            self.notice = Some(msg.clone());
        }
        toast
    }
}

fn render_boost_level_bar(ui: &mut Ui, overview: &BoostsOverview, palette: &ChartPalette) {
    let progress = compute_boost_level_progress(overview);
    let avail_w = ui.available_width().max(220.0);

    // Level labels header above bar.
    let (hdr_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 22.0), Sense::hover());
    let painter = ui.painter();
    let accent_purple = Color32::from_rgb(0x88, 0x62, 0xE6);
    let level_word = tr::lng_boosts_level();

    painter.text(
        Pos2::new(hdr_rect.left(), hdr_rect.center().y),
        Align2::LEFT_CENTER,
        format!("{level_word} {}", overview.level),
        FontId::proportional(13.5),
        to_color32(palette.box_text_fg),
    );
    let count_label = format!(
        "{} / {}",
        overview.boost_count, overview.next_level_boost_count
    );
    let count_g =
        ui.fonts_mut(|f| f.layout_no_wrap(count_label, FontId::proportional(12.5), accent_purple));
    let total_w = 14.0 + count_g.size().x;
    let start_x = hdr_rect.center().x - total_w * 0.5;
    paint_stats_icon(
        painter,
        Rect::from_center_size(
            Pos2::new(start_x + 6.0, hdr_rect.center().y),
            Vec2::splat(13.0),
        ),
        st::boosts_list_mini_icon,
        palette,
        Some(accent_purple),
    );
    painter.galley(
        Pos2::new(start_x + 14.0, hdr_rect.center().y - count_g.size().y * 0.5),
        count_g,
        accent_purple,
    );
    painter.text(
        Pos2::new(hdr_rect.right(), hdr_rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{level_word} {}", overview.level + 1),
        FontId::proportional(13.5),
        to_color32(palette.window_sub_text_fg),
    );

    ui.add_space(6.0);
    let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 28.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        bar_rect,
        CornerRadius::same(14),
        to_color32(palette.box_divider_bg),
    );
    let fill_w = (bar_rect.width() * progress)
        .max(36.0)
        .min(bar_rect.width());
    let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new(fill_w, bar_rect.height()));
    painter.rect_filled(fill_rect, CornerRadius::same(14), accent_purple);
    let fill_txt = format!("{}", overview.boost_count);
    let fill_g =
        ui.fonts_mut(|f| f.layout_no_wrap(fill_txt, FontId::proportional(12.0), Color32::WHITE));
    let fill_tw = 13.0 + fill_g.size().x;
    let fill_sx = fill_rect.center().x - fill_tw * 0.5;
    paint_stats_icon(
        painter,
        Rect::from_center_size(
            Pos2::new(fill_sx + 5.5, fill_rect.center().y),
            Vec2::splat(12.0),
        ),
        st::boosts_list_mini_icon,
        palette,
        Some(Color32::WHITE),
    );
    painter.galley(
        Pos2::new(fill_sx + 13.0, fill_rect.center().y - fill_g.size().y * 0.5),
        fill_g,
        Color32::WHITE,
    );
}

fn render_giveaway_row(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    boosts: i32,
    is_credits: bool,
    palette: &ChartPalette,
) -> bool {
    let avail_w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 54.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.box_text_fg, 0.04),
        );
    }

    let painter = ui.painter();
    let av_center = Pos2::new(rect.left() + 22.0, rect.center().y);
    let av_fill = if is_credits {
        Color32::from_rgb(0xEB, 0xA5, 0x2D)
    } else {
        Color32::from_rgb(0x88, 0x62, 0xE6)
    };
    painter.circle_filled(av_center, 19.0, av_fill);
    let av_icon = if is_credits {
        st::dialogs_premium_icon.icon
    } else {
        st::boosts_list_giveaway_mini_icon
    };
    paint_stats_icon(
        painter,
        Rect::from_center_size(av_center, Vec2::splat(20.0)),
        av_icon,
        palette,
        Some(Color32::WHITE),
    );

    painter.text(
        Pos2::new(rect.left() + 52.0, rect.top() + 9.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(13.0),
        to_color32(palette.box_text_fg),
    );
    painter.text(
        Pos2::new(rect.left() + 52.0, rect.top() + 29.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(11.5),
        to_color32(palette.window_sub_text_fg),
    );

    // Boosts pill badge on the right (`CreateBadge` with `premiumButtonBg2`).
    let badge_text = format!("{boosts}");
    let badge_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 56.0, rect.center().y - 10.0),
        Vec2::new(48.0, 20.0),
    );
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(10),
        Color32::from_rgb(0x88, 0x62, 0xE6),
    );
    let badge_g =
        ui.fonts_mut(|f| f.layout_no_wrap(badge_text, FontId::proportional(11.0), Color32::WHITE));
    let badge_tw = 12.0 + badge_g.size().x;
    let badge_sx = badge_rect.center().x - badge_tw * 0.5;
    paint_stats_icon(
        painter,
        Rect::from_center_size(
            Pos2::new(badge_sx + 5.0, badge_rect.center().y),
            Vec2::splat(11.0),
        ),
        st::boosts_list_mini_icon,
        palette,
        Some(Color32::WHITE),
    );
    painter.galley(
        Pos2::new(
            badge_sx + 12.0,
            badge_rect.center().y - badge_g.size().y * 0.5,
        ),
        badge_g,
        Color32::WHITE,
    );

    resp.clicked()
}

fn render_boosts_tab_bar(
    ui: &mut Ui,
    active_tab: &mut BoostsListTab,
    boosts_total: i32,
    gifts_total: i32,
    palette: &ChartPalette,
) {
    let tabs = [
        (
            BoostsListTab::Boosts,
            tr::lng_boosts_list_title(boosts_total),
        ),
        (
            BoostsListTab::Gifts,
            tr::lng_boosts_list_tab_gifts(gifts_total),
        ),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (tab, label) in tabs {
            let selected = *active_tab == tab;
            let text_col = if selected {
                to_color32(palette.window_active_text_fg)
            } else {
                to_color32(palette.window_sub_text_fg)
            };
            let galley = ui.fonts_mut(|f| {
                f.layout_no_wrap(label.clone(), FontId::proportional(13.5), text_col)
            });
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(galley.size().x + 8.0, 28.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                *active_tab = tab;
            }
            ui.painter().galley(
                Pos2::new(rect.left() + 4.0, rect.top() + 4.0),
                galley,
                text_col,
            );
            if selected {
                ui.painter().line_segment(
                    [
                        Pos2::new(rect.left(), rect.bottom() - 1.5),
                        Pos2::new(rect.right(), rect.bottom() - 1.5),
                    ],
                    Stroke::new(2.5, to_color32(palette.window_active_text_fg)),
                );
            }
        }
    });
    let line_rect = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            Pos2::new(line_rect.left(), line_rect.top()),
            Pos2::new(line_rect.right(), line_rect.top()),
        ],
        Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.08)),
    );
}

fn render_boost_row(
    ui: &mut Ui,
    boost: &Boost,
    user: Option<&PeerDisplayInfo>,
    palette: &ChartPalette,
) -> bool {
    let avail_w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 52.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.box_text_fg, 0.04),
        );
    }

    let painter = ui.painter();
    let av_center = Pos2::new(rect.left() + 22.0, rect.center().y);
    if let Some(u) = user {
        paint_avatar(painter, av_center, 19.0, u.avatar_rgb, &u.initials, false);
    } else if boost.credits > 0 {
        painter.circle_filled(av_center, 19.0, Color32::from_rgb(0xEB, 0xA5, 0x2D));
        paint_stats_icon(
            painter,
            Rect::from_center_size(av_center, Vec2::splat(20.0)),
            st::dialogs_premium_icon.icon,
            palette,
            Some(Color32::WHITE),
        );
    } else if boost.is_unclaimed {
        painter.circle_filled(av_center, 19.0, Color32::from_rgb(0x40, 0xD0, 0xCA));
        paint_stats_icon(
            painter,
            Rect::from_center_size(av_center, Vec2::splat(20.0)),
            st::boosts_list_unclaimed_icon,
            palette,
            Some(Color32::WHITE),
        );
    } else {
        painter.circle_filled(av_center, 19.0, Color32::from_rgb(0x32, 0x7F, 0xE5));
        paint_stats_icon(
            painter,
            Rect::from_center_size(av_center, Vec2::splat(20.0)),
            st::boosts_list_unknown_icon,
            palette,
            Some(Color32::WHITE),
        );
    }

    let title = format_boost_title(boost, user);
    let title_galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            title,
            FontId::proportional(13.0),
            to_color32(palette.box_text_fg),
        )
    });
    let title_w = title_galley.size().x;
    painter.galley(
        Pos2::new(rect.left() + 52.0, rect.top() + 8.0),
        title_galley,
        to_color32(palette.box_text_fg),
    );

    // Inline multiplier pill badge beside name when `multiplier > 1`.
    if boost.multiplier > 1 {
        let badge_x = rect.left() + 52.0 + title_w + 6.0;
        let badge_rect =
            Rect::from_min_size(Pos2::new(badge_x, rect.top() + 8.0), Vec2::new(34.0, 16.0));
        painter.rect_filled(
            badge_rect,
            CornerRadius::same(8),
            Color32::from_rgb(0x88, 0x62, 0xE6),
        );
        let mult_g = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                format!("{}", boost.multiplier),
                FontId::proportional(10.0),
                Color32::WHITE,
            )
        });
        let tw = 11.0 + mult_g.size().x;
        let sx = badge_rect.center().x - tw * 0.5;
        paint_stats_icon(
            painter,
            Rect::from_center_size(
                Pos2::new(sx + 4.5, badge_rect.center().y),
                Vec2::splat(10.0),
            ),
            st::boosts_list_mini_icon,
            palette,
            Some(Color32::WHITE),
        );
        painter.galley(
            Pos2::new(sx + 11.0, badge_rect.center().y - mult_g.size().y * 0.5),
            mult_g,
            Color32::WHITE,
        );
    }

    let status = format_boost_status(boost, user.is_some());
    painter.text(
        Pos2::new(rect.left() + 52.0, rect.top() + 28.0),
        Align2::LEFT_TOP,
        &status,
        FontId::proportional(11.5),
        to_color32(palette.window_sub_text_fg),
    );

    // Right translucent `"Giveaway"` or `"Gift"` pill badge (`BoostRow::rightActionPaint`).
    if (boost.is_giveaway || boost.is_gift) && boost.credits == 0 {
        let (label, color, icon) = if boost.is_giveaway {
            (
                tr::lng_gift_link_reason_giveaway(),
                Color32::from_rgb(0x4C, 0xAF, 0x50),
                st::boosts_list_giveaway_mini_icon,
            )
        } else {
            (
                tr::lng_gift_link_label_gift(),
                Color32::from_rgb(0xE0, 0x53, 0x88),
                st::boosts_list_gift_mini_icon,
            )
        };
        let label_g = ui.fonts_mut(|f| f.layout_no_wrap(label, FontId::proportional(11.5), color));
        let pill_w = (label_g.size().x + 28.0).max(52.0);
        let pill_rect = Rect::from_min_size(
            Pos2::new(rect.right() - pill_w - 6.0, rect.center().y - 10.0),
            Vec2::new(pill_w, 20.0),
        );
        painter.rect_filled(
            pill_rect,
            CornerRadius::same(10),
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 44),
        );
        paint_stats_icon(
            painter,
            Rect::from_center_size(
                Pos2::new(pill_rect.left() + 12.0, pill_rect.center().y),
                Vec2::splat(12.0),
            ),
            icon,
            palette,
            Some(color),
        );
        painter.galley(
            Pos2::new(
                pill_rect.left() + 21.0,
                pill_rect.center().y - label_g.size().y * 0.5,
            ),
            label_g,
            color,
        );
    }

    resp.clicked()
}

fn render_pill_action_button(
    ui: &mut Ui,
    label: &str,
    width: f32,
    bg: Color32,
    fg: Color32,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        fg,
    );
    resp.clicked()
}
