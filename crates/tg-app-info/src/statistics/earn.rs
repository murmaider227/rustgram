//! Channel Monetization (TON + Stars) and Bot Stars Earn screens (read-only views).
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/channel_statistics/earn/info_channel_earn_list.{h,cpp}`
//! - `Telegram/SourceFiles/info/bot/earn/info_bot_earn_list.{h,cpp}`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_stats::format::MINUS_SIGN;
use tg_stats::{
    ChartPalette, ChartViewType, CreditsAmount, EarnHistoryEntry, EarnOverviewRow, credits_to_usd,
    earn_chart_view_type, earn_overview_rows, format_credits_amount_decimal, lang_day_month,
    lang_day_month_year, lang_detailed_day_month, major_part_credits, minor_part_credits,
};
use tg_ui::tg_style::st;

use crate::statistics::backend::{BotEarnBundle, ChannelEarnBundle, StatsBackend};
use crate::statistics::chart_widget::{ChartWidgetState, to_color32, to_color32_alpha};
use crate::statistics::sections::{
    render_section_divider, render_section_header, render_show_more_button,
};
use crate::statistics::{paint_stats_icon, strings};

/// Active history tab in Channel Earn (`TON` vs `Stars`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelEarnHistoryTab {
    /// Toncoin (TON) ad proceeds & withdrawals.
    #[default]
    Ton,
    /// Telegram Stars transactions.
    Stars,
}

/// Active history filter tab in Bot Stars Earn (`All` / `Incoming` / `Outgoing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BotEarnHistoryTab {
    /// All transactions.
    #[default]
    All,
    /// Incoming Stars only.
    Incoming,
    /// Outgoing / withdrawn Stars only.
    Outgoing,
}

/// Formats a transaction history date or ad-proceeds range (`info_channel_earn_list.cpp:1089-1102`).
#[must_use]
pub fn format_earn_entry_date(entry: &EarnHistoryEntry) -> String {
    if entry.failed {
        format!(
            "{} — {}",
            lang_detailed_day_month(entry.date),
            tr::lng_channel_earn_history_failed()
        )
    } else if entry.ads_proceeds_to_date > 0 {
        format!(
            "{} — {}",
            lang_day_month(entry.date),
            lang_day_month_year(entry.ads_proceeds_to_date)
        )
    } else if entry.pending {
        tr::lng_channel_earn_history_pending()
    } else {
        lang_detailed_day_month(entry.date)
    }
}

/// Formats a signed TON or Stars history amount string (e.g. `"+6.57 TON"`, `"−15.00 TON"`, `"+1,450 ★"`).
#[must_use]
pub fn format_earn_entry_amount(entry: &EarnHistoryEntry) -> String {
    let sign = if entry.incoming { '+' } else { MINUS_SIGN };
    if entry.credits.is_ton() {
        format!(
            "{sign}{}{} {}",
            major_part_credits(entry.credits),
            minor_part_credits(entry.credits),
            strings::TON_UNIT
        )
    } else {
        format!("{sign}{} ★", format_credits_amount_decimal(entry.credits))
    }
}

// ============================================================================
// Channel Revenue / Monetization View
// ============================================================================

/// Interactive 2FA password prompt state for `payments.getStarsRevenueWithdrawalUrl`.
#[derive(Debug, Clone, Default)]
pub struct WithdrawalPromptState {
    /// Whether the inline 2FA withdrawal card is currently expanded.
    pub open: bool,
    /// `true` for TON ad revenue withdrawal, `false` for Telegram Stars withdrawal.
    pub ton: bool,
    /// User-entered 2FA cloud password (cleared after successful URL fetch).
    pub password: String,
    /// Optional whole-Stars amount string for Stars withdrawals (`None` for TON).
    pub amount_input: String,
    /// Withdrawal URL returned by Telegram (`payments.starsRevenueWithdrawalUrl`).
    pub url_result: Option<String>,
    /// Last RPC or validation error message.
    pub error: Option<String>,
}

impl WithdrawalPromptState {
    /// Opens the 2FA withdrawal card for TON (`ton = true`) or Stars (`ton = false`).
    pub fn open_for(&mut self, ton: bool, default_stars_amount: Option<i64>) {
        self.open = true;
        self.ton = ton;
        self.password.clear();
        self.amount_input = default_stars_amount
            .filter(|&v| v > 0)
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.url_result = None;
        self.error = None;
    }

