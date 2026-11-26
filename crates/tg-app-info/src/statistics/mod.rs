//! Telegram Desktop statistics, boosts, and monetization screens rendered with `egui` on top of `tg-stats`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/statistics/chart_widget.{h,cpp}` ([`chart_widget`])
//! - `Telegram/SourceFiles/statistics/widgets/*` ([`chart_widget`])
//! - `Telegram/SourceFiles/info/statistics/*` ([`sections`])
//! - `Telegram/SourceFiles/info/channel_statistics/boosts/*` ([`boosts`])
//! - `Telegram/SourceFiles/info/channel_statistics/earn/*` & `info/bot/earn/*` ([`earn`])
//! - Decoupled [`backend::StatsBackend`] trait & [`mock::MockStatsBackend`] fake

pub use tg_app_core::statistics::backend;
pub mod boosts;
pub mod chart_widget;
pub mod earn;
pub mod mock;
pub mod sections;
pub mod strings;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers,
    Order, Pos2, Rect, ScrollArea, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_stats::{AnyStatistics, ChartPalette, serialize_xlsx, sheets_from_statistics};
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{Icon as StyleIcon, st};
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::ThemeManager;

use crate::statistics::backend::StatsBackend;
use crate::statistics::boosts::BoostsView;
use crate::statistics::chart_widget::{to_color32, to_color32_alpha};
use crate::statistics::earn::{BotEarnView, ChannelEarnView};
use crate::statistics::mock::MockStatsBackend;
use crate::statistics::sections::{
    ChannelStatsView, MessageStatsView, SectionNavAction, SupergroupStatsView,
};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

fn cached_theme_palette(night: bool) -> &'static ThemePalette {
    static DAY: OnceLock<ThemePalette> = OnceLock::new();
    static NIGHT: OnceLock<ThemePalette> = OnceLock::new();
    if night {
        NIGHT.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Night)
                .palette()
                .clone()
        })
    } else {
        DAY.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Default)
                .palette()
                .clone()
        })
    }
}

/// Returns the cached `ThemeManager` palette corresponding to `palette`'s light/dark polarity.
#[must_use]
pub(crate) fn theme_palette_for_chart(palette: &ChartPalette) -> &'static ThemePalette {
    let bg = palette.box_bg;
    let luma = 0.299 * f32::from(bg.r) + 0.587 * f32::from(bg.g) + 0.114 * f32::from(bg.b);
    cached_theme_palette(luma < 128.0)
}

/// Paints a `tg-style` icon centered inside `rect`, resolving colors from `ThemeManager`'s palette.
pub(crate) fn paint_stats_icon(
    painter: &egui::Painter,
    rect: Rect,
    icon: StyleIcon,
    palette: &ChartPalette,
    tint: Option<Color32>,
) -> bool {
    paint_style_icon(painter, rect, icon, theme_palette_for_chart(palette), tint)
}

fn theme_color_to_stats(c: tg_ui::tg_theme::Color) -> tg_stats::Color {
    tg_stats::Color::rgba(c.r, c.g, c.b, c.a)
}

/// Active top-level or nested statistics screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatisticsScreen {
    /// Broadcast channel statistics (overview, 12 charts, recent posts & stories).
    #[default]
    Channel,
    /// Supergroup statistics (overview, 8 charts, top members/admins/inviters).
    Supergroup,
    /// Individual channel post statistics.
    Message(i32),
    /// Individual channel story statistics.
    Story(i32),
    /// Channel/supergroup boosts overview, prepaid giveaways, boosts/gifts list, boost link.
    Boosts,
    /// Channel TON + Stars monetization overview, revenue charts, history, withdrawal placeholder.
    ChannelEarn,
    /// Bot Stars monetization overview, revenue chart, history tabs, withdrawal placeholder.
    BotEarn,
}

impl StatisticsScreen {
    /// Human-readable title displayed in the top navigation bar.
    #[must_use]
    pub fn title(self) -> String {
        match self {
            Self::Channel | Self::Supergroup => tr::lng_stats_title(),
            Self::Message(id) => format!("{} #{id}", tr::lng_stats_message_title()),
            Self::Story(id) => format!("{} #{id}", tr::lng_stats_story_title()),
            Self::Boosts => tr::lng_boosts_title(),
            Self::ChannelEarn => tr::lng_channel_earn_title(),
            Self::BotEarn => tr::lng_bot_earn_title(),
        }
    }
}

/// Multi-screen statistics controller with lazy screen loading, back stack, and toast feedback.
pub struct StatisticsScreenHost {
    backend: Box<dyn StatsBackend>,
    started_at: Instant,
    palette: ChartPalette,
    theme_mode: ThemeMode,
    active_screen: StatisticsScreen,
    back_stack: Vec<StatisticsScreen>,
    channel_view: Option<ChannelStatsView>,
    supergroup_view: Option<SupergroupStatsView>,
    message_view: Option<MessageStatsView>,
    boosts_view: Option<BoostsView>,
    channel_earn_view: Option<ChannelEarnView>,
    bot_earn_view: Option<BotEarnView>,
    pending_open_affiliate_peer: Option<u64>,
    load_error: Option<String>,
    toast_message: Option<(String, i64)>,
}

impl StatisticsScreenHost {
    /// Creates a new statistics controller backed by `backend` and starting on `StatisticsScreen::Channel`.
    #[must_use]
    pub fn new(backend: Box<dyn StatsBackend>, theme_mode: ThemeMode) -> Self {
        let palette = chart_palette_for_mode(theme_mode);
        let mut host = Self {
            backend,
            started_at: Instant::now(),
            palette,
            theme_mode,
            active_screen: StatisticsScreen::Channel,
            back_stack: Vec::new(),
            channel_view: None,
            supergroup_view: None,
            message_view: None,
            boosts_view: None,
            channel_earn_view: None,
            bot_earn_view: None,
            pending_open_affiliate_peer: None,
            load_error: None,
            toast_message: None,
        };
        host.ensure_screen_loaded(StatisticsScreen::Channel);
        host
    }

    /// Creates a host pre-wired to [`MockStatsBackend`].
    #[must_use]
    pub fn with_mock(theme_mode: ThemeMode) -> Self {
        Self::new(Box::new(MockStatsBackend::new()), theme_mode)
    }

    /// Returns the currently displayed [`StatisticsScreen`].
    #[must_use]
    pub const fn active_screen(&self) -> StatisticsScreen {
        self.active_screen
    }

    /// Returns `true` when there is a previous screen on the back stack.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    /// Returns the active [`ChartPalette`].
    #[must_use]
    pub const fn palette(&self) -> &ChartPalette {
        &self.palette
    }

    /// Returns a reference to the loaded [`ChannelStatsView`], if any.
    #[must_use]
    pub fn channel_view(&self) -> Option<&ChannelStatsView> {
        self.channel_view.as_ref()
    }

