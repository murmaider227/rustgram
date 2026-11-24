//! 1:1 protected-chat sharing request card (`messageActionNoForwardsRequest`) and toggle
//! formatting (`messageActionNoForwardsToggle`).
//!
//! Port origin: `Telegram/SourceFiles/history/view/media/history_view_no_forwards_request.{h,cpp}`,
//! `Telegram/SourceFiles/api/api_suggest_post.cpp` (`RespondToNoForwardsRequest`), and
//! `Telegram/SourceFiles/history/history_item.cpp` (`HistoryServiceNoForwardsRequest`).

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};

use crate::history::backend::HistoryMessage;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

/// Fixed card width matching `st::chatSuggestInfoWidth` (`history_view_element.cpp:1786`).
pub const NO_FORWARDS_REQUEST_CARD_WIDTH: i32 = 276;

/// Three protected-chat capabilities listed as bullet points (`history_view_no_forwards_request.cpp:118-122`).
pub const NO_FORWARDS_REQUEST_FEATURES: [&str; 3] =
    ["Forwarding messages", "Saving media", "Copying text"];

/// Action emitted when the recipient clicks Allow or Decline on a 1:1 no-forwards request card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoForwardsRequestAction {
    /// Recipient approved enabling sharing (`messages.toggleNoForwards { enabled: false, request_msg_id }`).
    Accept(MsgId),
    /// Recipient declined enabling sharing (`messages.toggleNoForwards { enabled: true, request_msg_id }`).
    Decline(MsgId),
}

pub use tg_app_core::core_types::misc::{
    format_no_forwards_request_expired_text, format_no_forwards_request_title,
    format_no_forwards_toggle_text,
};

/// Returns `true` if a `NoForwardsRequest` service message should render the detailed card
/// (as opposed to the compact expired pill, matching `history_view_element.cpp:1781`).
#[must_use]
pub const fn should_render_no_forwards_request_card(expired: bool, action_taken: bool) -> bool {
    !expired || action_taken
}

/// Returns `true` if the Accept / Decline buttons should be shown below the card
/// (`!isExpired && !out() && !actionTaken`, matching `history_item.cpp:6099`).
#[must_use]
pub const fn has_no_forwards_request_buttons(
    expired: bool,
    outgoing: bool,
    action_taken: bool,
) -> bool {
    !expired && !outgoing && !action_taken
}

/// Computes the `(width, height)` of the centered `NoForwardsRequest` card (including action
/// buttons row when interactive).
#[must_use]
pub const fn measure_no_forwards_request_card(
    expired: bool,
    outgoing: bool,
    action_taken: bool,
) -> (i32, i32) {
    let base_h = 114;
    let buttons_h = if has_no_forwards_request_buttons(expired, outgoing, action_taken) {
        40
    } else {
        0
    };
    (NO_FORWARDS_REQUEST_CARD_WIDTH, base_h + buttons_h)
}

/// Paints the 1:1 no-forwards sharing request card (`GenerateNoForwardsRequestMedia`) and
/// optional Allow / Decline buttons.
pub fn paint_no_forwards_request_card(
    ui: &mut Ui,
    card_rect: Rect,
    msg: &HistoryMessage,
    expired: bool,
    action_taken: bool,
    colors: &HistoryColors,
) -> Option<NoForwardsRequestAction> {
    let show_buttons = has_no_forwards_request_buttons(expired, msg.outgoing, action_taken);
    let body_rect = if show_buttons {
        Rect::from_min_max(
            card_rect.min,
            Pos2::new(
                card_rect.max.x,
                (card_rect.max.y - 38.0).max(card_rect.min.y + 40.0),
            ),
        )
    } else {
        card_rect
    };

    ui.painter()
        .rect_filled(body_rect, CornerRadius::same(12), colors.service_bg);

    let title = format_no_forwards_request_title(&msg.sender_name, msg.outgoing);
    let inner_w = (body_rect.width() - 28.0).max(60.0);
    let title_galley = ui.painter().layout(
        title,
        FontId::proportional(12.5),
        colors.service_fg,
        inner_w,
    );
    let title_pos = Pos2::new(
        body_rect.center().x - title_galley.size().x * 0.5,
        body_rect.top() + 12.0,
    );
    ui.painter()
        .galley(title_pos, title_galley.clone(), colors.service_fg);

    // Three bullet points (`BulletPointPart`)
    let mut y = title_pos.y + title_galley.size().y + 10.0;
    let bullet_x = body_rect.left() + 22.0;
    for feature in NO_FORWARDS_REQUEST_FEATURES {
        ui.painter()
            .circle_filled(Pos2::new(bullet_x, y + 7.5), 2.5, colors.service_fg);
        ui.painter().text(
            Pos2::new(bullet_x + 10.0, y),
            Align2::LEFT_TOP,
            feature,
            FontId::proportional(12.5),
            colors.service_fg,
        );
        y += 20.0;
    }

    if !show_buttons {
        return None;
    }

    let gap = 6.0;
    let btn_y = body_rect.bottom() + 6.0;
    let btn_h = 30.0;
    let half_w = (card_rect.width() - gap) * 0.5;
    let decline_rect =
        Rect::from_min_size(Pos2::new(card_rect.left(), btn_y), Vec2::new(half_w, btn_h));
    let accept_rect = Rect::from_min_size(
        Pos2::new(decline_rect.right() + gap, btn_y),
        Vec2::new(half_w, btn_h),
    );

    let mut action = None;

    let decline_resp = ui.interact(
        decline_rect,
        Id::unique(("tg_nfr_decline", msg.id.0)),
        Sense::click(),
    );
    decline_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Decline sharing request"));
    if decline_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        decline_rect,
        CornerRadius::same(8),
        if decline_resp.hovered() {
            Color32::from_rgba_unmultiplied(
                colors.service_bg.r(),
                colors.service_bg.g(),
                colors.service_bg.b(),
                220,
            )
        } else {
            colors.service_bg
        },
    );
    ui.painter().text(
        decline_rect.center(),
        Align2::CENTER_CENTER,
        "Decline",
        FontId::proportional(12.5),
        colors.service_fg,
    );
    if decline_resp.clicked() {
        action = Some(NoForwardsRequestAction::Decline(msg.id));
    }

    let accept_resp = ui.interact(
        accept_rect,
        Id::unique(("tg_nfr_accept", msg.id.0)),
        Sense::click(),
    );
    accept_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Allow sharing request"));
    if accept_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        accept_rect,
        CornerRadius::same(8),
        if accept_resp.hovered() {
            colors.accent
        } else {
            Color32::from_rgba_unmultiplied(
                colors.accent.r(),
                colors.accent.g(),
                colors.accent.b(),
                215,
            )
        },
    );
    ui.painter().text(
        accept_rect.center(),
        Align2::CENTER_CENTER,
        "Allow",
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    if accept_resp.clicked() {
        action = Some(NoForwardsRequestAction::Accept(msg.id));
    }

    // Subtle divider under card buttons when hovered
    let _ = Stroke::NONE;

    action
}