    /// Submits the 2FA password to `backend.request_revenue_withdrawal_url(...)`.
    pub fn submit(&mut self, backend: &dyn StatsBackend, is_bot: bool) -> Result<String, String> {
        let amount = if self.ton {
            None
        } else {
            let trimmed = self.amount_input.trim();
            if trimmed.is_empty() {
                None
            } else {
                match trimmed.parse::<i64>() {
                    Ok(v) if v > 0 => Some(v),
                    _ => {
                        let err = "Enter a positive Stars amount to withdraw.".to_owned();
                        self.error = Some(err.clone());
                        return Err(err);
                    }
                }
            }
        };
        match backend.request_revenue_withdrawal_url(is_bot, self.ton, amount, &self.password) {
            Ok(url) => {
                self.password.clear();
                self.error = None;
                self.url_result = Some(url.clone());
                Ok(url)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        is_bot: bool,
        palette: &ChartPalette,
        toast: &mut Option<String>,
    ) {
        if !self.open {
            return;
        }
        egui::Frame::NONE
            .fill(to_color32_alpha(palette.window_active_text_fg, 0.08))
            .stroke(Stroke::new(
                1.0,
                to_color32_alpha(palette.window_active_text_fg, 0.25),
            ))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                let heading = if self.ton {
                    "Withdraw TON Revenue (2FA Verification)"
                } else {
                    "Withdraw Stars Revenue (2FA Verification)"
                };
                ui.label(
                    egui::RichText::new(heading)
                        .strong()
                        .size(12.5)
                        .color(to_color32(palette.box_text_fg)),
                );
                ui.add_space(6.0);
                if !self.ton {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Stars amount:")
                                .size(12.0)
                                .color(to_color32(palette.window_sub_text_fg)),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.amount_input)
                                .hint_text("Amount")
                                .desired_width(120.0),
                        );
                    });
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("2FA Password:")
                            .size(12.0)
                            .color(to_color32(palette.window_sub_text_fg)),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text("Cloud password")
                            .desired_width(180.0),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Get Withdrawal Link").clicked() {
                        match self.submit(backend, is_bot) {
                            Ok(_) => {
                                *toast = Some("Withdrawal URL generated.".to_owned());
                            }
                            Err(err) => {
                                *toast = Some(err);
                            }
                        }
                    }
                    if ui.button(tr::lng_cancel()).clicked() {
                        self.open = false;
                        self.password.clear();
                    }
                });

                if let Some(ref err) = self.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.5)
                            .color(Color32::from_rgb(0xE0, 0x53, 0x56)),
                    );
                }

                if let Some(ref url) = self.url_result {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(url)
                                .monospace()
                                .size(11.0)
                                .color(to_color32(palette.window_active_text_fg)),
                        );
                        if ui.button("Copy URL").clicked() {
                            ui.ctx().copy_text(url.clone());
                            *toast = Some("Withdrawal URL copied.".to_owned());
                        }
                    });
                }
            });
    }
}

/// Stateful Channel Revenue (TON + Stars) view (`Info::ChannelEarn::InnerWidget`).
#[derive(Debug, Clone)]
pub struct ChannelEarnView {
    /// Loaded channel monetization data.
    pub bundle: ChannelEarnBundle,
    /// Top ad hours (`Bar`), TON revenue (`StackBar`), and Stars revenue (`StackBar`) charts.
    pub charts: Vec<ChartWidgetState>,
    /// Active history tab (`TON` or `Stars`).
    pub history_tab: ChannelEarnHistoryTab,
    /// Selected transaction for the inline read-only receipt inspector.
    pub selected_entry: Option<EarnHistoryEntry>,
    /// Read-only withdrawal placeholder banner text when triggered.
    pub withdrawal_placeholder: Option<String>,
    /// Live 2FA withdrawal URL prompt state.
    pub withdrawal_prompt: WithdrawalPromptState,
}

impl ChannelEarnView {
    /// Builds a new channel earn view from `bundle`.
    #[must_use]
    pub fn new(bundle: ChannelEarnBundle, palette: &ChartPalette) -> Self {
        let mut charts = Vec::with_capacity(3);
        if bundle.can_view_currency && !bundle.ton.top_hours_graph.chart.is_empty() {
            charts.push(ChartWidgetState::new(
                tr::lng_channel_earn_chart_top_hours(),
                bundle.ton.top_hours_graph.clone(),
                earn_chart_view_type(true),
                palette,
            ));
        }
        if bundle.can_view_currency && !bundle.ton.revenue_graph.chart.is_empty() {
            let mut graph = bundle.ton.revenue_graph.clone();
            graph.chart.currency_rate = bundle.ton.usd_rate;
            charts.push(ChartWidgetState::new(
                tr::lng_channel_earn_chart_revenue(),
                graph,
                earn_chart_view_type(false),
                palette,
            ));
        }
        if bundle.can_view_credits && !bundle.credits.revenue_graph.chart.is_empty() {
            let mut graph = bundle.credits.revenue_graph.clone();
            graph.chart.currency_rate = bundle.credits.usd_rate;
            charts.push(ChartWidgetState::new(
                tr::lng_bot_earn_chart_revenue(),
                graph,
                ChartViewType::StackBar,
                palette,
            ));
        }

        let has_ton_hist = !bundle.ton.first_history_slice.list.is_empty();
        let has_stars_hist = !bundle.credits_history.list.is_empty();
        let history_tab = if !has_ton_hist && has_stars_hist {
            ChannelEarnHistoryTab::Stars
        } else {
            ChannelEarnHistoryTab::Ton
        };

        Self {
            bundle,
            charts,
            history_tab,
            selected_entry: None,
            withdrawal_placeholder: None,
            withdrawal_prompt: WithdrawalPromptState::default(),
        }
    }