    /// Returns a mutable reference to the loaded [`ChannelStatsView`], if any.
    #[must_use]
    pub fn channel_view_mut(&mut self) -> Option<&mut ChannelStatsView> {
        self.channel_view.as_mut()
    }

    /// Returns a reference to the loaded [`BoostsView`], if any.
    #[must_use]
    pub fn boosts_view(&self) -> Option<&BoostsView> {
        self.boosts_view.as_ref()
    }

    /// Returns a reference to the loaded [`ChannelEarnView`], if any.
    #[must_use]
    pub fn channel_earn_view(&self) -> Option<&ChannelEarnView> {
        self.channel_earn_view.as_ref()
    }

    /// Returns a reference to the loaded [`BotEarnView`], if any.
    #[must_use]
    pub fn bot_earn_view(&self) -> Option<&BotEarnView> {
        self.bot_earn_view.as_ref()
    }

    /// Drains a pending click on the direct Affiliate Program button inside [`BotEarnView`],
    /// returning the target bot peer ID.
    pub fn take_open_affiliate_peer(&mut self) -> Option<u64> {
        self.pending_open_affiliate_peer.take()
    }

    /// Clears cached per-peer views and opens `screen` fresh from the backend.
    pub fn open_screen_fresh(&mut self, screen: StatisticsScreen) {
        self.channel_view = None;
        self.supergroup_view = None;
        self.message_view = None;
        self.boosts_view = None;
        self.channel_earn_view = None;
        self.bot_earn_view = None;
        self.select_top_screen(screen);
    }

    /// Switches to a top-level screen (clears the back stack).
    pub fn select_top_screen(&mut self, screen: StatisticsScreen) {
        self.back_stack.clear();
        self.active_screen = screen;
        self.ensure_screen_loaded(screen);
    }

    /// Pushes `screen` onto the navigation stack so the user can return via [`Self::go_back`].
    pub fn push_screen(&mut self, screen: StatisticsScreen) {
        if self.active_screen != screen {
            self.back_stack.push(self.active_screen);
            self.active_screen = screen;
            self.ensure_screen_loaded(screen);
        }
    }

