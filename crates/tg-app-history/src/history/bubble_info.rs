//! Bottom info (views, edited mark, time, ticks) and reaction chips of a message bubble
//! (`history/view/history_view_bottom_info.cpp`, `reactions/history_view_reactions.cpp`).
//!
//! The widths given to the `tg-history` layout are measured with the same fonts and strings
//! this module paints, so the time fits after the last text line or reaction chip exactly when
//! the layout says it does.

use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use tg_history::consts::{
    HISTORY_PIN_WIDTH, HISTORY_SEND_STATE_SPACE, HISTORY_SILENT_WIDTH, HISTORY_VIEWS_SPACE,
    HISTORY_VIEWS_WIDTH,
};
use tg_history::reactions::{format_count_to_short, reaction_count_button_width};
use tg_history::{BottomInfoInput, InfoDisplayType};

use crate::history::backend::{HistoryMessage, ReactionCounter};
use crate::history::theme::HistoryColors;

/// `st::msgDateFont` as painted by Rustgram.
#[must_use]
pub fn date_font() -> FontId {
    FontId::proportional(11.0)
}

/// `st::semiboldFont` for reaction counts.
fn reaction_count_font() -> FontId {
    FontId::new(12.0, tg_ui::fonts::family_semibold())
}

/// `BottomInfo::_authorEditedDate`: `edited 13:47`. The inline bot ("via @bot") is on the name
/// line (`crate::history::name_line`), as in tdesktop.
#[must_use]
pub fn author_edited_date_text(msg: &HistoryMessage) -> String {
    let mut out = String::new();
    if msg.edited {
        out.push_str("edited ");
    }
    out.push_str(&crate::history::inner::message_time_text(msg));
    out
}

/// `BottomInfo::_views`: `Lang::FormatCountToShort(max(views, 1))`.
#[must_use]
pub fn views_text(msg: &HistoryMessage) -> Option<String> {
    msg.views
        .map(|v| format_count_to_short(i64::from(v.max(1))))
}

/// `Flag::Silent` is shown only for scheduled silent messages.
fn shows_silent(msg: &HistoryMessage) -> bool {
    msg.extra.silent && msg.extra.scheduled_date.is_some()
}

fn text_width(painter: &Painter, text: String, font: FontId) -> i32 {
    painter
        .layout_no_wrap(text, font, Color32::WHITE)
        .size()
        .x
        .ceil() as i32
}

/// `BottomInfoDataFromMessage` with measured text widths.
#[must_use]
pub fn bottom_info_input(painter: &Painter, msg: &HistoryMessage) -> BottomInfoInput {
    BottomInfoInput {
        author_edited_date_width: text_width(painter, author_edited_date_text(msg), date_font()),
        author_width: 0,
        after_author_width: 0,
        views_text_width: views_text(msg).map_or(0, |t| text_width(painter, t, date_font())),
        replies_text_width: 0,
        out_layout: msg.outgoing && !msg.is_post,
        sending: msg.is_sending,
        pinned: msg.is_pinned,
        silent: shows_silent(msg),
        ephemeral: false,
        has_effect: msg.extra.effect_id.is_some(),
        sponsored: false,
        shortcut: false,
    }
}

/// Paints the bottom info right to left like `BottomInfo::paint`: ticks, date, silent, pin,
/// views count and the eye, then the effect.
pub fn paint_bottom_info(
    painter: &Painter,
    msg: &HistoryMessage,
    rect: Rect,
    display: InfoDisplayType,
    outgoing: bool,
    colors: &HistoryColors,
) {
    let pill = matches!(
        display,
        InfoDisplayType::Image | InfoDisplayType::ImageLeftToRight | InfoDisplayType::Background
    );
    // `Message::drawInfo`: `InfoDisplayType::Background` (bubble-less media) uses the service
    // plate colours, `Image` the darker over-image plate.
    let background = matches!(display, InfoDisplayType::Background);
    let fg = if background {
        colors.service_fg
    } else if pill {
        colors.date_img_fg
    } else if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };
    if pill {
        let r = (rect.height() * 0.5).ceil() as u8;
        let bg = if background {
            colors.service_bg
        } else {
            colors.date_img_bg
        };
        painter.rect_filled(rect, CornerRadius::same(r), bg);
    }

    let pad_x = if pill { 7.0 } else { 0.0 };
    let cy = rect.center().y;
    let mut right = rect.right() - pad_x;

    if outgoing {
        paint_ticks(painter, msg, right - 12.0, cy, fg);
        right -= HISTORY_SEND_STATE_SPACE as f32;
    }

    let date = painter.layout_no_wrap(author_edited_date_text(msg), date_font(), fg);
    right -= date.size().x;
    painter.galley(Pos2::new(right, cy - date.size().y * 0.5), date, fg);

    if shows_silent(msg) {
        right -= HISTORY_SILENT_WIDTH as f32;
        tg_app_core::ui_icons::paint_style_tinted(
            painter,
            Rect::from_center_size(Pos2::new(right + 5.0, cy), Vec2::splat(11.0)),
            tg_ui::tg_style::st::menu_icon_mute,
            fg,
        );
    }
    if msg.is_pinned {
        right -= HISTORY_PIN_WIDTH as f32;
        tg_app_core::ui_icons::paint_style_tinted(
            painter,
            Rect::from_center_size(Pos2::new(right + 12.0, cy), Vec2::splat(11.0)),
            tg_ui::tg_style::st::menu_icon_pin,
            fg,
        );
    }
    if let Some(views) = views_text(msg) {
        let galley = painter.layout_no_wrap(views, date_font(), fg);
        right -= HISTORY_VIEWS_SPACE as f32 + galley.size().x;
        painter.galley(Pos2::new(right, cy - galley.size().y * 0.5), galley, fg);
        right -= HISTORY_VIEWS_WIDTH as f32;
        tg_app_core::ui_icons::paint_eye(painter, Pos2::new(right + 9.0, cy), 11.0, fg);
    }
    if msg.extra.effect_id.is_some() {
        right -= 13.0;
        tg_app_core::ui_icons::paint_sparkle(painter, Pos2::new(right + 5.0, cy), 10.0, fg);
    }
}