    /// Re-applies theme colors to all revenue charts.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        for chart in &mut self.charts {
            chart.apply_palette(palette);
        }
    }

    /// Loads the next page of TON transaction history through `backend`.
    pub fn load_more_ton_history(&mut self, backend: &dyn StatsBackend) {
        let slice = &mut self.bundle.ton.first_history_slice;
        if slice.all_loaded {
            return;
        }
        if let Ok(next) = backend.load_more_channel_earn_history(&slice.token) {
            slice.list.extend(next.list);
            slice.all_loaded = next.all_loaded;
            slice.token = next.token;
        }
    }

    /// Renders the channel monetization screen and returns any toast notification.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) -> Option<String> {
        let mut toast = None;

        // 1. Intro banner (`addAboutWithLearn`).
        egui::Frame::NONE
            .fill(to_color32(palette.box_divider_bg))
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_channel_earn_about("").trim())
                        .size(12.0)
                        .color(to_color32(palette.window_sub_text_fg)),
                );
            });

        // 2. Revenue & Ad Impression Charts.
        for (i, chart) in self.charts.iter_mut().enumerate() {
            if i > 0 {
                render_section_divider(ui, palette);
            }
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    chart.show(ui, backend, palette, now_ms);
                });
        }

        render_section_divider(ui, palette);

        // 3. Dual-currency Overview balance rows (`earn_overview_rows`).
        let rows = earn_overview_rows(&self.bundle.ton, &self.bundle.credits);
        let has_any_credits = !self.bundle.credits.available_balance.is_empty()
            || !self.bundle.credits.current_balance.is_empty()
            || !self.bundle.credits.overall_revenue.is_empty();
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                render_section_header(ui, &tr::lng_channel_earn_overview_title(), "", palette);
                ui.add_space(10.0);
                for row in &rows {
                    render_earn_overview_row(
                        ui,
                        row,
                        self.bundle.can_view_currency,
                        self.bundle.can_view_credits && has_any_credits,
                        palette,
                    );
                    ui.add_space(10.0);
                }
            });

        render_section_divider(ui, palette);

        // 4. Read-only Balance & Withdrawal Placeholder section.
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 12))
            .show(ui, |ui| {
                render_section_header(ui, &tr::lng_channel_earn_balance_title(), "", palette);
                ui.add_space(10.0);

                if self.bundle.can_view_currency && !self.bundle.ton.available_balance.is_empty() {
                    let avail = self.bundle.ton.available_balance;
                    let usd = credits_to_usd(avail, self.bundle.ton.usd_rate, 0);
                    render_centered_balance_card(
                        ui,
                        &format!("{}{}", major_part_credits(avail), minor_part_credits(avail)),
                        strings::TON_UNIT,
                        &usd,
                        Color32::from_rgb(0x32, 0x7F, 0xE5),
                        st::statistics_currency_icon,
                        palette,
                    );
                    ui.add_space(8.0);
                    if render_withdrawal_placeholder_button(
                        ui,
                        strings::WITHDRAW_TON_FRAGMENT,
                        self.bundle.withdrawal_enabled,
                        palette,
                    ) {
                        if backend.supports_live_withdrawal() {
                            self.withdrawal_prompt.open_for(true, None);
                            self.withdrawal_placeholder = None;
                        } else {
                            let msg = strings::TON_WITHDRAWAL_READ_ONLY_NOTICE.to_owned();
                            self.withdrawal_placeholder = Some(msg.clone());
                            toast = Some(msg);
                        }
                    }
                    ui.add_space(12.0);
                }

                if self.bundle.can_view_credits
                    && self.bundle.credits.available_balance.value() > 0.0
                {
                    let avail = self.bundle.credits.available_balance;
                    let usd = credits_to_usd(avail, self.bundle.credits.usd_rate, 0);
                    render_centered_balance_card(
                        ui,
                        &format_credits_amount_decimal(avail),
                        strings::STARS_UNIT,
                        &usd,
                        Color32::from_rgb(0xEB, 0xA5, 0x2D),
                        st::dialogs_premium_icon.icon,
                        palette,
                    );
                    ui.add_space(8.0);
                    if render_withdrawal_placeholder_button(
                        ui,
                        strings::WITHDRAW_STARS_READ_ONLY,
                        self.bundle.credits.is_withdrawal_enabled,
                        palette,
                    ) {
                        if backend.supports_live_withdrawal() {
                            self.withdrawal_prompt
                                .open_for(false, Some(avail.whole().max(1)));
                            self.withdrawal_placeholder = None;
                        } else {
                            let msg = strings::STARS_WITHDRAWAL_READ_ONLY_NOTICE.to_owned();
                            self.withdrawal_placeholder = Some(msg.clone());
                            toast = Some(msg);
                        }
                    }
                }

                if self.withdrawal_prompt.open {
                    ui.add_space(8.0);
                    self.withdrawal_prompt
                        .show(ui, backend, false, palette, &mut toast);
                }

                if let Some(ref note) = self.withdrawal_placeholder {
                    ui.add_space(8.0);
                    render_placeholder_notice_box(ui, note, palette);
                }
            });

        render_section_divider(ui, palette);

        // 5. Transaction History (tabbed TON / Stars).
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                let has_ton = !self.bundle.ton.first_history_slice.list.is_empty();
                let has_stars = !self.bundle.credits_history.list.is_empty();
                if has_ton && has_stars {
                    render_channel_history_tabs(ui, &mut self.history_tab, palette);
                    ui.add_space(8.0);
                } else {
                    render_section_header(ui, &tr::lng_channel_earn_history_title(), "", palette);
                    ui.add_space(6.0);
                }

                let (entries, all_loaded, is_ton_tab) = match self.history_tab {
                    ChannelEarnHistoryTab::Ton => (
                        self.bundle.ton.first_history_slice.list.clone(),
                        self.bundle.ton.first_history_slice.all_loaded,
                        true,
                    ),
                    ChannelEarnHistoryTab::Stars => (
                        self.bundle.credits_history.list.clone(),
                        self.bundle.credits_history.all_loaded,
                        false,
                    ),
                };

                for entry in &entries {
                    if render_earn_history_row(ui, entry, palette) {
                        self.selected_entry = Some(entry.clone());
                    }
                }

                if is_ton_tab && !all_loaded {
                    ui.add_space(4.0);
                    if render_show_more_button(ui, &tr::lng_stories_show_more(), palette) {
                        self.load_more_ton_history(backend);
                    }
                }

                if let Some(entry) = self.selected_entry.clone() {
                    ui.add_space(10.0);
                    if render_receipt_card(ui, &entry, palette) {
                        self.selected_entry = None;
                    }
                }
            });

        // 6. Read-only "Switch Off Ads" status row (`channelRestrictSponsoredLevelMin`).
        render_section_divider(ui, palette);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 8))
            .show(ui, |ui| {
                let locked = self.bundle.current_level < self.bundle.restrict_sponsored_level_min;
                let status_label = if locked {
                    let level_word = tr::lng_boosts_level();
                    format!(
                        "{}  •  {level_word} {} ({level_word} {})",
                        tr::lng_channel_earn_off(),
                        self.bundle.restrict_sponsored_level_min,
                        self.bundle.current_level
                    )
                } else if self.bundle.ton.switched_off {
                    strings::ADS_SWITCHED_OFF.to_owned()
                } else {
                    strings::ADS_ENABLED_READ_ONLY.to_owned()
                };
                ui.label(
                    egui::RichText::new(status_label)
                        .size(12.5)
                        .color(to_color32(palette.window_sub_text_fg)),
                );
            });

        toast
    }
}