    /// Pops the previous screen from the back stack, returning `true` if navigation occurred.
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.back_stack.pop() {
            self.active_screen = prev;
            self.ensure_screen_loaded(prev);
            true
        } else {
            false
        }
    }

    /// Updates Day/Night palette across all cached screens when `theme_mode` changes.
    pub fn sync_theme(&mut self, theme_mode: ThemeMode) {
        if self.theme_mode == theme_mode {
            return;
        }
        self.theme_mode = theme_mode;
        self.palette = chart_palette_for_mode(theme_mode);
        if let Some(ref mut v) = self.channel_view {
            v.apply_palette(&self.palette);
        }
        if let Some(ref mut v) = self.supergroup_view {
            v.apply_palette(&self.palette);
        }
        if let Some(ref mut v) = self.message_view {
            v.apply_palette(&self.palette);
        }
        if let Some(ref mut v) = self.channel_earn_view {
            v.apply_palette(&self.palette);
        }
        if let Some(ref mut v) = self.bot_earn_view {
            v.apply_palette(&self.palette);
        }
    }

    fn ensure_screen_loaded(&mut self, screen: StatisticsScreen) {
        self.load_error = None;
        match screen {
            StatisticsScreen::Channel => {
                if self.channel_view.is_none() {
                    match self.backend.channel_statistics() {
                        Ok(bundle) => {
                            self.channel_view = Some(ChannelStatsView::new(bundle, &self.palette));
                        }
                        Err(err) => self.load_error = Some(err),
                    }
                }
            }
            StatisticsScreen::Supergroup => {
                if self.supergroup_view.is_none() {
                    match self.backend.supergroup_statistics() {
                        Ok(bundle) => {
                            self.supergroup_view =
                                Some(SupergroupStatsView::new(bundle, &self.palette));
                        }
                        Err(err) => self.load_error = Some(err),
                    }
                }
            }
            StatisticsScreen::Message(id) => match self.backend.message_statistics(id) {
                Ok(bundle) => {
                    self.message_view = Some(MessageStatsView::new(bundle, &self.palette));
                }
                Err(err) => self.load_error = Some(err),
            },
            StatisticsScreen::Story(id) => match self.backend.story_statistics(id) {
                Ok(bundle) => {
                    self.message_view = Some(MessageStatsView::new(bundle, &self.palette));
                }
                Err(err) => self.load_error = Some(err),
            },
            StatisticsScreen::Boosts => {
                if self.boosts_view.is_none() {
                    match self.backend.boost_status() {
                        Ok(bundle) => {
                            self.boosts_view = Some(BoostsView::new(bundle));
                        }
                        Err(err) => self.load_error = Some(err),
                    }
                }
            }
            StatisticsScreen::ChannelEarn => {
                if self.channel_earn_view.is_none() {
                    match self.backend.channel_earn() {
                        Ok(bundle) => {
                            self.channel_earn_view =
                                Some(ChannelEarnView::new(bundle, &self.palette));
                        }
                        Err(err) => self.load_error = Some(err),
                    }
                }
            }
            StatisticsScreen::BotEarn => {
                if self.bot_earn_view.is_none() {
                    match self.backend.bot_earn() {
                        Ok(bundle) => {
                            self.bot_earn_view = Some(BotEarnView::new(bundle, &self.palette));
                        }
                        Err(err) => self.load_error = Some(err),
                    }
                }
            }
        }
    }

    /// Shows a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, msg: String, now_ms: i64) {
        let Some(msg) = tg_app_core::user_error::visible_toast_text(msg) else {
            return;
        };
        self.toast_message = Some((msg, now_ms + 3_000));
    }

    /// Exports the currently loaded statistics screen into `.xlsx` bytes (`statistics_xlsx`).
    pub fn export_current_xlsx_bytes(&self) -> Result<Vec<u8>, String> {
        let stats = match self.active_screen {
            StatisticsScreen::Channel => self
                .channel_view
                .as_ref()
                .map(|v| AnyStatistics::Channel(Box::new(v.bundle.stats.clone()))),
            StatisticsScreen::Supergroup => self
                .supergroup_view
                .as_ref()
                .map(|v| AnyStatistics::Supergroup(Box::new(v.bundle.stats.clone()))),
            StatisticsScreen::Message(_) => self
                .message_view
                .as_ref()
                .map(|v| AnyStatistics::Message(Box::new(v.bundle.stats.clone()))),
            StatisticsScreen::Story(_) => self
                .message_view
                .as_ref()
                .map(|v| AnyStatistics::Story(Box::new(v.bundle.stats.clone()))),
            _ => None,
        }
        .ok_or_else(|| {
            "XLSX export is available on Channel, Supergroup, Message, and Story stats.".to_owned()
        })?;

        let sheets = sheets_from_statistics(&stats, None);
        serialize_xlsx(&sheets).map_err(|e| format!("XLSX export failed: {e}"))
    }

    /// Returns elapsed milliseconds since host construction.
    #[must_use]
    pub fn elapsed_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    /// Renders the complete statistics panel (top header, screen tabs, scrollable content, toast).
    ///
    /// Returns `true` if the user clicked the optional close button.
    pub fn show(&mut self, ui: &mut Ui, show_close: bool) -> bool {
        let now_ms = self.elapsed_ms();
        self.show_at(ui, show_close, now_ms)
    }

    /// Renders the statistics panel at an explicit timestamp `now_ms` (deterministic for tests/screenshots).
    pub fn show_at(&mut self, ui: &mut Ui, show_close: bool, now_ms: i64) -> bool {
        ensure_tdesktop_fonts(ui.ctx());
        let mut closed = false;
        let palette = self.palette;
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, to_color32(palette.box_bg));

        // Top navigation bar (50px).
        let top_h = 50.0;
        let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), top_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        if self.can_go_back() || show_close {
                            let (btn_r, resp) =
                                ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                ui.painter().circle_filled(
                                    btn_r.center(),
                                    14.0,
                                    to_color32_alpha(palette.box_text_fg, 0.06),
                                );
                            }
                            let tint = if resp.hovered() {
                                to_color32(palette.box_text_fg)
                            } else {
                                to_color32(palette.window_sub_text_fg)
                            };
                            paint_stats_icon(
                                ui.painter(),
                                btn_r,
                                st::back_button_icon,
                                &palette,
                                Some(tint),
                            );
                            if resp.clicked() && !self.go_back() {
                                closed = true;
                            }
                        }

                        ui.label(
                            egui::RichText::new(self.active_screen.title())
                                .font(font_id_from_style(
                                    st::statistics_header_title_text_style.font,
                                ))
                                .strong()
                                .color(to_color32(palette.box_text_fg)),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if show_close {
                                let (btn_r, resp) =
                                    ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    ui.painter().circle_filled(
                                        btn_r.center(),
                                        14.0,
                                        to_color32_alpha(palette.box_text_fg, 0.06),
                                    );
                                }
                                let tint = if resp.hovered() {
                                    to_color32(palette.box_text_fg)
                                } else {
                                    to_color32(palette.window_sub_text_fg)
                                };
                                paint_stats_icon(
                                    ui.painter(),
                                    btn_r,
                                    st::box_title_close_icon,
                                    &palette,
                                    Some(tint),
                                );
                                if resp.on_hover_text(tr::lng_close()).clicked() {
                                    closed = true;
                                }
                            }

                            if matches!(
                                self.active_screen,
                                StatisticsScreen::Channel
                                    | StatisticsScreen::Supergroup
                                    | StatisticsScreen::Message(_)
                                    | StatisticsScreen::Story(_)
                            ) && ui.small_button("Export XLSX").clicked()
                            {
                                match self.export_current_xlsx_bytes() {
                                    Ok(bytes) => {
                                        if let Some(path) = rfd::FileDialog::new()
                                            .set_file_name("statistics.xlsx")
                                            .add_filter("Excel Workbook", &["xlsx"])
                                            .save_file()
                                        {
                                            match std::fs::write(&path, &bytes) {
                                                Ok(()) => self.show_toast(
                                                    format!(
                                                        "Exported XLSX ({} bytes).",
                                                        bytes.len()
                                                    ),
                                                    now_ms,
                                                ),
                                                Err(err) => self.show_toast(
                                                    format!("Failed to save XLSX: {err}"),
                                                    now_ms,
                                                ),
                                            }
                                        }
                                    }
                                    Err(err) => self.show_toast(err, now_ms),
                                }
                            }
                        });
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(top_rect.left(), top_rect.bottom()),
                    Pos2::new(top_rect.right(), top_rect.bottom()),
                ],
                Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.08)),
            );
        });

        // Section switcher bar (Channel / Supergroup / Boosts / Channel Earn / Bot Earn).
        let tab_h = 36.0;
        let tab_rect = Rect::from_min_size(
            Pos2::new(rect.left(), top_rect.bottom()),
            Vec2::new(rect.width(), tab_h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 0))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        let tabs = [
                            (StatisticsScreen::Channel, strings::TAB_CHANNEL.to_owned()),
                            (
                                StatisticsScreen::Supergroup,
                                strings::TAB_SUPERGROUP.to_owned(),
                            ),
                            (StatisticsScreen::Boosts, tr::lng_boosts_title()),
                            (StatisticsScreen::ChannelEarn, tr::lng_channel_earn_title()),
                            (StatisticsScreen::BotEarn, tr::lng_bot_earn_title()),
                        ];
                        for (screen, label) in tabs {
                            let is_selected = self.active_screen == screen
                                || (screen == StatisticsScreen::Channel
                                    && matches!(
                                        self.active_screen,
                                        StatisticsScreen::Message(_) | StatisticsScreen::Story(_)
                                    ));
                            let col = if is_selected {
                                to_color32(palette.window_active_text_fg)
                            } else {
                                to_color32(palette.window_sub_text_fg)
                            };
                            let galley = ui.fonts_mut(|f| {
                                f.layout_no_wrap(label, FontId::proportional(12.5), col)
                            });
                            let (r, resp) = ui.allocate_exact_size(
                                Vec2::new(galley.size().x + 6.0, tab_h),
                                Sense::click(),
                            );
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                self.select_top_screen(screen);
                            }
                            ui.painter().galley(
                                Pos2::new(r.left() + 3.0, r.center().y - galley.size().y / 2.0),
                                galley,
                                col,
                            );
                            if is_selected {
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(r.left(), r.bottom() - 2.0),
                                        Pos2::new(r.right(), r.bottom() - 2.0),
                                    ],
                                    Stroke::new(2.5, to_color32(palette.window_active_text_fg)),
                                );
                            }
                        }
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(tab_rect.left(), tab_rect.bottom()),
                    Pos2::new(tab_rect.right(), tab_rect.bottom()),
                ],
                Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.08)),
            );
        });

        // Scrollable screen body.
        let body_rect = Rect::from_min_max(Pos2::new(rect.left(), tab_rect.bottom()), rect.max);
        let mut pending_nav: Option<SectionNavAction> = None;
        let mut pending_toast: Option<String> = None;

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            // A failed stats load shows nothing; the loading state stays
            // (`info_statistics_inner_widget.cpp:656-665`).
            if self.load_error.is_some() {
                return;
            }

            ScrollArea::vertical()
                .id_salt(("tg_statistics_scroll", format!("{:?}", self.active_screen)))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match self.active_screen {
                        StatisticsScreen::Channel => {
                            if let Some(ref mut view) = self.channel_view {
                                pending_nav =
                                    view.show(ui, self.backend.as_ref(), &palette, now_ms);
                            }
                        }
                        StatisticsScreen::Supergroup => {
                            if let Some(ref mut view) = self.supergroup_view {
                                view.show(ui, self.backend.as_ref(), &palette, now_ms);
                            }
                        }
                        StatisticsScreen::Message(_) | StatisticsScreen::Story(_) => {
                            if let Some(ref mut view) = self.message_view {
                                view.show(ui, self.backend.as_ref(), &palette, now_ms);
                            }
                        }
                        StatisticsScreen::Boosts => {
                            if let Some(ref mut view) = self.boosts_view {
                                pending_toast = view.show(ui, self.backend.as_ref(), &palette);
                            }
                        }
                        StatisticsScreen::ChannelEarn => {
                            if let Some(ref mut view) = self.channel_earn_view {
                                pending_toast =
                                    view.show(ui, self.backend.as_ref(), &palette, now_ms);
                            }
                        }
                        StatisticsScreen::BotEarn => {
                            if let Some(ref mut view) = self.bot_earn_view {
                                pending_toast =
                                    view.show(ui, self.backend.as_ref(), &palette, now_ms);
                                if let Some(pid) = view.take_open_affiliate() {
                                    self.pending_open_affiliate_peer = Some(pid);
                                }
                            }
                        }
                    }
                    ui.add_space(16.0);
                });
        });

        if let Some(nav) = pending_nav {
            match nav {
                SectionNavAction::OpenMessageStats(id) => {
                    self.push_screen(StatisticsScreen::Message(id));
                }
                SectionNavAction::OpenStoryStats(id) => {
                    self.push_screen(StatisticsScreen::Story(id));
                }
            }
        }
        if let Some(msg) = pending_toast {
            self.show_toast(msg, now_ms);
        }

        // Transient bottom toast banner.
        if let Some((ref text, expires_at)) = self.toast_message {
            if now_ms >= expires_at {
                self.toast_message = None;
            } else {
                let toast_w = (rect.width() - 40.0).min(420.0);
                let toast_rect = Rect::from_min_size(
                    Pos2::new(rect.center().x - toast_w / 2.0, rect.bottom() - 48.0),
                    Vec2::new(toast_w, 34.0),
                );
                ui.painter().rect_filled(
                    toast_rect,
                    CornerRadius::same(8),
                    Color32::from_rgba_unmultiplied(0x1E, 0x24, 0x2B, 232),
                );
                ui.painter().text(
                    toast_rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
                ui.ctx().request_repaint();
            }
        }

        closed
    }
}

