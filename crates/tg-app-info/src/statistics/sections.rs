//! Channel, Supergroup, Message, and Story statistics screens.
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/statistics/info_statistics_inner_widget.{h,cpp}`
//! - `Telegram/SourceFiles/info/statistics/info_statistics_recent_message.{h,cpp}`
//! - `Telegram/SourceFiles/info/statistics/info_statistics_list_controllers.{h,cpp}`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_stats::{
    ChartPalette, ChartViewType, OverviewBlock, StatisticalGraph, channel_overview_blocks,
    format_count_decimal, lang_detailed_day_month, message_overview_block,
    supergroup_overview_block,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::statistics::backend::{
    ChannelStatsBundle, MessageStatsBundle, PeerDisplayInfo, RecentPostPreview, StatsBackend,
    SupergroupStatsBundle,
};
use crate::statistics::chart_widget::{ChartWidgetState, to_color32, to_color32_alpha};
use crate::statistics::{paint_stats_icon, strings};

/// Initial page size for recent posts (`kFirstPage` in `info_statistics_inner_widget.cpp:910`).
pub const RECENT_POSTS_FIRST_PAGE: usize = 10;
/// Additional rows revealed per `"Show more"` click (`kPerPage` in `info_statistics_inner_widget.cpp:911`).
pub const RECENT_POSTS_PER_PAGE: usize = 30;
/// Initial page size for supergroup leaderboard lists (`Top Members`, `Top Admins`, `Top Inviters`).
pub const LEADERBOARD_FIRST_PAGE: usize = 5;

/// Navigation action emitted by statistics sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionNavAction {
    /// Open statistics for a specific channel post.
    OpenMessageStats(i32),
    /// Open statistics for a specific channel story.
    OpenStoryStats(i32),
}

/// Renders a horizontal section divider matching `Ui::AddDivider` (`boxDividerBg`).
pub fn render_section_divider(ui: &mut Ui, palette: &ChartPalette) {
    ui.add_space(6.0);
    let avail_w = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 8.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, to_color32(palette.box_divider_bg));
    let hairline = Stroke::new(1.0, to_color32_alpha(palette.box_text_fg, 0.06));
    painter.line_segment([rect.left_top(), rect.right_top()], hairline);
    painter.line_segment([rect.left_bottom(), rect.right_bottom()], hairline);
    ui.add_space(8.0);
}

/// Renders a section title header in Telegram blue (`Statistic::Header`).
pub fn render_section_header(ui: &mut Ui, title: &str, subtitle: &str, palette: &ChartPalette) {
    let h = if subtitle.is_empty() { 20.0 } else { 36.0 };
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width().max(1.0), h), Sense::hover());
    let painter = ui.painter();
    painter.text(
        rect.min,
        Align2::LEFT_TOP,
        title,
        font_id_from_style(st::statistics_header_title_text_style.font),
        to_color32(palette.window_active_text_fg),
    );
    if !subtitle.is_empty() {
        painter.text(
            Pos2::new(rect.left(), rect.top() + 20.0),
            Align2::LEFT_TOP,
            subtitle,
            font_id_from_style(st::statistics_header_dates_text_style.font),
            to_color32(palette.window_sub_text_fg),
        );
    }
}

/// Renders a 2x2 [`OverviewBlock`] matching `FillOverview` (`info_statistics_inner_widget.cpp`).
pub fn render_overview_block(ui: &mut Ui, block: &OverviewBlock, palette: &ChartPalette) {
    if !block.title.is_empty() {
        render_section_header(ui, &block.title, &block.date_range, palette);
        ui.add_space(8.0);
    }

    let avail_w = ui.available_width().max(240.0);
    let col_w = (avail_w - 14.0) / 2.0;
    let value_font = font_id_from_style(st::statistics_overview_value.style.font);
    let second_font = font_id_from_style(st::statistics_overview_second_value.style.font);
    let subtext_font = font_id_from_style(st::statistics_overview_subtext.style.font);

    for row_cards in block.cards.chunks(2) {
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 46.0), Sense::hover());
        let painter = ui.painter();

        for (col_idx, card) in row_cards.iter().enumerate() {
            let x = row_rect.left() + (col_idx as f32) * (col_w + 14.0);
            let y = row_rect.top();

            let primary_galley = ui.fonts_mut(|f| {
                f.layout_no_wrap(
                    card.primary.clone(),
                    value_font.clone(),
                    to_color32(palette.box_text_fg),
                )
            });
            let primary_w = primary_galley.size().x;
            painter.galley(
                Pos2::new(x, y),
                primary_galley,
                to_color32(palette.box_text_fg),
            );

            if let Some(ref delta) = card.delta {
                painter.text(
                    Pos2::new(x + primary_w + 6.0, y + 3.0),
                    Align2::LEFT_TOP,
                    &delta.text,
                    second_font.clone(),
                    to_color32(delta.color),
                );
            }

            painter.text(
                Pos2::new(x, y + 22.0),
                Align2::LEFT_TOP,
                &card.caption,
                subtext_font.clone(),
                to_color32(palette.window_sub_text_fg),
            );
        }
        ui.add_space(4.0);
    }
}

/// Renders a circular monogram avatar (or gradient-ring story avatar).
pub fn paint_avatar(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    rgb: [u8; 3],
    initials: &str,
    story_ring: bool,
) {
    let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    if story_ring {
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(2.0, Color32::from_rgb(0x40, 0xA7, 0xE3)),
        );
        painter.circle_filled(center, (radius - 3.0).max(4.0), fill);
    } else {
        painter.circle_filled(center, radius, fill);
    }
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional((radius * 0.72).clamp(10.0, 16.0)),
        Color32::WHITE,
    );
}

/// Formats pluralized English count strings matching `FormatText` in `info_statistics_list_controllers.cpp`.
#[must_use]
pub fn format_plural(count: i32, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", format_count_decimal(i64::from(count)))
    }
}

/// Formats a supergroup top-sender status line (`tr::lng_stats_member_messages` / `tr::lng_stats_member_characters`).
#[must_use]
pub fn format_top_sender_status(messages: i32, avg_chars: i32) -> String {
    let mut parts = Vec::new();
    if messages > 0 {
        parts.push(tr::lng_stats_member_messages(messages));
    }
    if avg_chars > 0 {
        parts.push(tr::lng_stats_member_characters(avg_chars));
    }
    parts.join(", ")
}

/// Formats a supergroup administrator actions status line (`tr::lng_stats_member_deletions` / `bans` / `restrictions`).
#[must_use]
pub fn format_top_admin_status(deletions: i32, bans: i32, restrictions: i32) -> String {
    let mut parts = Vec::new();
    if deletions > 0 {
        parts.push(tr::lng_stats_member_deletions(deletions));
    }
    if bans > 0 {
        parts.push(tr::lng_stats_member_bans(bans));
    }
    if restrictions > 0 {
        parts.push(tr::lng_stats_member_restrictions(restrictions));
    }
    parts.join(", ")
}

/// Formats a supergroup inviter status line (`tr::lng_stats_member_invitations`).
#[must_use]
pub fn format_top_inviter_status(added: i32) -> String {
    tr::lng_stats_member_invitations(added)
}

/// Formats a public forward status line (`tr::lng_chat_status_subscribers` / `tr::lng_stats_recent_messages_views`).
#[must_use]
pub fn format_public_forward_status(subscribers: i32, views: i32) -> String {
    let mut parts = Vec::new();
    if subscribers > 0 {
        parts.push(tr::lng_chat_status_subscribers(subscribers));
    }
    if views > 0 {
        parts.push(tr::lng_stats_recent_messages_views(views));
    }
    if parts.is_empty() {
        tr::lng_stories_no_views()
    } else {
        parts.join(", ")
    }
}

fn push_chart_if_present(
    charts: &mut Vec<ChartWidgetState>,
    graph: &StatisticalGraph,
    title: String,
    view_type: ChartViewType,
    palette: &ChartPalette,
) {
    if !graph.chart.is_empty() {
        charts.push(ChartWidgetState::new(
            title,
            graph.clone(),
            view_type,
            palette,
        ));
    }
}

// ============================================================================
// Channel Statistics Screen
// ============================================================================

/// Stateful view for Broadcast Channel Statistics (`Info::Statistics::InnerWidget` channel mode).
#[derive(Debug, Clone)]
pub struct ChannelStatsView {
    /// Loaded bundle (peer, overview metrics, recent posts).
    pub bundle: ChannelStatsBundle,
    /// Interactive channel chart widgets in exact tdesktop order.
    pub charts: Vec<ChartWidgetState>,
    /// Number of visible recent posts (`kFirstPage = 10`, expands by `kPerPage = 30`).
    pub recent_posts_visible: usize,
}

impl ChannelStatsView {
    /// Creates a new channel statistics view from `bundle`.
    #[must_use]
    pub fn new(bundle: ChannelStatsBundle, palette: &ChartPalette) -> Self {
        let s = &bundle.stats;
        let mut charts = Vec::with_capacity(12);
        push_chart_if_present(
            &mut charts,
            &s.member_count_graph,
            tr::lng_chart_title_member_count(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.join_graph,
            tr::lng_chart_title_join(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.mute_graph,
            tr::lng_chart_title_mute(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.view_count_by_hour_graph,
            tr::lng_chart_title_view_count_by_hour(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.view_count_by_source_graph,
            tr::lng_chart_title_view_count_by_source(),
            ChartViewType::StackBar,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.join_by_source_graph,
            tr::lng_chart_title_join_by_source(),
            ChartViewType::StackBar,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.language_graph,
            tr::lng_chart_title_language(),
            ChartViewType::StackLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.message_interaction_graph,
            tr::lng_chart_title_message_interaction(),
            ChartViewType::DoubleLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.instant_view_interaction_graph,
            tr::lng_chart_title_instant_view_interaction(),
            ChartViewType::DoubleLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.reactions_by_emotion_graph,
            tr::lng_chart_title_reactions_by_emotion(),
            ChartViewType::Bar,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.story_interactions_graph,
            tr::lng_chart_title_story_interactions(),
            ChartViewType::DoubleLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.story_reactions_by_emotion_graph,
            tr::lng_chart_title_story_reactions_by_emotion(),
            ChartViewType::Bar,
            palette,
        );

        let initial_visible = bundle.recent_posts.len().min(RECENT_POSTS_FIRST_PAGE);
        Self {
            bundle,
            charts,
            recent_posts_visible: initial_visible,
        }
    }

    /// Re-applies theme colors to all channel charts.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        for chart in &mut self.charts {
            chart.apply_palette(palette);
        }
    }

    /// Expands the recent posts list by `RECENT_POSTS_PER_PAGE`.
    pub fn show_more_recent_posts(&mut self) {
        self.recent_posts_visible =
            (self.recent_posts_visible + RECENT_POSTS_PER_PAGE).min(self.bundle.recent_posts.len());
    }

    /// Renders the channel statistics page and returns any clicked post/story navigation action.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) -> Option<SectionNavAction> {
        let mut action = None;
        let (primary, secondary) = channel_overview_blocks(&self.bundle.stats, palette);

        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 14))
            .show(ui, |ui| {
                render_overview_block(ui, &primary, palette);
                ui.add_space(4.0);
                render_overview_block(ui, &secondary, palette);
            });

        for chart in &mut self.charts {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 6))
                .show(ui, |ui| {
                    chart.show(ui, backend, palette, now_ms);
                });
        }

        // Recent Posts & Stories list (`InnerWidget::fillRecentPosts`).
        if !self.bundle.recent_posts.is_empty() {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    render_section_header(
                        ui,
                        &tr::lng_stats_recent_messages_title(),
                        &primary.date_range,
                        palette,
                    );
                    ui.add_space(6.0);

                    let visible = self
                        .recent_posts_visible
                        .min(self.bundle.recent_posts.len());
                    for post in &self.bundle.recent_posts[..visible] {
                        if render_recent_post_row(ui, post, palette) {
                            action = Some(if post.is_story {
                                SectionNavAction::OpenStoryStats(post.interaction.story_id)
                            } else {
                                SectionNavAction::OpenMessageStats(post.interaction.message_id)
                            });
                        }
                    }

                    if self.recent_posts_visible < self.bundle.recent_posts.len() {
                        ui.add_space(6.0);
                        if render_show_more_button(ui, &tr::lng_stories_show_more(), palette) {
                            self.show_more_recent_posts();
                        }
                    }
                });
        }

        action
    }
}

fn render_recent_post_row(ui: &mut Ui, post: &RecentPostPreview, palette: &ChartPalette) -> bool {
    let avail_w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 56.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.box_text_fg, 0.04),
        );
    }

    let painter = ui.painter();
    let avatar_center = Pos2::new(rect.left() + 24.0, rect.center().y);
    paint_avatar(
        painter,
        avatar_center,
        20.0,
        post.avatar_rgb,
        &post.initials,
        post.is_story,
    );

    let views_text = tr::lng_stats_recent_messages_views(post.interaction.views_count);
    let sub_fg = to_color32(palette.window_sub_text_fg);
    let sub_font = FontId::proportional(11.5);

    // Right column: views (top) + real tg-style reactions & shares icons (bottom).
    let right_x = rect.right() - 8.0;
    let views_galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            views_text,
            FontId::proportional(13.0),
            to_color32(palette.box_text_fg),
        )
    });
    let right_col_w = views_galley.size().x.max(116.0);
    painter.galley(
        Pos2::new(right_x - views_galley.size().x, rect.top() + 9.0),
        views_galley,
        to_color32(palette.box_text_fg),
    );

    let icon_skip = st::statistics_recent_post_icon_skip.0 as f32;
    let mut cursor_x = right_x;
    let sub_y = rect.top() + 30.0;
    if post.interaction.forwards_count > 0 {
        let txt = format_count_decimal(i64::from(post.interaction.forwards_count));
        let g = ui.fonts_mut(|f| f.layout_no_wrap(txt, sub_font.clone(), sub_fg));
        cursor_x -= g.size().x;
        let icon_cy = sub_y + g.size().y * 0.5;
        painter.galley(Pos2::new(cursor_x, sub_y), g, sub_fg);
        cursor_x -= 14.0 + icon_skip.min(3.0);
        let icon_rect =
            Rect::from_center_size(Pos2::new(cursor_x + 6.0, icon_cy), Vec2::splat(13.0));
        paint_stats_icon(
            painter,
            icon_rect,
            st::statistics_recent_post_share_icon,
            palette,
            Some(sub_fg),
        );
        cursor_x -= 8.0;
    }
    if post.interaction.reactions_count > 0 {
        let txt = format_count_decimal(i64::from(post.interaction.reactions_count));
        let g = ui.fonts_mut(|f| f.layout_no_wrap(txt, sub_font, sub_fg));
        cursor_x -= g.size().x;
        let icon_cy = sub_y + g.size().y * 0.5;
        painter.galley(Pos2::new(cursor_x, sub_y), g, sub_fg);
        cursor_x -= 14.0 + icon_skip.min(3.0);
        let icon_rect =
            Rect::from_center_size(Pos2::new(cursor_x + 6.0, icon_cy), Vec2::splat(13.0));
        paint_stats_icon(
            painter,
            icon_rect,
            st::statistics_recent_post_reaction_icon,
            palette,
            Some(sub_fg),
        );
    }

    // Left column: snippet (elided) + date.
    let text_left = rect.left() + 56.0;
    let max_text_w = (avail_w - 56.0 - right_col_w - 20.0).max(60.0);
    let snippet_elided = elide_text(ui, &post.snippet, FontId::proportional(13.0), max_text_w);
    painter.text(
        Pos2::new(text_left, rect.top() + 9.0),
        Align2::LEFT_TOP,
        &snippet_elided,
        FontId::proportional(13.0),
        to_color32(palette.box_text_fg),
    );
    let date_label = lang_detailed_day_month(post.date_unix);
    painter.text(
        Pos2::new(text_left, rect.top() + 30.0),
        Align2::LEFT_TOP,
        &date_label,
        FontId::proportional(11.5),
        sub_fg,
    );

    resp.clicked()
}

// ============================================================================
// Supergroup Statistics Screen
// ============================================================================

/// Stateful view for Supergroup Statistics (`Info::Statistics::InnerWidget` supergroup mode).
#[derive(Debug, Clone)]
pub struct SupergroupStatsView {
    /// Loaded supergroup stats and member leaderboards.
    pub bundle: SupergroupStatsBundle,
    /// Interactive supergroup chart widgets.
    pub charts: Vec<ChartWidgetState>,
    /// Visible top-senders count.
    pub top_senders_visible: usize,
    /// Visible top-admins count.
    pub top_admins_visible: usize,
    /// Visible top-inviters count.
    pub top_inviters_visible: usize,
}

impl SupergroupStatsView {
    /// Creates a new supergroup statistics view.
    #[must_use]
    pub fn new(bundle: SupergroupStatsBundle, palette: &ChartPalette) -> Self {
        let s = &bundle.stats;
        let mut charts = Vec::with_capacity(8);
        push_chart_if_present(
            &mut charts,
            &s.member_count_graph,
            tr::lng_chart_title_member_count(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.join_graph,
            tr::lng_chart_title_group_join(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.join_by_source_graph,
            tr::lng_chart_title_group_join_by_source(),
            ChartViewType::StackBar,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.language_graph,
            tr::lng_chart_title_group_language(),
            ChartViewType::StackLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.message_content_graph,
            tr::lng_chart_title_group_message_content(),
            ChartViewType::StackBar,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.action_graph,
            tr::lng_chart_title_group_action(),
            ChartViewType::DoubleLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.day_graph,
            tr::lng_chart_title_group_day(),
            ChartViewType::Linear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &s.week_graph,
            tr::lng_chart_title_group_week(),
            ChartViewType::StackLinear,
            palette,
        );

        let top_senders_visible = bundle.lists.top_senders.len().min(LEADERBOARD_FIRST_PAGE);
        let top_admins_visible = bundle
            .lists
            .top_administrators
            .len()
            .min(LEADERBOARD_FIRST_PAGE);
        let top_inviters_visible = bundle.lists.top_inviters.len().min(LEADERBOARD_FIRST_PAGE);

        Self {
            bundle,
            charts,
            top_senders_visible,
            top_admins_visible,
            top_inviters_visible,
        }
    }

    /// Re-applies theme colors to all supergroup charts.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        for chart in &mut self.charts {
            chart.apply_palette(palette);
        }
    }

    /// Renders the supergroup statistics screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) {
        let overview = supergroup_overview_block(&self.bundle.stats, palette);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 14))
            .show(ui, |ui| {
                render_overview_block(ui, &overview, palette);
            });

        for chart in &mut self.charts {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 6))
                .show(ui, |ui| {
                    chart.show(ui, backend, palette, now_ms);
                });
        }

        // Top Members leaderboard.
        if !self.bundle.lists.top_senders.is_empty() {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    render_section_header(ui, &tr::lng_stats_members_title(), "", palette);
                    ui.add_space(4.0);
                    let visible = self
                        .top_senders_visible
                        .min(self.bundle.lists.top_senders.len());
                    for row in &self.bundle.lists.top_senders[..visible] {
                        let user = self.lookup_user(row.user_id);
                        let sub = format_top_sender_status(
                            row.sent_message_count,
                            row.average_character_count,
                        );
                        render_peer_stat_row(ui, &user, &sub, palette);
                    }
                    if self.top_senders_visible < self.bundle.lists.top_senders.len()
                        && render_show_more_button(ui, &tr::lng_stories_show_more(), palette)
                    {
                        self.top_senders_visible = self.bundle.lists.top_senders.len();
                    }
                });
        }

        // Top Administrators leaderboard.
        if !self.bundle.lists.top_administrators.is_empty() {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    render_section_header(ui, &tr::lng_stats_admins_title(), "", palette);
                    ui.add_space(4.0);
                    let visible = self
                        .top_admins_visible
                        .min(self.bundle.lists.top_administrators.len());
                    for row in &self.bundle.lists.top_administrators[..visible] {
                        let user = self.lookup_user(row.user_id);
                        let sub = format_top_admin_status(
                            row.deleted_message_count,
                            row.banned_user_count,
                            row.restricted_user_count,
                        );
                        render_peer_stat_row(ui, &user, &sub, palette);
                    }
                });
        }

        // Top Inviters leaderboard.
        if !self.bundle.lists.top_inviters.is_empty() {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    render_section_header(ui, &tr::lng_stats_inviters_title(), "", palette);
                    ui.add_space(4.0);
                    let visible = self
                        .top_inviters_visible
                        .min(self.bundle.lists.top_inviters.len());
                    for row in &self.bundle.lists.top_inviters[..visible] {
                        let user = self.lookup_user(row.user_id);
                        let sub = format_top_inviter_status(row.added_member_count);
                        render_peer_stat_row(ui, &user, &sub, palette);
                    }
                });
        }
    }

    fn lookup_user(&self, user_id: u64) -> PeerDisplayInfo {
        self.bundle
            .users
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| PeerDisplayInfo {
                id: user_id,
                title: format!("{}{user_id}", strings::MEMBER_FALLBACK_PREFIX),
                subtitle: String::new(),
                initials: "M".to_owned(),
                avatar_rgb: [0x58, 0xA8, 0xED],
            })
    }
}

// ============================================================================
// Message & Story Statistics Screen
// ============================================================================

/// Stateful view for individual Message or Story Statistics.
#[derive(Debug, Clone)]
pub struct MessageStatsView {
    /// Loaded message/story statistics bundle.
    pub bundle: MessageStatsBundle,
    /// Interactive charts (`Interactions` + `Reactions by emotion`).
    pub charts: Vec<ChartWidgetState>,
}

impl MessageStatsView {
    /// Creates a new message or story statistics view.
    #[must_use]
    pub fn new(bundle: MessageStatsBundle, palette: &ChartPalette) -> Self {
        let mut charts = Vec::with_capacity(2);
        push_chart_if_present(
            &mut charts,
            &bundle.stats.message_interaction_graph,
            tr::lng_chart_title_message_interaction(),
            ChartViewType::DoubleLinear,
            palette,
        );
        push_chart_if_present(
            &mut charts,
            &bundle.stats.reactions_by_emotion_graph,
            tr::lng_chart_title_reactions_by_emotion(),
            ChartViewType::Bar,
            palette,
        );
        Self { bundle, charts }
    }

    /// Re-applies theme colors to all charts.
    pub fn apply_palette(&mut self, palette: &ChartPalette) {
        for chart in &mut self.charts {
            chart.apply_palette(palette);
        }
    }

    /// Loads the next paginated slice of public forwards via `backend`.
    pub fn load_more_public_forwards(&mut self, backend: &dyn StatsBackend) {
        if self.bundle.public_forwards_next_offset.is_empty() {
            return;
        }
        let is_story = self.bundle.post.is_story;
        let id = if is_story {
            self.bundle.post.interaction.story_id
        } else {
            self.bundle.post.interaction.message_id
        };
        let offset = self.bundle.public_forwards_next_offset.clone();
        if let Ok((more, next_offset)) = backend.load_more_public_forwards(id, is_story, &offset) {
            self.bundle.public_forwards.extend(more);
            self.bundle.public_forwards_next_offset = next_offset;
        }
    }

    /// Renders the message or story statistics screen.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StatsBackend,
        palette: &ChartPalette,
        now_ms: i64,
    ) {
        // Post / Story header preview banner.
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 12))
            .show(ui, |ui| {
                let _ = render_recent_post_row(ui, &self.bundle.post, palette);
            });
        render_section_divider(ui, palette);

        let overview = message_overview_block(&self.bundle.stats);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 12))
            .show(ui, |ui| {
                render_overview_block(ui, &overview, palette);
            });

        for chart in &mut self.charts {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 6))
                .show(ui, |ui| {
                    chart.show(ui, backend, palette, now_ms);
                });
        }

        if !self.bundle.public_forwards.is_empty() {
            render_section_divider(ui, palette);
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    render_section_header(
                        ui,
                        &tr::lng_stats_overview_message_public_shares(),
                        "",
                        palette,
                    );
                    ui.add_space(4.0);
                    for fwd in &self.bundle.public_forwards {
                        let sub = format_public_forward_status(fwd.subscribers, fwd.views);
                        render_peer_stat_row(ui, &fwd.peer, &sub, palette);
                    }
                    if !self.bundle.public_forwards_next_offset.is_empty() {
                        ui.add_space(6.0);
                        if render_show_more_button(ui, &tr::lng_stories_show_more(), palette) {
                            self.load_more_public_forwards(backend);
                        }
                    }
                });
        }
    }
}

fn render_peer_stat_row(ui: &mut Ui, peer: &PeerDisplayInfo, status: &str, palette: &ChartPalette) {
    let avail_w = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 50.0), Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.box_text_fg, 0.03),
        );
    }
    let painter = ui.painter();
    paint_avatar(
        painter,
        Pos2::new(rect.left() + 22.0, rect.center().y),
        19.0,
        peer.avatar_rgb,
        &peer.initials,
        false,
    );
    painter.text(
        Pos2::new(rect.left() + 52.0, rect.top() + 8.0),
        Align2::LEFT_TOP,
        &peer.title,
        FontId::proportional(13.0),
        to_color32(palette.box_text_fg),
    );
    painter.text(
        Pos2::new(rect.left() + 52.0, rect.top() + 27.0),
        Align2::LEFT_TOP,
        status,
        FontId::proportional(11.5),
        to_color32(palette.window_sub_text_fg),
    );
}

/// Renders a Telegram-style `"Show more"` link row (`st::statisticsShowMoreButton`).
pub fn render_show_more_button(ui: &mut Ui, label: &str, palette: &ChartPalette) -> bool {
    let avail_w = ui.available_width().max(160.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, 34.0), Sense::click());
    let accent = to_color32(palette.window_active_text_fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            to_color32_alpha(palette.window_active_text_fg, 0.08),
        );
    }
    let arrow_size = (st::statistics_show_more_button_arrow_size.0 as f32).clamp(6.0, 12.0);
    let ax = rect.left() + 14.0;
    let ay = rect.center().y;
    let half = arrow_size * 0.45;
    let chevron = Stroke::new(1.6, accent);
    ui.painter().line_segment(
        [
            Pos2::new(ax - half, ay - half * 0.5),
            Pos2::new(ax, ay + half * 0.5),
        ],
        chevron,
    );
    ui.painter().line_segment(
        [
            Pos2::new(ax + half, ay - half * 0.5),
            Pos2::new(ax, ay + half * 0.5),
        ],
        chevron,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        accent,
    );
    resp.clicked()
}

/// Elides text with `"…"` so it fits within `max_width` in `font`.
#[must_use]
pub fn elide_text(ui: &Ui, text: &str, font: FontId, max_width: f32) -> String {
    crate::elide::elide_end(ui.painter(), text, &font, max_width)
}