// ============================================================================
// Bot Stars Earn View
// ============================================================================

/// Stateful Bot Stars Monetization view (`Info::BotEarn::InnerWidget`).
#[derive(Debug, Clone)]
pub struct BotEarnView {
    /// Loaded bot Stars revenue bundle.
    pub bundle: BotEarnBundle,
    /// Optional Stars revenue chart (`StackBar`).
    pub revenue_chart: Option<ChartWidgetState>,
    /// Active history filter tab (`All` / `Incoming` / `Outgoing`).
    pub history_tab: BotEarnHistoryTab,
    /// Selected transaction for inline read-only receipt inspector.
    pub selected_entry: Option<EarnHistoryEntry>,
    /// Read-only withdrawal placeholder banner text when triggered.
    pub withdrawal_placeholder: Option<String>,
    /// Live 2FA withdrawal URL prompt state.
    pub withdrawal_prompt: WithdrawalPromptState,
    /// Set to `true` when the user clicks the direct Affiliate Program ("Earn Stars") button.
    pub open_affiliate_requested: bool,
}

impl BotEarnView {
    /// Creates a new bot Stars earn view from `bundle`.
    #[must_use]
    pub fn new(bundle: BotEarnBundle, palette: &ChartPalette) -> Self {
        let revenue_chart = if !bundle.credits.revenue_graph.chart.is_empty() {
            let mut graph = bundle.credits.revenue_graph.clone();
            graph.chart.currency_rate = bundle.credits.usd_rate;
            Some(ChartWidgetState::new(
                tr::lng_bot_earn_chart_revenue(),
                graph,
                ChartViewType::StackBar,
                palette,
            ))
        } else {
            None
        };
        Self {
            bundle,
            revenue_chart,
            history_tab: BotEarnHistoryTab::All,
            selected_entry: None,
            withdrawal_placeholder: None,
            withdrawal_prompt: WithdrawalPromptState::default(),
            open_affiliate_requested: false,
        }
    }