/// Maps `tg-app`'s [`ThemeMode`] to `tg-stats`'s [`ChartPalette`] using `tg-ui`'s `ThemeManager`.
#[must_use]
pub fn chart_palette_for_mode(mode: ThemeMode) -> ChartPalette {
    let night = matches!(mode, ThemeMode::Night);
    let theme = cached_theme_palette(night);
    let mut p = if night {
        ChartPalette::dark()
    } else {
        ChartPalette::light()
    };
    if let Some(c) = theme.color_by_name("boxBg") {
        p.box_bg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("boxTextFg") {
        p.box_text_fg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("boxDividerBg") {
        p.box_divider_bg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("windowActiveTextFg") {
        p.window_active_text_fg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("windowSubTextFg") {
        p.window_sub_text_fg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("premiumButtonFg") {
        p.premium_button_fg = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("statisticsChartActive") {
        p.chart_active = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("statisticsChartInactive") {
        p.chart_inactive = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("boxTextFgGood") {
        p.delta_positive = theme_color_to_stats(c);
    }
    if let Some(c) = theme.color_by_name("boxTextFgError") {
        p.delta_negative = theme_color_to_stats(c);
    }
    p
}

/// Self-contained overlay host for `app.rs` (single-line hook in `TgforkApp::ui`).
///
/// Press `Ctrl+Shift+T` in the running desktop app to open or close the Statistics &
/// Monetization panel over the workspace.
pub struct AppStatisticsHost {
    open: bool,
    screen_host: StatisticsScreenHost,
}

impl AppStatisticsHost {
    fn existing_state(ctx: &Context) -> Option<Arc<Mutex<Self>>> {
        ctx.data_mut(|d| d.get_temp::<Arc<Mutex<Self>>>(Id::unique("tg_app_statistics_host")))
    }

    /// Returns `true` while the floating demo Statistics drawer is open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        Self::existing_state(ctx).is_some_and(|s| s.lock().is_ok_and(|g| g.open))
    }

    /// Closes the floating demo Statistics drawer (passcode lock:
    /// `Application::closeAdditionalWindows`).
    pub fn close(ctx: &Context) {
        if let Some(state) = Self::existing_state(ctx)
            && let Ok(mut guard) = state.lock()
        {
            guard.open = false;
        }
    }

    /// Drives keyboard toggle (`Ctrl+Shift+T`) and renders the floating Statistics drawer when open.
    pub fn update_and_render(ctx: &Context, palette: &Palette, theme_mode: ThemeMode) {
        let toggle_pressed = ctx.input(|i| {
            i.modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                && i.key_pressed(Key::T)
        });

        let state_arc: Arc<Mutex<Self>> = ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_statistics_host"), || {
                Arc::new(Mutex::new(Self {
                    open: false,
                    screen_host: StatisticsScreenHost::with_mock(theme_mode),
                }))
            })
            .clone()
        });

        let Ok(mut guard) = state_arc.lock() else {
            return;
        };
        if toggle_pressed {
            guard.open = !guard.open;
        }
        if !guard.open {
            return;
        }
        guard.screen_host.sync_theme(theme_mode);

        let screen_rect = ctx.content_rect();
        let panel_w = (screen_rect.width() * 0.62).clamp(420.0, 620.0);
        let panel_h = (screen_rect.height() - 48.0).max(400.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.top() + 24.0,
        );

        let mut close_requested = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Area::new(Id::unique("tg_statistics_overlay_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, CornerRadius::ZERO, palette.layer_backdrop);
                if resp.clicked() {
                    close_requested = true;
                }
            });

        egui::Area::new(Id::unique("tg_statistics_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter().rect_filled(
                    panel_rect.expand(1.0),
                    CornerRadius::same(10),
                    palette.divider,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.screen_host.show(ui, true) {
                        close_requested = true;
                    }
                });
            });

        if close_requested {
            guard.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui::epaint::{Primitive, TextureId};
    use egui::{Pos2, RawInput, Rect, Vec2};
    use tg_stats::{
        Boost, BoostPrepaidGiveaway, BoostsOverview, ChartPalette, ChartViewType, CreditsAmount,
        CreditsType, EarnHistoryEntry, Limits, StatisticalChart, StatisticalGraph,
    };

    use super::*;
    use crate::statistics::boosts::{
        BoostsListTab, compute_boost_level_progress, format_boost_status, format_boost_title,
        format_prepaid_giveaway_labels,
    };
    use crate::statistics::chart_widget::{ChartWidgetState, ShakeAnimation};
    use crate::statistics::earn::{
        BotEarnHistoryTab, ChannelEarnHistoryTab, format_earn_entry_amount, format_earn_entry_date,
    };
    use crate::statistics::sections::{
        RECENT_POSTS_FIRST_PAGE, format_public_forward_status, format_top_admin_status,
        format_top_inviter_status, format_top_sender_status,
    };

    #[test]
    fn shake_animation_follows_tdesktop_six_segment_curve() {
        assert_eq!(ShakeAnimation::evaluate_shift(0.0), 0.0);
        // End of segment 0 (p = 1/6) -> +SHAKE_SHIFT_PX (+4.0)
        assert_eq!(ShakeAnimation::evaluate_shift(1.0 / 6.0), 4.0);
        // End of segment 1 (p = 2/6) -> -4.0
        assert_eq!(ShakeAnimation::evaluate_shift(2.0 / 6.0), -4.0);
        // End of segment 2 (p = 3/6) -> +4.0
        assert_eq!(ShakeAnimation::evaluate_shift(3.0 / 6.0), 4.0);
        // End of segment 3 (p = 4/6) -> -4.0
        assert_eq!(ShakeAnimation::evaluate_shift(4.0 / 6.0), -4.0);
        // End of segment 4 (p = 5/6) -> +4.0
        assert_eq!(ShakeAnimation::evaluate_shift(5.0 / 6.0), 4.0);
        // End (p = 1.0) -> returns to 0.0
        assert_eq!(ShakeAnimation::evaluate_shift(1.0), 0.0);

        let mut shake = ShakeAnimation::default();
        shake.start(1_000);
        assert!(shake.is_animating());
        assert!(shake.tick(1_050)); // 50ms / 300ms = 1/6 -> +4.0
        assert_eq!(shake.shift_px(), 4.0);
        assert!(!shake.tick(1_300));
        assert_eq!(shake.shift_px(), 0.0);
    }

    #[test]
    fn chart_line_filter_toggles_and_shakes_on_last_line() {
        let backend = MockStatsBackend::new();
        let bundle = backend.channel_statistics().unwrap();
        let palette = ChartPalette::light();
        let mut chart = ChartWidgetState::new(
            "Followers",
            bundle.stats.join_graph,
            ChartViewType::Linear,
            &palette,
        );
        assert_eq!(chart.chips.len(), 2);

        // Disable line 1 ("Joined") -> succeeds and starts animation.
        assert!(chart.toggle_line(1, 100));
        assert!(!chart.filter.is_enabled(1));
        assert!(!chart.chips[0].checked);
        assert!(chart.anim.is_animating());

        // Attempting to disable line 2 ("Left") -> refused & triggers chip shake!
        assert!(!chart.toggle_line(2, 120));
        assert!(chart.filter.is_enabled(2));
        assert!(chart.chips[1].checked);
        assert!(chart.chips[1].shake.is_animating());

        // Tick forward to settle both filter animation and shake.
        let mut t = 120;
        while t < 2_000 {
            t += 16;
            chart.tick(t);
        }
        assert!(!chart.chips[1].shake.is_animating());
        assert_eq!(chart.filter.alpha(1), 0.0);
        assert_eq!(chart.filter.alpha(2), 1.0);
    }

    #[test]
    fn chart_remote_zoom_loads_hourly_details_through_backend_and_zooms_out() {
        let backend = MockStatsBackend::new();
        let bundle = backend.channel_statistics().unwrap();
        let palette = ChartPalette::light();
        let mut chart = ChartWidgetState::new(
            "Followers",
            bundle.stats.join_graph,
            ChartViewType::Linear,
            &palette,
        );
        assert!(chart.is_zoomable());
        assert!(!chart.is_zoomed());

        chart.trigger_zoom(5, &backend, &palette, 500).unwrap();
        assert!(chart.is_zoomed());
        let zoomed = chart.remote_zoomed().expect("remote zoomed chart present");
        assert_eq!(zoomed.meta.title, "Followers");
        assert_eq!(zoomed.meta.subtitle, "20 Nov 2023");
        assert_eq!(zoomed.widget.chart.x.len(), 24);
        assert!(zoomed.widget.chart.is_footer_hidden);

        chart.zoom_out(600);
        assert!(!chart.is_zoomed());
        assert!(chart.remote_zoomed().is_none());
    }

    #[test]
    fn stack_linear_local_zoom_transitions_to_pie_and_back() {
        let backend = MockStatsBackend::new();
        let bundle = backend.channel_statistics().unwrap();
        let palette = ChartPalette::light();
        let mut chart = ChartWidgetState::new(
            "Languages",
            bundle.stats.language_graph,
            ChartViewType::StackLinear,
            &palette,
        );
        assert!(chart.has_local_zoom());
        assert!(chart.is_zoomable());

        chart.trigger_zoom(6, &backend, &palette, 1_000).unwrap();
        assert!(chart.is_zoomed());
        // Advance 400ms local zoom animation.
        for step in 1..=30 {
            chart.tick(1_000 + step * 16);
        }
        assert!((chart.local_zoom_progress() - 1.0).abs() < 1e-6);

        // Zoom back out -> returns to 0.0 and exits local zoom.
        chart.zoom_out(2_000);
        for step in 1..=30 {
            chart.tick(2_000 + step * 16);
        }
        assert!(!chart.is_zoomed());
        assert!((chart.local_zoom_progress() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn footer_limits_update_rulers_and_subtitle() {
        let backend = MockStatsBackend::new();
        let bundle = backend.channel_statistics().unwrap();
        let palette = ChartPalette::light();
        let mut chart = ChartWidgetState::new(
            "Views by source",
            bundle.stats.view_count_by_source_graph,
            ChartViewType::StackBar,
            &palette,
        );
        let initial_sub = chart.subtitle.clone();
        chart.apply_x_percentage_limits(Limits::new(0.0, 0.4), 100);
        assert_ne!(chart.subtitle, initial_sub);
        assert!(chart.anim.is_animating());
        let mut t = 100;
        while chart.tick(t) && t < 4_000 {
            t += 16;
        }
        assert!(!chart.anim.is_animating());
    }

    #[test]
    fn sections_recent_posts_pagination_and_leaderboard_formatters() {
        let backend = MockStatsBackend::new();
        let palette = ChartPalette::light();
        let mut view = ChannelStatsView::new(backend.channel_statistics().unwrap(), &palette);
        assert_eq!(view.charts.len(), 12);
        assert_eq!(view.recent_posts_visible, RECENT_POSTS_FIRST_PAGE);
        assert!(view.bundle.recent_posts.len() > RECENT_POSTS_FIRST_PAGE);
        view.show_more_recent_posts();
        assert_eq!(view.recent_posts_visible, view.bundle.recent_posts.len());

        assert_eq!(
            format_top_sender_status(412, 84),
            "412 messages, 84 symbols per message"
        );
        assert_eq!(
            format_top_admin_status(42, 1, 11),
            "42 deletions, 1 ban, 11 restrictions"
        );
        assert_eq!(format_top_inviter_status(1), "1 invitation");
        assert_eq!(format_top_inviter_status(34), "34 invitations");
        assert_eq!(
            format_public_forward_status(24_600, 6_340),
            "24600 subscribers, 6340 views"
        );
        assert_eq!(format_public_forward_status(0, 0), "No views yet");
    }

    #[test]
    fn boosts_view_model_progress_formatting_and_pagination() {
        let backend = MockStatsBackend::new();
        let mut view = BoostsView::new(backend.boost_status().unwrap());
        assert_eq!(view.active_tab, BoostsListTab::Boosts);

        let ov = BoostsOverview {
            level: 4,
            boost_count: 19,
            current_level_boost_count: 15,
            next_level_boost_count: 25,
            ..Default::default()
        };
        assert!((compute_boost_level_progress(&ov) - 0.4).abs() < 1e-6);

        let unclaimed = Boost {
            is_unclaimed: true,
            date: 1_700_524_800,
            expires_after_months: 3,
            ..Default::default()
        };
        assert_eq!(format_boost_title(&unclaimed, None), "Unclaimed");
        assert_eq!(format_boost_status(&unclaimed, false), "3 months • 21 Nov");

        let stars_boost = Boost {
            credits: 2_500,
            expires_at: 1_700_611_200,
            ..Default::default()
        };
        assert_eq!(format_boost_title(&stars_boost, None), "2500 Stars");
        assert_eq!(
            format_boost_status(&stars_boost, false),
            "boost expires on 22 Nov"
        );

        let prepaid = BoostPrepaidGiveaway {
            quantity: 5,
            months: 3,
            boosts: 0,
            ..Default::default()
        };
        let (title, sub, boosts) = format_prepaid_giveaway_labels(&prepaid, 4);
        assert_eq!(title, "5 Telegram Premium");
        assert_eq!(sub, "5 subscriptions 3-month subscriptions");
        assert_eq!(boosts, 20);

        let before = view.bundle.status.first_slice_boosts.list.len();
        assert!(!view.bundle.status.first_slice_boosts.all_loaded);
        view.load_more(&backend);
        assert!(view.bundle.status.first_slice_boosts.all_loaded);
        assert!(view.bundle.status.first_slice_boosts.list.len() > before);
    }

    #[test]
    fn earn_views_formatting_pagination_and_filtering() {
        let backend = MockStatsBackend::new();
        let palette = ChartPalette::light();

        let mut ch = ChannelEarnView::new(backend.channel_earn().unwrap(), &palette);
        assert_eq!(ch.history_tab, ChannelEarnHistoryTab::Ton);
        assert_eq!(ch.charts.len(), 3);
        let before = ch.bundle.ton.first_history_slice.list.len();
        ch.load_more_ton_history(&backend);
        assert!(ch.bundle.ton.first_history_slice.all_loaded);
        assert!(ch.bundle.ton.first_history_slice.list.len() > before);

        let incoming_ton = EarnHistoryEntry {
            incoming: true,
            credits: CreditsAmount::from_nanoton(6_570_000_000),
            date: 1_700_784_000,
            ads_proceeds_to_date: 1_701_129_600,
            ..Default::default()
        };
        assert_eq!(format_earn_entry_amount(&incoming_ton), "+6.57 TON");
        assert_eq!(
            format_earn_entry_date(&incoming_ton),
            "24 Nov — 28 Nov 2023"
        );

        let failed_out = EarnHistoryEntry {
            incoming: false,
            failed: true,
            credits: CreditsAmount::from_whole(500, CreditsType::Stars),
            date: 1_700_006_400,
            ..Default::default()
        };
        assert_eq!(format_earn_entry_amount(&failed_out), "\u{2212}500 ★");
        assert!(format_earn_entry_date(&failed_out).ends_with("Failed"));

        let mut bot = BotEarnView::new(backend.bot_earn().unwrap(), &palette);
        assert_eq!(bot.history_tab, BotEarnHistoryTab::All);
        let all_len = bot.active_history_entries().len();
        bot.history_tab = BotEarnHistoryTab::Incoming;
        let in_len = bot.active_history_entries().len();
        bot.history_tab = BotEarnHistoryTab::Outgoing;
        let out_len = bot.active_history_entries().len();
        assert_eq!(in_len + out_len, all_len);
    }

    #[test]
    fn screen_host_navigation_stack_and_theme_switching() {
        let mut host = StatisticsScreenHost::with_mock(ThemeMode::Day);
        assert_eq!(host.active_screen(), StatisticsScreen::Channel);
        assert!(!host.can_go_back());

        host.push_screen(StatisticsScreen::Message(901));
        assert_eq!(host.active_screen(), StatisticsScreen::Message(901));
        assert!(host.can_go_back());

        assert!(host.go_back());
        assert_eq!(host.active_screen(), StatisticsScreen::Channel);
        assert!(!host.can_go_back());

        host.select_top_screen(StatisticsScreen::Boosts);
        assert!(host.boosts_view().is_some());
        host.select_top_screen(StatisticsScreen::ChannelEarn);
        assert!(host.channel_earn_view().is_some());
        host.select_top_screen(StatisticsScreen::BotEarn);
        assert!(host.bot_earn_view().is_some());

        host.sync_theme(ThemeMode::Night);
        assert_eq!(*host.palette(), chart_palette_for_mode(ThemeMode::Night));
        assert_ne!(
            chart_palette_for_mode(ThemeMode::Night).box_bg,
            chart_palette_for_mode(ThemeMode::Day).box_bg
        );
    }

    // ========================================================================
    // Headless egui mesh software rasterizer + pure-Rust PNG encoder for docs
    // ========================================================================

    struct SoftwareTexture {
        width: usize,
        height: usize,
        /// Premultiplied sRGBA pixels (`Color32::to_array()`).
        pixels: Vec<[u8; 4]>,
    }

    fn render_egui_to_png(
        logical_w: u32,
        logical_h: u32,
        ppp: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        ensure_tdesktop_fonts(&ctx);
        let palette = Palette::day();
        ctx.set_visuals(palette.to_egui_visuals(ThemeMode::Day));

        let mut textures: HashMap<TextureId, SoftwareTexture> = HashMap::new();
        let mut last_output = None;

        // Run 3 warm-up frames so layout, font atlas, and widget widths settle cleanly.
        for frame in 0..3 {
            let raw_input = RawInput {
                screen_rect: Some(Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(logical_w as f32, logical_h as f32),
                )),
                time: Some(frame as f64 * 0.016),
                ..Default::default()
            };
            ctx.set_pixels_per_point(ppp);
            let mut output = ctx.run_ui(raw_input, |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| draw(ui));
            });

            for (id, deltas) in &output.textures_delta.set {
                for delta in deltas {
                    let egui::ImageData::Color(ref color_img) = delta.image;
                    let [iw, ih] = color_img.size;
                    if let Some([ox, oy]) = delta.pos {
                        if let Some(tex) = textures.get_mut(id) {
                            for y in 0..ih {
                                for x in 0..iw {
                                    let dst_x = ox + x;
                                    let dst_y = oy + y;
                                    if dst_x < tex.width && dst_y < tex.height {
                                        tex.pixels[dst_y * tex.width + dst_x] =
                                            color_img.pixels[y * iw + x].to_array();
                                    }
                                }
                            }
                        }
                    } else {
                        let pixels = color_img.pixels.iter().map(|c| c.to_array()).collect();
                        textures.insert(
                            *id,
                            SoftwareTexture {
                                width: iw,
                                height: ih,
                                pixels,
                            },
                        );
                    }
                }
            }
            for free_id in &output.textures_delta.free {
                textures.remove(free_id);
            }
            output.textures_delta.clear();
            last_output = Some(output);
        }

        let output = last_output.expect("egui produced output");
        let clipped = ctx.tessellate(output.shapes, ppp);

        let phys_w = ((logical_w as f32) * ppp).round() as usize;
        let phys_h = ((logical_h as f32) * ppp).round() as usize;
        let mut fb = vec![[255u8, 255u8, 255u8, 255u8]; phys_w * phys_h];

        for prim in clipped {
            let Primitive::Mesh(mesh) = prim.primitive else {
                continue;
            };
            let tex = textures.get(&mesh.texture_id);
            let clip_min_x = (prim.clip_rect.min.x * ppp).floor().max(0.0) as i32;
            let clip_min_y = (prim.clip_rect.min.y * ppp).floor().max(0.0) as i32;
            let clip_max_x = (prim.clip_rect.max.x * ppp).ceil().min(phys_w as f32) as i32;
            let clip_max_y = (prim.clip_rect.max.y * ppp).ceil().min(phys_h as f32) as i32;

            for tri in mesh.indices.as_chunks::<3>().0 {
                let v0 = mesh.vertices[tri[0] as usize];
                let v1 = mesh.vertices[tri[1] as usize];
                let v2 = mesh.vertices[tri[2] as usize];

                let x0 = v0.pos.x * ppp;
                let y0 = v0.pos.y * ppp;
                let x1 = v1.pos.x * ppp;
                let y1 = v1.pos.y * ppp;
                let x2 = v2.pos.x * ppp;
                let y2 = v2.pos.y * ppp;

                let area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
                if area.abs() < 1e-6 {
                    continue;
                }
                let inv_area = 1.0 / area;

                let min_x = x0.min(x1).min(x2).floor() as i32;
                let max_x = x0.max(x1).max(x2).ceil() as i32;
                let min_y = y0.min(y1).min(y2).floor() as i32;
                let max_y = y0.max(y1).max(y2).ceil() as i32;

                let bx0 = min_x.max(clip_min_x);
                let bx1 = max_x.min(clip_max_x - 1);
                let by0 = min_y.max(clip_min_y);
                let by1 = max_y.min(clip_max_y - 1);
                if bx0 > bx1 || by0 > by1 {
                    continue;
                }

                let c0 = v0.color.to_array();
                let c1 = v1.color.to_array();
                let c2 = v2.color.to_array();

                for py in by0..=by1 {
                    let cy = py as f32 + 0.5;
                    for px in bx0..=bx1 {
                        let cx = px as f32 + 0.5;
                        let w0 = ((x1 - cx) * (y2 - cy) - (y1 - cy) * (x2 - cx)) * inv_area;
                        let w1 = ((x2 - cx) * (y0 - cy) - (y2 - cy) * (x0 - cx)) * inv_area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                            continue;
                        }

                        // Vertex premultiplied RGBA
                        let vr =
                            (w0 * f32::from(c0[0]) + w1 * f32::from(c1[0]) + w2 * f32::from(c2[0]))
                                .clamp(0.0, 255.0);
                        let vg =
                            (w0 * f32::from(c0[1]) + w1 * f32::from(c1[1]) + w2 * f32::from(c2[1]))
                                .clamp(0.0, 255.0);
                        let vb =
                            (w0 * f32::from(c0[2]) + w1 * f32::from(c1[2]) + w2 * f32::from(c2[2]))
                                .clamp(0.0, 255.0);
                        let va =
                            (w0 * f32::from(c0[3]) + w1 * f32::from(c1[3]) + w2 * f32::from(c2[3]))
                                .clamp(0.0, 255.0);

                        let (sr, sg, sb, sa) = if let Some(t) = tex {
                            let u = (w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x).clamp(0.0, 1.0);
                            let v = (w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y).clamp(0.0, 1.0);
                            let [tr, tg, tb, ta] = sample_bilinear(t, u, v);
                            (
                                vr * tr / 255.0,
                                vg * tg / 255.0,
                                vb * tb / 255.0,
                                va * ta / 255.0,
                            )
                        } else {
                            (vr, vg, vb, va)
                        };

                        if sa <= 0.5 {
                            continue;
                        }
                        let dst = &mut fb[(py as usize) * phys_w + (px as usize)];
                        let inv_a = 1.0 - sa / 255.0;
                        dst[0] = (sr + f32::from(dst[0]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[1] = (sg + f32::from(dst[1]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[2] = (sb + f32::from(dst[2]) * inv_a).round().clamp(0.0, 255.0) as u8;
                        dst[3] = 255;
                    }
                }
            }
        }

        encode_rgba_png(phys_w as u32, phys_h as u32, &fb)
    }

    fn sample_bilinear(tex: &SoftwareTexture, u: f32, v: f32) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [255.0, 255.0, 255.0, 255.0];
        }
        let fx = (u * (tex.width as f32) - 0.5).clamp(0.0, (tex.width - 1) as f32);
        let fy = (v * (tex.height as f32) - 0.5).clamp(0.0, (tex.height - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(tex.width - 1);
        let y1 = (y0 + 1).min(tex.height - 1);
        let tx = fx - (x0 as f32);
        let ty = fy - (y0 as f32);

        let p00 = tex.pixels[y0 * tex.width + x0];
        let p10 = tex.pixels[y0 * tex.width + x1];
        let p01 = tex.pixels[y1 * tex.width + x0];
        let p11 = tex.pixels[y1 * tex.width + x1];

        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let top = f32::from(p00[i]) * (1.0 - tx) + f32::from(p10[i]) * tx;
            let bot = f32::from(p01[i]) * (1.0 - tx) + f32::from(p11[i]) * tx;
            out[i] = top * (1.0 - ty) + bot * ty;
        }
        out
    }

    /// Minimal pure-Rust RFC 2083 PNG encoder (uncompressed zlib stored blocks, zero extra crates).
    fn encode_rgba_png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut raw = Vec::with_capacity((height as usize) * (1 + (width as usize) * 4));
        for y in 0..(height as usize) {
            raw.push(0); // filter type 0 (None)
            let row = &pixels[y * (width as usize)..(y + 1) * (width as usize)];
            for &[r, g, b, a] in row {
                raw.extend_from_slice(&[r, g, b, a]);
            }
        }

        // Zlib wrapper (CMF=0x78, FLG=0x01) + stored DEFLATE blocks + Adler-32.
        let mut zlib = Vec::with_capacity(raw.len() + (raw.len() / 65_535 + 1) * 5 + 6);
        zlib.push(0x78);
        zlib.push(0x01);
        let mut offset = 0usize;
        while offset < raw.len() {
            let remaining = raw.len() - offset;
            let chunk_len = remaining.min(65_535);
            let is_final = offset + chunk_len >= raw.len();
            zlib.push(u8::from(is_final));
            let len_u16 = chunk_len as u16;
            zlib.extend_from_slice(&len_u16.to_le_bytes());
            zlib.extend_from_slice(&(!len_u16).to_le_bytes());
            zlib.extend_from_slice(&raw[offset..offset + chunk_len]);
            offset += chunk_len;
        }
        let adler = adler32(&raw);
        zlib.extend_from_slice(&adler.to_be_bytes());

        let mut png = Vec::with_capacity(zlib.len() + 64);
        png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        write_png_chunk(&mut png, *b"IHDR", &ihdr);
        write_png_chunk(&mut png, *b"IDAT", &zlib);
        write_png_chunk(&mut png, *b"IEND", &[]);
        png
    }

    fn write_png_chunk(out: &mut Vec<u8>, chunk_type: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&chunk_type);
        out.extend_from_slice(data);
        let mut crc = crc32_update(0xFFFF_FFFF, &chunk_type);
        crc = crc32_update(crc, data);
        out.extend_from_slice(&(!crc).to_be_bytes());
    }

    fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB8_8320;
                } else {
                    crc >>= 1;
                }
            }
        }
        crc
    }

    fn adler32(bytes: &[u8]) -> u32 {
        const MOD: u32 = 65_521;
        let mut s1 = 1u32;
        let mut s2 = 0u32;
        for &b in bytes {
            s1 = (s1 + u32::from(b)) % MOD;
            s2 = (s2 + s1) % MOD;
        }
        (s2 << 16) | s1
    }

    #[test]
    fn renders_channel_statistics_and_zoomed_chart_screenshots_via_egui() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }

        // 1. Channel Statistics page screenshot (overview + Growth + Followers with hover popup + footer + chips).
        let mut host = StatisticsScreenHost::with_mock(ThemeMode::Day);
        if let Some(ch) = host.channel_view_mut() {
            // Pin hover details popup on the Followers chart (index 1) so the screenshot showcases
            // the interactive tooltip card, vertical guide, colored dots, footer handles, and filter chips!
            if let Some(followers) = ch.charts.get_mut(1) {
                let zoomable = followers.is_zoomable();
                followers
                    .popup
                    .set_pinned(&followers.chart, 9, zoomable, &followers.filter);
            }
        }

        let channel_png = render_egui_to_png(600, 1120, 1.5, |ui| {
            host.show_at(ui, false, 1_000);
        });
        assert!(channel_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            let channel_path = docs_dir.join("statistics_channel.png");
            std::fs::write(&channel_path, &channel_png).expect("write statistics_channel.png");
        }

        // 2. Zoomed Chart screenshot: top shows remote day-zoomed 24h Interactions chart (DoubleLinear
        //    loaded via StatsBackend::load_zoom_graph with Zoom Out button & hover popup),
        //    bottom shows local pie-zoomed Languages chart (StackLinear -> Pie with exploded slice + detail card).
        let backend = MockStatsBackend::new();
        let bundle = backend.channel_statistics().unwrap();
        let palette = ChartPalette::light();

        let mut interactions = ChartWidgetState::new(
            tr::lng_chart_title_message_interaction(),
            bundle.stats.message_interaction_graph,
            ChartViewType::DoubleLinear,
            &palette,
        );
        interactions
            .trigger_zoom(6, &backend, &palette, 1_000)
            .unwrap();
        if let Some(zoomed) = interactions.remote_zoomed_mut() {
            zoomed
                .widget
                .popup
                .set_pinned(&zoomed.widget.chart, 14, false, &zoomed.widget.filter);
        }

        let mut languages = ChartWidgetState::new(
            tr::lng_chart_title_language(),
            bundle.stats.language_graph,
            ChartViewType::StackLinear,
            &palette,
        );
        languages
            .trigger_zoom(6, &backend, &palette, 1_000)
            .unwrap();
        for step in 1..=30 {
            languages.tick(1_000 + step * 16);
        }
        languages.set_hovered_pie_line(Some(1), 2_000.0);

        let zoomed_png = render_egui_to_png(600, 730, 1.5, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, to_color32(palette.box_bg));
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(strings::ZOOMED_CHARTS_CAPTURE_TITLE)
                            .size(16.0)
                            .strong()
                            .color(to_color32(palette.box_text_fg)),
                    );
                    ui.add_space(10.0);
                    interactions.show(ui, &backend, &palette, 2_500);
                    crate::statistics::sections::render_section_divider(ui, &palette);
                    languages.show(ui, &backend, &palette, 2_500);
                });
        });
        assert!(zoomed_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            let zoomed_path = docs_dir.join("statistics_zoomed.png");
            std::fs::write(&zoomed_path, &zoomed_png).expect("write statistics_zoomed.png");
        }
    }

    #[test]
    fn mock_backend_serves_all_sample_charts_non_empty() {
        let backend = MockStatsBackend::new();
        let ch = backend.channel_statistics().unwrap();
        assert!(!ch.stats.join_graph.chart.is_empty());
        assert!(!ch.stats.view_count_by_source_graph.chart.is_empty());
        assert!(!ch.stats.language_graph.chart.is_empty());
        assert!(!ch.stats.message_interaction_graph.chart.is_empty());
        assert!(!ch.stats.reactions_by_emotion_graph.chart.is_empty());

        let sg = backend.supergroup_statistics().unwrap();
        assert!(!sg.lists.top_senders.is_empty());
        assert!(!sg.lists.top_administrators.is_empty());
        assert!(!sg.lists.top_inviters.is_empty());

        let msg = backend.message_statistics(901).unwrap();
        assert!(!msg.stats.is_empty());
        assert!(!msg.public_forwards.is_empty());

        let story = backend.story_statistics(401).unwrap();
        assert!(story.post.is_story);
        assert!(!story.stats.is_empty());

        let zoomed = backend
            .load_zoom_graph("zoom_ton_revenue", 1_700_611_200_000.0)
            .unwrap();
        assert_eq!(zoomed.chart.x.len(), 24);
        assert!(zoomed.chart.currency_rate > 0.0);

        let empty_zoom = backend.load_zoom_graph("", 0.0);
        assert!(empty_zoom.is_err());

        // Sanity check zero-data chart does not panic during construction.
        let empty = ChartWidgetState::new(
            "Empty",
            StatisticalGraph {
                chart: StatisticalChart::default(),
                zoom_token: String::new(),
                error: "No data".to_owned(),
            },
            ChartViewType::Linear,
            &ChartPalette::light(),
        );
        assert_eq!(empty.zoom_error.as_deref(), Some("No data"));
    }
}
