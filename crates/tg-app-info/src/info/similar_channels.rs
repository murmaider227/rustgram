//! Similar Channels section (`Info::SimilarPeers`).
//!
//! Port origin: `Telegram/SourceFiles/info/similar_peers/info_similar_peers_widget.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Ui, Vec2,
    WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;

use crate::info::backend::{SimilarChannelEntry, SimilarChannelsResult};
use crate::info::values::format_subscribers_status;
use tg_app_core::stand_in::types::PeerId;

/// Events emitted by [`SimilarChannelsSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimilarChannelsAction {
    /// User clicked a recommended channel row to open its profile or chat.
    OpenChannel(PeerId),
    /// User clicked the "Show More (Premium)" unlock banner.
    RequestPremiumUnlock,
}

/// Renders the Similar Channels Info tab (`channels.getChannelRecommendations`).
pub struct SimilarChannelsSection;

impl SimilarChannelsSection {
    /// Renders recommended similar channels and an optional Premium unlock banner.
    pub fn show(
        ui: &mut Ui,
        result: &SimilarChannelsResult,
        colors: &dyn Colors,
    ) -> Option<SimilarChannelsAction> {
        let mut action = None;

        if result.channels.is_empty() {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No similar channels found yet.")
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            ui.add_space(28.0);
            return None;
        }

        ui.add_space(6.0);
        for ch in &result.channels {
            if similar_channel_row(ui, ch, colors) {
                action = Some(SimilarChannelsAction::OpenChannel(ch.peer_id));
            }
        }

        if result.has_more_for_premium {
            ui.add_space(12.0);
            let remaining = result
                .total_count
                .saturating_sub(result.channels.len() as u32);
            let width = (ui.available_width() - 28.0).max(160.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
                let label = if remaining > 0 {
                    format!("SHOW {remaining} MORE SIMILAR CHANNELS")
                } else {
                    tr::lng_similar_channels_show_more().to_uppercase()
                };
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                let bg = if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    colors.color("activeButtonBgOver")
                } else {
                    colors.color("activeButtonBg")
                };
                ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    &label,
                    FontId::new(12.5, family_semibold()),
                    Color32::WHITE,
                );
                if resp.clicked() {
                    action = Some(SimilarChannelsAction::RequestPremiumUnlock);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(
                        "Subscribe to Telegram Premium to see up to 100 similar channels.",
                    )
                    .font(FontId::new(12.0, family_regular()))
                    .color(colors.color("windowSubTextFg")),
                );
            });
        }

        ui.add_space(12.0);
        action
    }
}

fn similar_channel_row(ui: &mut Ui, ch: &SimilarChannelEntry, colors: &dyn Colors) -> bool {
    let width = ui.available_width().max(180.0);
    let row_h = 56.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &ch.title));

    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let av_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 16.0 + 20.0, rect.center().y),
        Vec2::splat(40.0),
    );
    EmptyUserpic::for_color_index(ch.color_index, &ch.title, colors).paint_circle_or_photo(
        ui.painter(),
        av_rect,
        ch.peer_id.0,
    );

    let text_x = av_rect.max.x + 12.0;
    let title_fg = colors.color("windowBoldFg");
    let title_galley = ui.painter().layout_no_wrap(
        ch.title.clone(),
        font_id_from_style(st::semibold_font),
        title_fg,
    );
    let title_w = title_galley.size().x;
    let title_h = title_galley.size().y;
    ui.painter()
        .galley(Pos2::new(text_x, rect.min.y + 10.0), title_galley, title_fg);
    if ch.verified {
        tg_app_core::ui_icons::paint_select_check(
            ui.painter(),
            Pos2::new(text_x + title_w + 10.0, rect.min.y + 10.0 + title_h * 0.5),
            13.0,
            colors.color("windowActiveTextFg"),
        );
    }

    let subtitle = if let Some(ref uname) = ch.username {
        format!(
            "@{uname} · {}",
            format_subscribers_status(ch.subscribers_count)
        )
    } else {
        format_subscribers_status(ch.subscribers_count)
    };
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 30.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::new(12.5, family_regular()),
        colors.color("windowSubTextFg"),
    );

    resp.clicked()
}