    /// Marks the direct Affiliate Program button as clicked (same effect as a pointer click).
    pub const fn request_open_affiliate(&mut self) {
        self.open_affiliate_requested = true;
    }

    /// Drains a pending click on the direct Affiliate Program button, returning the target bot peer ID.
    pub fn take_open_affiliate(&mut self) -> Option<u64> {
        if self.open_affiliate_requested {
            self.open_affiliate_requested = false;
            Some(self.bundle.peer.id)
        } else {
            None
        }
    }

    /// Re-applies theme colors to the bot revenue chart.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        if let Some(ref mut chart) = self.revenue_chart {
            chart.apply_palette(palette);
        }
    }

    /// Returns the currently selected history slice for `history_tab`.
    #[must_use]
    pub fn active_history_entries(&self) -> &[EarnHistoryEntry] {
        match self.history_tab {
            BotEarnHistoryTab::All => &self.bundle.full_history.list,
            BotEarnHistoryTab::Incoming => &self.bundle.in_history.list,
            BotEarnHistoryTab::Outgoing => &self.bundle.out_history.list,
        }
    }

    /// Renders the bot Stars earn screen and returns an optional toast message.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) -> Option<String> {
        let mut toast = None;

        if let Some(ref mut chart) = self.revenue_chart {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 12))
                .show(ui, |ui| {
                    chart.show(ui, backend, palette, now_ms);
                });
            render_section_divider(ui, palette);
        }

        // Stars Overview (Available, Current, Total).
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                render_section_header(ui, &tr::lng_bot_earn_overview_title(), "", palette);
                ui.add_space(10.0);
                let rate = self.bundle.credits.usd_rate;
                let items: [(String, CreditsAmount); 3] = [
                    (
                        tr::lng_bot_earn_available(),
                        self.bundle.credits.available_balance,
                    ),
                    (
                        tr::lng_bot_earn_reward(),
                        self.bundle.credits.current_balance,
                    ),
                    (
                        tr::lng_bot_earn_total(),
                        self.bundle.credits.overall_revenue,
                    ),
                ];
                for (caption, amount) in items {
                    let usd = if amount.is_empty() {
                        String::new()
                    } else {
                        credits_to_usd(amount, rate, 3)
                    };
                    render_single_stars_overview_row(ui, &caption, amount, &usd, palette);
                    ui.add_space(10.0);
                }
            });

        render_section_divider(ui, palette);

        // Read-only Stars Balance & Withdrawal placeholder.
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 12))
            .show(ui, |ui| {
                render_section_header(ui, &tr::lng_bot_earn_balance_title(), "", palette);
                ui.add_space(10.0);
                let avail = self.bundle.credits.available_balance;
                let usd = credits_to_usd(avail, self.bundle.credits.usd_rate, 3);
                render_centered_balance_card(
                    ui,
                    &format_credits_amount_decimal(avail),
                    strings::STARS_UNIT,
                    &usd,
                    Color32::from_rgb(0xEB, 0xA5, 0x2D),
                    st::dialogs_premium_icon.icon,
                    palette,
                );
                ui.add_space(8.0);
                if render_withdrawal_placeholder_button(
                    ui,
                    strings::WITHDRAW_STARS_FRAGMENT,
                    self.bundle.credits.is_withdrawal_enabled,
                    palette,
                ) {
                    if backend.supports_live_withdrawal() {
                        self.withdrawal_prompt
                            .open_for(false, Some(avail.whole().max(1)));
                        self.withdrawal_placeholder = None;
                    } else {
                        let msg = strings::BOT_WITHDRAWAL_READ_ONLY_NOTICE.to_owned();
                        self.withdrawal_placeholder = Some(msg.clone());
                        toast = Some(msg);
                    }
                }
                if self.withdrawal_prompt.open {
                    ui.add_space(8.0);
                    self.withdrawal_prompt
                        .show(ui, backend, true, palette, &mut toast);
                }
                if self.bundle.credits.next_withdrawal_at > 0 {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}{}",
                            strings::NEXT_WITHDRAWAL_PREFIX,
                            lang_detailed_day_month(self.bundle.credits.next_withdrawal_at)
                        ))
                        .size(11.5)
                        .color(to_color32(palette.window_sub_text_fg)),
                    );
                }
                if let Some(ref note) = self.withdrawal_placeholder {
                    ui.add_space(8.0);
                    render_placeholder_notice_box(ui, note, palette);
                }
            });

        render_section_divider(ui, palette);

        // Direct Affiliate Program button (`BotStarRef::AddViewListButton` in `info_bot_earn_list.cpp:263-273`).
        if self.bundle.affiliate_allowed {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    let avail_w = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(avail_w, 58.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let fill = if resp.hovered() {
                        to_color32_alpha(palette.window_active_text_fg, 0.14)
                    } else {
                        to_color32_alpha(palette.window_active_text_fg, 0.08)
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(10),
                        Stroke::new(1.0, to_color32_alpha(palette.window_active_text_fg, 0.28)),
                        egui::StrokeKind::Inside,
                    );
                    let star_color = Color32::from_rgb(0xEB, 0xA5, 0x2D);
                    paint_stats_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(rect.left() + 22.0, rect.center().y),
                            Vec2::splat(20.0),
                        ),
                        st::dialogs_premium_icon.icon,
                        palette,
                        Some(star_color),
                    );
                    let title = tr::lng_credits_summary_earn_title();
                    let about = tr::lng_credits_summary_earn_about();
                    ui.painter().text(
                        Pos2::new(rect.left() + 42.0, rect.top() + 12.0),
                        Align2::LEFT_TOP,
                        &title,
                        FontId::proportional(13.5),
                        to_color32(palette.window_active_text_fg),
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 42.0, rect.top() + 31.0),
                        Align2::LEFT_TOP,
                        &about,
                        FontId::proportional(11.5),
                        to_color32(palette.window_sub_text_fg),
                    );
                    if resp.clicked() {
                        self.open_affiliate_requested = true;
                        toast = Some(format!(
                            "Opening affiliate program for {}…",
                            self.bundle.peer.title
                        ));
                    }
                });
            render_section_divider(ui, palette);
        }

        // Tabbed Stars History (`All` / `Incoming` / `Outgoing`).
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 10))
            .show(ui, |ui| {
                render_bot_history_tabs(
                    ui,
                    &mut self.history_tab,
                    !self.bundle.in_history.list.is_empty(),
                    !self.bundle.out_history.list.is_empty(),
                    palette,
                );
                ui.add_space(8.0);
                let entries: Vec<EarnHistoryEntry> = self.active_history_entries().to_vec();
                for entry in &entries {
                    if render_earn_history_row(ui, entry, palette) {
                        self.selected_entry = Some(entry.clone());
                    }
                }
                if let Some(entry) = self.selected_entry.clone() {
                    ui.add_space(10.0);
                    if render_receipt_card(ui, &entry, palette) {
                        self.selected_entry = None;
                    }
                }
            });

        toast
    }
}