/// Outgoing send / read ticks painted as crisp vector strokes.
fn paint_ticks(painter: &Painter, msg: &HistoryMessage, tx: f32, ty: f32, fg: Color32) {
    let stroke = Stroke::new(1.4, fg);
    if msg.is_sending {
        painter.circle_stroke(Pos2::new(tx + 5.0, ty), 4.2, stroke);
        painter.line_segment(
            [Pos2::new(tx + 5.0, ty - 2.2), Pos2::new(tx + 5.0, ty)],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(tx + 5.0, ty), Pos2::new(tx + 7.2, ty + 1.0)],
            stroke,
        );
        return;
    }
    painter.add(Shape::line(
        vec![
            Pos2::new(tx, ty + 0.5),
            Pos2::new(tx + 2.8, ty + 3.2),
            Pos2::new(tx + 7.8, ty - 3.0),
        ],
        stroke,
    ));
    if msg.is_read {
        painter.add(Shape::line(
            vec![
                Pos2::new(tx + 4.0, ty + 0.8),
                Pos2::new(tx + 6.4, ty + 3.2),
                Pos2::new(tx + 11.4, ty - 3.0),
            ],
            stroke,
        ));
    }
}

/// Reaction count text (`InlineList::setButtonCount`: `Lang::FormatCountToShort`).
#[must_use]
pub fn reaction_count_text(r: &ReactionCounter) -> String {
    if r.count == 0 {
        String::new()
    } else {
        format_count_to_short(i64::from(r.count))
    }
}

/// Chip widths for `MessageInput::reaction_button_widths` (`InlineList::countOptimalSize`).
/// Private chats show reactor userpics; their width comes from `userpics_width`.
#[must_use]
pub fn reaction_chip_widths(
    painter: &Painter,
    reactions: &[ReactionCounter],
    userpics_width: Option<&dyn Fn(&ReactionCounter) -> i32>,
) -> Vec<i32> {
    reactions
        .iter()
        .map(|r| match userpics_width {
            Some(f) => f(r),
            None => {
                let text = reaction_count_text(r);
                let w = if text.is_empty() {
                    0
                } else {
                    text_width(painter, text, reaction_count_font())
                };
                reaction_count_button_width(w)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::types::{MsgId, PeerId};

    fn msg() -> HistoryMessage {
        HistoryMessage {
            id: MsgId(1),
            peer_id: PeerId::from_channel(1),
            from_id: PeerId::from_channel(1),
            sender_name: String::new(),
            date_unix: 0,
            outgoing: false,
            is_post: true,
            text: tg_text::TextWithEntities::simple("x"),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: Some(14_512),
            edited: true,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn views_use_format_count_to_short_and_edited_prefixes_the_time() {
        let m = msg();
        assert_eq!(views_text(&m).as_deref(), Some("14.5K"));
        assert!(author_edited_date_text(&m).starts_with("edited "));
        // TGB-2965: the inline bot is not part of the bottom info.
        let mut via = msg();
        via.extra.via_bot = Some("gif".to_owned());
        assert_eq!(author_edited_date_text(&via), author_edited_date_text(&m));
        let mut zero = m;
        zero.views = Some(0);
        assert_eq!(views_text(&zero).as_deref(), Some("1"));
    }
}