// ============================================================================
// Shared Earn UI Helpers
// ============================================================================

fn render_earn_overview_row(
    ui: &mut Ui,
    row: &EarnOverviewRow,
    show_ton: bool,
    show_stars: bool,
    palette: &ChartPalette,
) {
    let avail_w = ui.available_width().max(220.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 40.0), Sense::hover());
    let painter = ui.painter();
    let half_w = avail_w / 2.0;

    if show_ton {
        let x = rect.left();
        let ton_text = format!("{}{}", row.ton_major, row.ton_minor);
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                ton_text,
                FontId::proportional(15.0),
                to_color32(palette.box_text_fg),
            )
        });
        let w = galley.size().x;
        let icon_cy = rect.top() + galley.size().y * 0.5;
        paint_stats_icon(
            painter,
            Rect::from_center_size(Pos2::new(x + 8.0, icon_cy), Vec2::splat(16.0)),
            st::statistics_currency_icon,
            palette,
            Some(Color32::from_rgb(0x32, 0x7F, 0xE5)),
        );
        painter.galley(
            Pos2::new(x + 20.0, rect.top()),
            galley,
            to_color32(palette.box_text_fg),
        );
        if !row.ton_usd.is_empty() {
            painter.text(
                Pos2::new(x + 20.0 + w + 6.0, rect.top() + 3.0),
                Align2::LEFT_TOP,
                &row.ton_usd,
                FontId::proportional(12.0),
                to_color32(palette.window_sub_text_fg),
            );
        }
    }

    if show_stars {
        let x = if show_ton {
            rect.left() + half_w + 8.0
        } else {
            rect.left()
        };
        let val_galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                row.stars_text.clone(),
                FontId::proportional(15.0),
                to_color32(palette.box_text_fg),
            )
        });
        let val_w = val_galley.size().x;
        let icon_cy = rect.top() + val_galley.size().y * 0.5;
        paint_stats_icon(
            painter,
            Rect::from_center_size(Pos2::new(x + 8.0, icon_cy), Vec2::splat(15.0)),
            st::dialogs_premium_icon.icon,
            palette,
            Some(Color32::from_rgb(0xEB, 0xA5, 0x2D)),
        );
        painter.galley(
            Pos2::new(x + 20.0, rect.top()),
            val_galley,
            to_color32(palette.box_text_fg),
        );
        if !row.stars_usd.is_empty() {
            painter.text(
                Pos2::new(x + 20.0 + val_w + 6.0, rect.top() + 3.0),
                Align2::LEFT_TOP,
                &row.stars_usd,
                FontId::proportional(12.0),
                to_color32(palette.window_sub_text_fg),
            );
        }
    }

    painter.text(
        Pos2::new(rect.left(), rect.top() + 22.0),
        Align2::LEFT_TOP,
        &row.caption,
        FontId::proportional(12.0),
        to_color32(palette.window_sub_text_fg),
    );
}

fn render_single_stars_overview_row(
    ui: &mut Ui,
    caption: &str,
    amount: CreditsAmount,
    usd: &str,
    palette: &ChartPalette,
) {
    let avail_w = ui.available_width().max(200.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 40.0), Sense::hover());
    let painter = ui.painter();

    let star_color = Color32::from_rgb(0xEB, 0xA5, 0x2D);
    let val_text = format_credits_amount_decimal(amount);
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            val_text,
            FontId::proportional(15.5),
            to_color32(palette.box_text_fg),
        )
    });
    let val_w = galley.size().x;
    paint_stats_icon(
        painter,
        Rect::from_center_size(
            Pos2::new(rect.left() + 8.0, rect.top() + galley.size().y * 0.5),
            Vec2::splat(16.0),
        ),
        st::dialogs_premium_icon.icon,
        palette,
        Some(star_color),
    );
    painter.galley(
        Pos2::new(rect.left() + 20.0, rect.top()),
        galley,
        to_color32(palette.box_text_fg),
    );
    if !usd.is_empty() {
        painter.text(
            Pos2::new(rect.left() + 26.0 + val_w, rect.top() + 3.0),
            Align2::LEFT_TOP,
            usd,
            FontId::proportional(12.0),
            to_color32(palette.window_sub_text_fg),
        );
    }
    painter.text(
        Pos2::new(rect.left(), rect.top() + 22.0),
        Align2::LEFT_TOP,
        caption,
        FontId::proportional(12.0),
        to_color32(palette.window_sub_text_fg),
    );
}

fn render_centered_balance_card(
    ui: &mut Ui,
    amount_text: &str,
    unit: &str,
    usd_text: &str,
    accent: Color32,
    icon: tg_ui::tg_style::Icon,
    palette: &ChartPalette,
) {
    let avail_w = ui.available_width().max(200.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 62.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(10),
        to_color32_alpha(palette.box_divider_bg, 0.85),
    );
    let full = format!("{amount_text} {unit}");
    let g = ui.fonts_mut(|f| f.layout_no_wrap(full, FontId::proportional(20.0), accent));
    let total_w = 22.0 + g.size().x;
    let start_x = rect.center().x - total_w * 0.5;
    paint_stats_icon(
        painter,
        Rect::from_center_size(
            Pos2::new(start_x + 9.0, rect.top() + 12.0 + g.size().y * 0.5),
            Vec2::splat(18.0),
        ),
        icon,
        palette,
        Some(accent),
    );
    painter.galley(Pos2::new(start_x + 22.0, rect.top() + 12.0), g, accent);
    painter.text(
        Pos2::new(rect.center().x, rect.top() + 38.0),
        Align2::CENTER_TOP,
        usd_text,
        FontId::proportional(12.5),
        to_color32(palette.window_sub_text_fg),
    );
}

fn render_withdrawal_placeholder_button(
    ui: &mut Ui,
    label: &str,
    enabled_flag: bool,
    palette: &ChartPalette,
) -> bool {
    let avail_w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 36.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if enabled_flag {
        to_color32(palette.window_active_text_fg)
    } else {
        to_color32_alpha(palette.window_active_text_fg, 0.45)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
    resp.clicked()
}

fn render_placeholder_notice_box(ui: &mut Ui, message: &str, palette: &ChartPalette) {
    egui::Frame::NONE
        .fill(to_color32_alpha(palette.window_active_text_fg, 0.1))
        .stroke(Stroke::new(
            1.0,
            to_color32_alpha(palette.window_active_text_fg, 0.25),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(message)
                    .size(12.0)
                    .color(to_color32(palette.box_text_fg)),
            );
        });
}

fn render_channel_history_tabs(
    ui: &mut Ui,
    tab: &mut ChannelEarnHistoryTab,
    palette: &ChartPalette,
) {
    let tabs = [
        (
            ChannelEarnHistoryTab::Ton,
            tr::lng_channel_earn_currency_history(),
        ),
        (
            ChannelEarnHistoryTab::Stars,
            tr::lng_channel_earn_credits_history(),
        ),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (t, label) in tabs {
            let selected = *tab == t;
            let col = if selected {
                to_color32(palette.window_active_text_fg)
            } else {
                to_color32(palette.window_sub_text_fg)
            };
            let galley = ui.fonts_mut(|f| f.layout_no_wrap(label, FontId::proportional(13.5), col));
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(galley.size().x + 8.0, 28.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                *tab = t;
            }
            ui.painter()
                .galley(Pos2::new(rect.left() + 4.0, rect.top() + 4.0), galley, col);
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
}

fn render_bot_history_tabs(
    ui: &mut Ui,
    tab: &mut BotEarnHistoryTab,
    has_in: bool,
    has_out: bool,
    palette: &ChartPalette,
) {
    let mut tabs = vec![(
        BotEarnHistoryTab::All,
        tr::lng_credits_summary_history_tab_full(),
    )];
    if has_in {
        tabs.push((
            BotEarnHistoryTab::Incoming,
            tr::lng_credits_summary_history_tab_in(),
        ));
    }
    if has_out {
        tabs.push((
            BotEarnHistoryTab::Outgoing,
            tr::lng_credits_summary_history_tab_out(),
        ));
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (t, label) in tabs {
            let selected = *tab == t;
            let col = if selected {
                to_color32(palette.window_active_text_fg)
            } else {
                to_color32(palette.window_sub_text_fg)
            };
            let galley = ui.fonts_mut(|f| f.layout_no_wrap(label, FontId::proportional(13.5), col));
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(galley.size().x + 8.0, 28.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                *tab = t;
            }
            ui.painter()
                .galley(Pos2::new(rect.left() + 4.0, rect.top() + 4.0), galley, col);
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
}

fn render_earn_history_row(ui: &mut Ui, entry: &EarnHistoryEntry, palette: &ChartPalette) -> bool {
    let avail_w = ui.available_width().max(220.0);
    let has_provider = !entry.provider.is_empty();
    let row_h = if has_provider { 62.0 } else { 48.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.box_text_fg, 0.04),
        );
    }

    let painter = ui.painter();
    let amount_str = format_earn_entry_amount(entry);
    let amount_col = if entry.incoming {
        to_color32(palette.delta_positive)
    } else {
        to_color32(palette.delta_negative)
    };
    painter.text(
        Pos2::new(rect.right() - 6.0, rect.center().y),
        Align2::RIGHT_CENTER,
        &amount_str,
        FontId::proportional(13.5),
        amount_col,
    );

    let mut y = rect.top() + 7.0;
    painter.text(
        Pos2::new(rect.left() + 4.0, y),
        Align2::LEFT_TOP,
        &entry.title,
        FontId::proportional(13.0),
        to_color32(palette.box_text_fg),
    );
    y += 18.0;
    if has_provider {
        painter.text(
            Pos2::new(rect.left() + 4.0, y),
            Align2::LEFT_TOP,
            &entry.provider,
            FontId::monospace(10.5),
            to_color32(palette.window_sub_text_fg),
        );
        y += 15.0;
    }
    let date_text = format_earn_entry_date(entry);
    let date_col = if entry.failed {
        to_color32(palette.delta_negative)
    } else {
        to_color32(palette.window_sub_text_fg)
    };
    painter.text(
        Pos2::new(rect.left() + 4.0, y),
        Align2::LEFT_TOP,
        &date_text,
        FontId::proportional(11.5),
        date_col,
    );

    resp.clicked()
}

fn render_receipt_card(ui: &mut Ui, entry: &EarnHistoryEntry, palette: &ChartPalette) -> bool {
    let mut close = false;
    egui::Frame::NONE
        .fill(to_color32(palette.box_divider_bg))
        .stroke(Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.1)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(strings::RECEIPT_TITLE)
                        .size(13.0)
                        .strong()
                        .color(to_color32(palette.box_text_fg)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr::lng_close()).clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("{}{}", strings::RECEIPT_ID_PREFIX, entry.id))
                    .size(11.5)
                    .color(to_color32(palette.window_sub_text_fg)),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{}{}",
                    strings::RECEIPT_AMOUNT_PREFIX,
                    format_earn_entry_amount(entry)
                ))
                .size(12.5)
                .color(to_color32(palette.box_text_fg)),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{}{}",
                    strings::RECEIPT_DATE_PREFIX,
                    format_earn_entry_date(entry)
                ))
                .size(12.0)
                .color(to_color32(palette.window_sub_text_fg)),
            );
            if !entry.provider.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{}{}",
                        strings::RECEIPT_PROVIDER_PREFIX,
                        entry.provider
                    ))
                    .size(11.5)
                    .color(to_color32(palette.window_sub_text_fg)),
                );
            }
        });
    close
}
