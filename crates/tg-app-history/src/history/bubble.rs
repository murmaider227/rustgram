//! Message bubble, service message, date separator, unread bar, headers, reactions,
//! and bottom-info painting.
//!
//! Port origin: `Telegram/SourceFiles/ui/chat/message_bubble.cpp`,
//! `Telegram/SourceFiles/history/view/history_view_message.cpp`,
//! `Telegram/SourceFiles/history/view/history_view_service_message.cpp`,
//! `Telegram/SourceFiles/history/view/history_view_reply.cpp`, and
//! `Telegram/SourceFiles/history/view/history_view_bottom_info.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense, Shape,
    Stroke, Ui, Vec2,
};
use tg_history::consts::{
    HISTORY_REPLY_PADDING, HISTORY_UNREAD_BAR_HEIGHT, MSG_PADDING, MSG_SERVICE_PADDING,
};
use tg_history::{
    BubbleCornerRounding, BubbleRounding, InfoDisplayType, MessageLayout, ServiceLayout,
    TextSelection,
};
use tg_ui::text::{
    RichTextOptions, SyntaxHighlightPalette, build_layout_job, render_rich_text,
    render_text_with_entities,
};
use tg_ui::userpic::{EmptyUserpic, peer_name_color};

use crate::bots::inline_keyboard::{
    InlineKeyboardActivation, InlineKeyboardParams, InlineKeyboardState, show_inline_keyboard,
};
use crate::history::backend::{
    HistoryMedia, HistoryMessage, MessageSummaryEntry, ReactionCounter, ServiceAction, to_bot_full,
};
use crate::history::inline_media::HistoryMediaRuntime;
use crate::history::media::{
    MediaAssetCache, MediaInteraction, VoicePlaybackState, bubble_rounding_to_egui,
    paint_history_media, paint_history_media_with_runtime,
};
use crate::history::sections_model::LinkPreviewData;
use crate::history::text_bridge::{
    char_range_rects, entities_to_rich_text, message_text_style, pointer_to_char_index,
};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{FullMsgId, MsgId, PeerId};

/// Interactive events returned from painting one message element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagePaintEvent {
    /// Clicked a reply header to jump to its target message.
    JumpToMessage(MsgId),
    /// Clicked a hyperlink / mention / hashtag inside rich text.
    OpenLink(String),
    /// Clicked a hidden hyperlink (`messageEntityTextUrl` / `EntityType::CustomUrl`) that
    /// requires scheme validation and confirmation before opening externally.
    OpenHiddenLink(String),
    /// Clicked a reaction pill to toggle it.
    ToggleReaction {
        /// Message id.
        msg_id: MsgId,
        /// Reaction emoji.
        emoji: String,
    },
    /// Media interaction (open viewer, save file, voice play/seek, vote poll).
    Media(MediaInteraction),
    /// User started dragging text selection at `(msg_id, char_index)`.
    TextDragStart {
        /// Message id.
        msg_id: MsgId,
        /// Starting char offset.
        char_index: u16,
    },
    /// User continued dragging text selection at `(msg_id, char_index)`.
    TextDragUpdate {
        /// Message id.
        msg_id: MsgId,
        /// Current char offset.
        char_index: u16,
    },
    /// User clicked the comments footer on a channel post to open its replies thread.
    OpenComments(MsgId),
    /// User clicked "Show Original" on a translated message.
    ClearTranslation(MsgId),
    /// User clicked the `+` reaction button to open the full emoji reactions picker.
    OpenReactionsPicker {
        /// Message id.
        msg_id: MsgId,
        /// Screen position to anchor the picker.
        pos: Pos2,
    },
    /// User clicked an inline keyboard button rendered beneath this bubble.
    InlineKeyboard(InlineKeyboardActivation),
    /// User clicked an Invoice / Payment Receipt card inside a message bubble.
    OpenInvoice {
        /// Message id.
        msg_id: MsgId,
        /// Target invoice identifier (`tg://invoice?slug=...`, `msg:<peer>:<msg>`, or `receipt:<peer>:<msg>`).
        invoice_id: String,
    },
    /// User clicked a Paid Media unlock card inside a message bubble.
    UnlockPaidMedia {
        /// Message id.
        msg_id: MsgId,
        /// Stars required to unlock.
        stars_amount: i64,
        /// Display title of the paid media card.
        title: String,
    },
    /// User clicked an interactive single-emoji message bubble (`history_view_emoji_interactions`).
    TapInteractiveEmoji {
        /// Message id.
        msg_id: MsgId,
        /// Emoticon string.
        emoticon: String,
        /// Screen center anchor for burst overlay.
        anchor: Pos2,
        /// True if outgoing layout.
        outgoing: bool,
    },
    /// User clicked the AI summary pill or expanded summary header to toggle summary vs original.
    ToggleSummary(MsgId),
    /// User clicked the info icon on the expanded AI summary header.
    AboutAiSummary(MsgId),
}

/// Interactive action emitted by a centered service action card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCardAction {
    /// Allow/Decline on a 1:1 `NoForwardsRequest` card.
    NoForwards(crate::history::no_forwards_request::NoForwardsRequestAction),
    /// CTA button on a `SuggestBirthday` or `SuggestProfilePhoto` card.
    Suggestion(crate::history::suggestion_cards::SuggestionCardAction),
}

/// Returns `true` when `lp` represents an Invoice or Payment Receipt card.
#[must_use]
pub fn is_invoice_link_preview(lp: &LinkPreviewData) -> bool {
    matches!(
        lp.site_name.as_str(),
        "Invoice" | "Test Invoice" | "Payment Receipt"
    ) || lp.url.starts_with("tg://invoice")
        || lp.url.starts_with("msg:")
        || lp.url.starts_with("receipt:")
}

/// Parses the Stars amount when `lp` represents a Paid Media unlock card.
#[must_use]
pub fn parse_paid_media_stars(lp: &LinkPreviewData) -> Option<i64> {
    if let Some(rest) = lp.url.strip_prefix("paid_media:") {
        let stars = rest
            .split(':')
            .nth(2)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(1);
        return Some(stars.max(1));
    }
    if lp.site_name == "Paid Media" {
        let digits: String = lp
            .title
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        return Some(digits.parse::<i64>().unwrap_or(1).max(1));
    }
    None
}

/// Returns the layout height in logical pixels reserved for a bubble's `link_preview` card.
#[must_use]
pub fn link_preview_card_height(lp: &LinkPreviewData) -> i32 {
    if is_invoice_link_preview(lp) || parse_paid_media_stars(lp).is_some() {
        84
    } else {
        58
    }
}

/// Optional bot inline-keyboard rendering context passed into [`paint_message_element_with_bots`].
pub struct InlineKeyboardRenderCtx<'a> {
    /// Shared per-chat inline keyboard loading/pressed state.
    pub state: &'a mut InlineKeyboardState,
    /// Owning bot user id, if known.
    pub bot_id: Option<tg_bots::stand_in::UserId>,
    /// Whether the bot is verified (affects URL confirmation behavior).
    pub bot_verified: bool,
    /// Active UI palette for semantic bot button colors.
    pub palette: &'a Palette,
}

/// Converts a `tg-history` integer [`tg_history::Rect`] offset by `(ox, oy)` into an `egui::Rect`.
#[inline]
#[must_use]
pub fn hist_rect_to_egui(r: tg_history::Rect, ox: f32, oy: f32) -> Rect {
    Rect::from_min_size(
        Pos2::new(ox + r.x as f32, oy + r.y as f32),
        Vec2::new(r.width.max(0) as f32, r.height.max(0) as f32),
    )
}

/// Paints a tdesktop message bubble shell with per-corner rounding, drop shadow, and speech tail
/// (`Ui::PaintBubble` in `ui/chat/message_bubble.cpp`).
pub fn paint_message_bubble_shell(
    painter: &Painter,
    rect: Rect,
    rounding: BubbleRounding,
    fill: Color32,
    shadow: Color32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }

    let cr = bubble_rounding_to_egui(rounding);

    // Subtle 1.5px bottom drop shadow (`st::msgShadow = 2px`).
    let shadow_rect = rect.translate(Vec2::new(0.0, 1.5));
    painter.rect_filled(shadow_rect, cr, shadow);

    // Main rounded bubble body.
    painter.rect_filled(rect, cr, fill);

    // Curved speech-bubble tail on bottom-left (incoming) or bottom-right (outgoing).
    if rounding.bottom_left == BubbleCornerRounding::Tail {
        let bx = rect.left();
        let by = rect.bottom();
        let tail_pts = vec![
            Pos2::new(bx + 4.0, by),
            Pos2::new(bx - 7.0, by),
            Pos2::new(bx - 2.5, by - 3.5),
            Pos2::new(bx, by - 10.0),
            Pos2::new(bx + 4.0, by - 10.0),
        ];
        painter.add(Shape::convex_polygon(
            tail_pts.iter().map(|p| *p + Vec2::new(0.0, 1.5)).collect(),
            shadow,
            Stroke::NONE,
        ));
        painter.add(Shape::convex_polygon(tail_pts, fill, Stroke::NONE));
    }
    if rounding.bottom_right == BubbleCornerRounding::Tail {
        let bx = rect.right();
        let by = rect.bottom();
        let tail_pts = vec![
            Pos2::new(bx - 4.0, by),
            Pos2::new(bx + 7.0, by),
            Pos2::new(bx + 2.5, by - 3.5),
            Pos2::new(bx, by - 10.0),
            Pos2::new(bx - 4.0, by - 10.0),
        ];
        painter.add(Shape::convex_polygon(
            tail_pts.iter().map(|p| *p + Vec2::new(0.0, 1.5)).collect(),
            shadow,
            Stroke::NONE,
        ));
        painter.add(Shape::convex_polygon(tail_pts, fill, Stroke::NONE));
    }
}

/// `st::msgServiceFont` (13px semibold), used for service pills and date badges.
#[must_use]
pub fn service_font() -> FontId {
    FontId::new(13.0, tg_ui::fonts::family_semibold())
}

/// Service message text as tdesktop renders it (`HistoryService::setServiceText`):
/// `lng_action_created_channel` for broadcast channels, `lng_action_created_chat` with «»
/// quotes for groups.
#[must_use]
pub fn service_text(msg: &HistoryMessage, svc: &crate::history::backend::ServiceAction) -> String {
    use crate::history::backend::ServiceAction;
    match svc {
        ServiceAction::NoForwardsToggle { new_value, .. } => {
            crate::history::no_forwards_request::format_no_forwards_toggle_text(
                &msg.sender_name,
                msg.outgoing,
                *new_value,
            )
        }
        ServiceAction::ChatCreated { title } => {
            let broadcast = matches!(
                msg.peer_id.kind(),
                tg_app_core::stand_in::types::PeerKind::Channel
            ) && (msg.is_post || msg.from_id == msg.peer_id);
            if broadcast {
                tg_lang::tr::lng_action_created_channel()
            } else {
                tg_lang::tr::lng_action_created_chat(&msg.sender_name, title)
            }
        }
        _ => svc.display_text(&msg.sender_name),
    }
}

/// Paints a centered date separator pill (`DateBadge`, `history_view_service_message.cpp`).
pub fn paint_date_separator(
    painter: &Painter,
    date_rect: Rect,
    label: &str,
    colors: &HistoryColors,
) {
    let radius = (date_rect.height() * 0.5).ceil() as u8;
    painter.rect_filled(date_rect, CornerRadius::same(radius), colors.service_bg);
    painter.text(
        date_rect.center(),
        Align2::CENTER_CENTER,
        label,
        service_font(),
        colors.service_fg,
    );
}

/// Paints the full-width "Unread messages" divider bar (`UnreadBar`, `history_view_element.cpp`).
pub fn paint_unread_bar(
    painter: &Painter,
    row_left: f32,
    row_width: f32,
    top_y: f32,
    colors: &HistoryColors,
) {
    let bar_rect = Rect::from_min_size(
        Pos2::new(row_left, top_y),
        Vec2::new(row_width, HISTORY_UNREAD_BAR_HEIGHT as f32),
    );
    painter.rect_filled(bar_rect, CornerRadius::ZERO, colors.unread_bar_bg);
    painter.line_segment(
        [
            Pos2::new(bar_rect.left(), bar_rect.top()),
            Pos2::new(bar_rect.right(), bar_rect.top()),
        ],
        Stroke::new(1.0, colors.divider),
    );
    painter.line_segment(
        [
            Pos2::new(bar_rect.left(), bar_rect.bottom()),
            Pos2::new(bar_rect.right(), bar_rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );
    painter.text(
        bar_rect.center(),
        Align2::CENTER_CENTER,
        "Unread messages",
        FontId::proportional(12.5),
        colors.unread_bar_fg,
    );
}

/// Paints a centered service message (`HistoryView::Service`), including interactive
/// `NoForwardsRequest`, `SuggestBirthday`, and `SuggestProfilePhoto` cards when applicable.
#[allow(clippy::too_many_arguments)]
pub fn paint_service_message_interactive(
    ui: &mut Ui,
    msg: &HistoryMessage,
    layout: &ServiceLayout,
    origin: Pos2,
    action_taken: bool,
    peer_name: &str,
    reduced_motion: bool,
    colors: &HistoryColors,
) -> Option<ServiceCardAction> {
    let svc = msg.service.as_ref()?;
    match svc {
        ServiceAction::NoForwardsRequest { expired, .. }
            if crate::history::no_forwards_request::should_render_no_forwards_request_card(
                *expired,
                action_taken,
            ) =>
        {
            let card_rect = if !layout.media_rect.is_empty() {
                hist_rect_to_egui(layout.media_rect, origin.x, origin.y)
            } else {
                let (w, h) = crate::history::no_forwards_request::measure_no_forwards_request_card(
                    *expired,
                    msg.outgoing,
                    action_taken,
                );
                let cx = origin.x + layout.current_size.width as f32 * 0.5;
                Rect::from_min_size(
                    Pos2::new(cx - w as f32 * 0.5, origin.y + layout.margin_top as f32),
                    Vec2::new(w as f32, h as f32),
                )
            };
            return crate::history::no_forwards_request::paint_no_forwards_request_card(
                ui,
                card_rect,
                msg,
                *expired,
                action_taken,
                colors,
            )
            .map(ServiceCardAction::NoForwards);
        }
        ServiceAction::SuggestBirthday { day, month, year } => {
            let card_rect = if !layout.media_rect.is_empty() {
                hist_rect_to_egui(layout.media_rect, origin.x, origin.y)
            } else {
                let (w, h) = crate::history::suggestion_cards::measure_birthday_suggestion_card(
                    msg.outgoing,
                );
                let cx = origin.x + layout.current_size.width as f32 * 0.5;
                Rect::from_min_size(
                    Pos2::new(cx - w as f32 * 0.5, origin.y + layout.margin_top as f32),
                    Vec2::new(w as f32, h as f32),
                )
            };
            return crate::history::suggestion_cards::paint_birthday_suggestion_card_with_tgs(
                ui,
                card_rect,
                msg,
                peer_name,
                *day,
                *month,
                *year,
                None,
                reduced_motion,
                colors,
            )
            .map(ServiceCardAction::Suggestion);
        }
        ServiceAction::SuggestProfilePhoto {
            photo_id,
            access_hash,
            file_reference,
            is_video,
            fallback_rgb,
            ..
        } => {
            let card_rect = if !layout.media_rect.is_empty() {
                hist_rect_to_egui(layout.media_rect, origin.x, origin.y)
            } else {
                let (w, h) = crate::history::suggestion_cards::measure_userpic_suggestion_card();
                let cx = origin.x + layout.current_size.width as f32 * 0.5;
                Rect::from_min_size(
                    Pos2::new(cx - w as f32 * 0.5, origin.y + layout.margin_top as f32),
                    Vec2::new(w as f32, h as f32),
                )
            };
            return crate::history::suggestion_cards::paint_userpic_suggestion_card(
                ui,
                card_rect,
                msg,
                peer_name,
                *photo_id,
                *access_hash,
                file_reference,
                *is_video,
                *fallback_rgb,
                colors,
            )
            .map(ServiceCardAction::Suggestion);
        }
        _ => {}
    }
    paint_service_message(ui.painter(), msg, layout, origin, colors);
    None
}

/// Paints a centered service message (`HistoryView::Service`).
pub fn paint_service_message(
    painter: &Painter,
    msg: &HistoryMessage,
    layout: &ServiceLayout,
    origin: Pos2,
    colors: &HistoryColors,
) {
    let Some(ref svc) = msg.service else {
        return;
    };
    let text = service_text(msg, svc);
    let bubble_rect = hist_rect_to_egui(layout.text_bubble_rect, origin.x, origin.y);
    if bubble_rect.is_positive() {
        let r = (bubble_rect.height() * 0.5).min(13.0).round() as u8;
        painter.rect_filled(bubble_rect, CornerRadius::same(r), colors.service_bg);
        let max_w = (bubble_rect.width() - MSG_SERVICE_PADDING.horizontal() as f32).max(20.0);
        let galley = painter.layout(text, service_font(), colors.service_fg, max_w);
        let text_pos = Pos2::new(
            bubble_rect.center().x - galley.size().x * 0.5,
            bubble_rect.center().y - galley.size().y * 0.5,
        );
        painter.galley(text_pos, galley, colors.service_fg);
    }
}

/// Paints a complete regular chat message element using its [`MessageLayout`].
#[allow(clippy::too_many_arguments)]
pub fn paint_message_element(
    ui: &mut Ui,
    msg: &HistoryMessage,
    layout: &MessageLayout,
    origin: Pos2,
    selection: TextSelection,
    highlight_opacity: f64,
    show_sender_name: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
) -> Vec<MessagePaintEvent> {
    paint_message_element_internal(
        ui,
        msg.peer_id,
        msg,
        layout,
        origin,
        selection,
        highlight_opacity,
        show_sender_name,
        colors,
        assets,
        voice_state,
        None,
        None,
        None,
        false,
        None,
    )
}

/// Paints a complete regular chat message element wired to [`HistoryMediaRuntime`].
#[allow(clippy::too_many_arguments)]
pub fn paint_message_element_with_runtime(
    ui: &mut Ui,
    peer_id: PeerId,
    msg: &HistoryMessage,
    layout: &MessageLayout,
    origin: Pos2,
    selection: TextSelection,
    highlight_opacity: f64,
    show_sender_name: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: &mut HistoryMediaRuntime,
) -> Vec<MessagePaintEvent> {
    paint_message_element_internal(
        ui,
        peer_id,
        msg,
        layout,
        origin,
        selection,
        highlight_opacity,
        show_sender_name,
        colors,
        assets,
        voice_state,
        Some(runtime),
        None,
        None,
        false,
        None,
    )
}

/// Paints a complete regular chat message element wired to [`HistoryMediaRuntime`] and inline bot keyboards.
#[allow(clippy::too_many_arguments)]
pub fn paint_message_element_with_bots(
    ui: &mut Ui,
    peer_id: PeerId,
    msg: &HistoryMessage,
    layout: &MessageLayout,
    origin: Pos2,
    selection: TextSelection,
    highlight_opacity: f64,
    show_sender_name: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: &mut HistoryMediaRuntime,
    inline_kb: Option<InlineKeyboardRenderCtx<'_>>,
) -> Vec<MessagePaintEvent> {
    paint_message_element_internal(
        ui,
        peer_id,
        msg,
        layout,
        origin,
        selection,
        highlight_opacity,
        show_sender_name,
        colors,
        assets,
        voice_state,
        Some(runtime),
        inline_kb,
        None,
        false,
        None,
    )
}

/// Paints a complete regular chat message element wired to [`HistoryMediaRuntime`], inline bot
/// keyboards, and optional AI message summary state (`messages.summarizeText`).
#[allow(clippy::too_many_arguments)]
pub fn paint_message_element_full(
    ui: &mut Ui,
    peer_id: PeerId,
    msg: &HistoryMessage,
    layout: &MessageLayout,
    origin: Pos2,
    selection: TextSelection,
    highlight_opacity: f64,
    show_sender_name: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: &mut HistoryMediaRuntime,
    inline_kb: Option<InlineKeyboardRenderCtx<'_>>,
    summary: Option<&MessageSummaryEntry>,
    paused_chat_effects: bool,
    reaction_userpics: Option<&ReactionUserpics>,
) -> Vec<MessagePaintEvent> {
    paint_message_element_internal(
        ui,
        peer_id,
        msg,
        layout,
        origin,
        selection,
        highlight_opacity,
        show_sender_name,
        colors,
        assets,
        voice_state,
        Some(runtime),
        inline_kb,
        summary,
        paused_chat_effects,
        reaction_userpics,
    )
}

#[allow(clippy::too_many_arguments)]
fn paint_message_element_internal(
    ui: &mut Ui,
    peer_id: PeerId,
    msg: &HistoryMessage,
    layout: &MessageLayout,
    origin: Pos2,
    selection: TextSelection,
    highlight_opacity: f64,
    show_sender_name: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: Option<&mut HistoryMediaRuntime>,
    inline_kb: Option<InlineKeyboardRenderCtx<'_>>,
    summary: Option<&MessageSummaryEntry>,
    paused_chat_effects: bool,
    reaction_userpics: Option<&ReactionUserpics>,
) -> Vec<MessagePaintEvent> {
    let mut events = Vec::new();
    let is_selected = selection.is_full();
    let ui_colors = colors.ui_colors();

    let bubble_rect = hist_rect_to_egui(layout.geometry, origin.x, origin.y);

    // 1. Bubble shell (unless unwrapped, e.g. Lottie sticker).
    if layout.has_bubble {
        let mut fill = match (layout.has_out_layout, is_selected) {
            (true, false) => colors.msg_out_bg,
            (true, true) => colors.msg_out_bg_selected,
            (false, false) => colors.msg_in_bg,
            (false, true) => colors.msg_in_bg_selected,
        };
        if highlight_opacity > 0.01 {
            let hl = colors.highlight_tint;
            fill = tg_ui::anim::color_lerp(
                fill,
                Color32::from_rgb(hl.r(), hl.g(), hl.b()),
                highlight_opacity * 0.35,
            );
        }
        let shadow = if layout.has_out_layout {
            colors.msg_out_shadow
        } else {
            colors.msg_in_shadow
        };
        paint_message_bubble_shell(
            ui.painter(),
            bubble_rect,
            layout.bubble_rounding,
            fill,
            shadow,
        );
    }

    // 2. Stacked bubble headers: sender name, forward header, reply header.
    let inner_left = bubble_rect.left() + MSG_PADDING.left as f32;
    let inner_w = (bubble_rect.width() - MSG_PADDING.horizontal() as f32).max(40.0);
    let mut header_y = bubble_rect.top() + MSG_PADDING.top as f32;

    if layout.has_bubble && show_sender_name && layout.header_heights.from_name_or_via > 0 {
        let color_idx = tg_ui::userpic::decide_color_index(msg.from_id.bare());
        let name_fg = peer_name_color(color_idx, &ui_colors);
        ui.painter().text(
            Pos2::new(inner_left, header_y),
            Align2::LEFT_TOP,
            &msg.sender_name,
            FontId::new(13.0, tg_ui::fonts::family_semibold()),
            name_fg,
        );
        header_y += layout.header_heights.from_name_or_via as f32;
    }

    if layout.has_bubble
        && let Some(ref fwd) = msg.forward
        && layout.header_heights.forwarded > 0
    {
        let fwd_color = if layout.has_out_layout {
            colors.msg_out_reply_bar
        } else {
            colors.msg_in_reply_bar
        };
        let label = format!("Forwarded from {}", fwd.from_name);
        ui.painter().text(
            Pos2::new(inner_left, header_y),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(12.5),
            fwd_color,
        );
        header_y += layout.header_heights.forwarded as f32;
    }

    if layout.has_bubble
        && let Some(ref reply) = msg.reply
        && layout.header_heights.reply > 0
    {
        let reply_h = (layout.header_heights.reply - 4).max(32) as f32;
        let reply_rect = Rect::from_min_size(
            Pos2::new(inner_left, header_y + 2.0),
            Vec2::new(inner_w, reply_h),
        );
        let bar_color = if layout.has_out_layout {
            colors.msg_out_reply_bar
        } else {
            colors.msg_in_reply_bar
        };
        let bg_color = if layout.has_out_layout {
            colors.msg_out_reply_bg
        } else {
            colors.msg_in_reply_bg
        };

        ui.painter()
            .rect_filled(reply_rect, CornerRadius::same(4), bg_color);
        let accent_bar = Rect::from_min_size(reply_rect.min, Vec2::new(3.0, reply_rect.height()));
        ui.painter()
            .rect_filled(accent_bar, CornerRadius::same(2), bar_color);

        let mut text_x = reply_rect.left() + HISTORY_REPLY_PADDING.left as f32;
        if let Some([r, g, b]) = reply.preview_thumb_rgb {
            let thumb = Rect::from_min_size(
                Pos2::new(reply_rect.left() + 7.0, reply_rect.top() + 3.0),
                Vec2::splat((reply_h - 6.0).min(30.0)),
            );
            ui.painter()
                .rect_filled(thumb, CornerRadius::same(3), Color32::from_rgb(r, g, b));
            text_x = thumb.right() + 6.0;
        }

        let name_col = if layout.has_out_layout {
            bar_color
        } else {
            peer_name_color(
                tg_ui::userpic::decide_color_index(reply.author_color_id),
                &ui_colors,
            )
        };
        // history_view_reply.cpp: name in `msgNameStyle` (semibold 13px), one elided line of
        // `defaultTextStyle` (13px) in the bubble's text colour below it.
        let text_max_w =
            (reply_rect.right() - text_x - HISTORY_REPLY_PADDING.right as f32).max(40.0);
        let name_top = reply_rect.top() + HISTORY_REPLY_PADDING.top as f32;
        let name = elided_line(
            ui,
            &reply.author_name,
            FontId::new(13.0, tg_ui::fonts::family_semibold()),
            name_col,
            text_max_w,
        );
        let name_h = name.size().y.max(18.0);
        ui.painter()
            .galley(Pos2::new(text_x, name_top), name, name_col);
        let snippet_text = if let Some(ref quote) = reply.quote_text {
            format!("“{quote}”")
        } else {
            reply.preview_text.clone()
        };
        let snippet = elided_line(
            ui,
            &snippet_text,
            FontId::proportional(13.0),
            colors.text_fg,
            text_max_w,
        );
        ui.painter().galley(
            Pos2::new(text_x, name_top + name_h),
            snippet,
            colors.text_fg,
        );

        let reply_resp = ui.interact(
            reply_rect,
            Id::unique(("tg_reply_hdr", msg.id.0)),
            Sense::click(),
        );
        if reply_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if reply_resp.clicked() {
            events.push(MessagePaintEvent::JumpToMessage(reply.reply_to_msg_id));
        }
    }

    // 3. Media attachment (photo, album, video, document, voice, Lottie sticker, poll).
    if let Some(ref media) = msg.media
        && !layout.media_rect.is_empty()
    {
        let media_rect = hist_rect_to_egui(layout.media_rect, origin.x, origin.y);
        let media_rounding = if layout.has_bubble {
            let mut r = layout.message_rounding;
            if layout.header_heights.total() > 0 {
                r.top_left = BubbleCornerRounding::None;
                r.top_right = BubbleCornerRounding::None;
            }
            if !msg.text.is_empty()
                && !layout
                    .media_layout
                    .as_ref()
                    .is_some_and(|m| m.is_bubble_bottom)
            {
                r.bottom_left = BubbleCornerRounding::None;
                r.bottom_right = BubbleCornerRounding::None;
            }
            r
        } else {
            layout.message_rounding
        };

        let view_once_kind = match media {
            HistoryMedia::Photo(_) => {
                Some(tg_app_info::boxes::self_destruct::ViewOnceMediaKind::Photo)
            }
            HistoryMedia::Video(v) if !v.is_gif => {
                Some(tg_app_info::boxes::self_destruct::ViewOnceMediaKind::Video)
            }
            HistoryMedia::Voice(_) => {
                Some(tg_app_info::boxes::self_destruct::ViewOnceMediaKind::Voice)
            }
            HistoryMedia::RoundVideo(_) => {
                Some(tg_app_info::boxes::self_destruct::ViewOnceMediaKind::RoundVideo)
            }
            _ => None,
        };

        let media_ev = if let (Some(ttl_seconds), Some(kind)) =
            (msg.extra.ttl_seconds.filter(|&t| t > 0), view_once_kind)
        {
            if tg_app_info::boxes::self_destruct::paint_view_once_media_plate(
                ui,
                msg.id,
                media_rect,
                kind,
                ttl_seconds,
                msg.extra.ttl_viewed,
            ) {
                Some(crate::history::media::MediaInteraction::OpenViewOnceMedia {
                    msg_id: msg.id,
                    kind,
                    ttl_seconds,
                })
            } else {
                None
            }
        } else if let Some(rt) = runtime {
            paint_history_media_with_runtime(
                ui,
                peer_id,
                msg.id,
                &msg.sender_name,
                media,
                layout.media_layout.as_ref(),
                media_rect,
                media_rounding,
                layout.has_out_layout,
                colors,
                assets,
                voice_state,
                rt,
            )
        } else {
            paint_history_media(
                ui,
                msg.id,
                media,
                layout.media_layout.as_ref(),
                media_rect,
                media_rounding,
                layout.has_out_layout,
                colors,
                assets,
                voice_state,
            )
        };
        if let Some(ev) = media_ev {
            events.push(MessagePaintEvent::Media(ev));
        }

        // Layer-233 open-answers poll overlay strip ("+ Add Option" + attachment badges).
        if let HistoryMedia::Poll(poll) = media
            && let Some(ref poll_extras) = msg.extra.poll_extras
        {
            let has_attachments = poll_extras.option_attachments.iter().any(Option::is_some);
            if has_attachments {
                let badge_summary = poll_extras
                    .option_attachments
                    .iter()
                    .flatten()
                    .map(|a| a.display_badge())
                    .collect::<Vec<_>>()
                    .join(" · ");
                ui.painter().text(
                    Pos2::new(media_rect.left() + 12.0, media_rect.top() + 4.0),
                    Align2::LEFT_TOP,
                    badge_summary,
                    FontId::proportional(10.5),
                    colors.active_text_fg,
                );
            }
            if poll_extras.open_answers && !poll.closed {
                let btn_rect = Rect::from_min_size(
                    Pos2::new(media_rect.right() - 104.0, media_rect.bottom() - 22.0),
                    Vec2::new(94.0, 20.0),
                );
                let resp = ui.interact(
                    btn_rect,
                    Id::unique(("tg_poll_add_opt_btn", msg.id.0)),
                    Sense::click(),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    btn_rect,
                    CornerRadius::same(5),
                    Color32::from_rgba_unmultiplied(
                        colors.accent.r(),
                        colors.accent.g(),
                        colors.accent.b(),
                        if resp.hovered() { 42 } else { 22 },
                    ),
                );
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "+ Add Option",
                    FontId::proportional(11.0),
                    colors.active_text_fg,
                );
                if resp.clicked() {
                    events.push(MessagePaintEvent::Media(
                        crate::history::media::MediaInteraction::OpenAddPollOption(msg.id),
                    ));
                }
            }
        }
    }

    // 4. Rich document message blocks + formulas OR standard formatted message body.
    if let Some(ref rich_payload) = msg.extra.rich_message
        && !layout.text_rect.is_empty()
    {
        let text_rect = hist_rect_to_egui(layout.text_rect, origin.x, origin.y);
        let view_style =
            tg_app_media::webview::rich_page_view::RichPageViewStyle::from_history_colors(
                colors,
                layout.has_out_layout,
            );
        let mut rich_events = Vec::new();
        ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
            rich_events = tg_app_media::webview::formula_widget::with_math_engine(|engine| {
                tg_app_media::webview::rich_page_view::paint_rich_page(
                    ui,
                    ("tg_rich_msg_bubble", msg.id.0),
                    engine,
                    &rich_payload.page,
                    text_rect.width().max(60.0),
                    &view_style,
                )
            });
        });
        for ev in rich_events {
            let tg_app_media::webview::rich_page_view::RichPageViewEvent::OpenLink(url) = ev;
            events.push(MessagePaintEvent::OpenLink(url));
        }
    } else if !msg.text.is_empty() && !layout.text_rect.is_empty() {
        let full_text_rect = hist_rect_to_egui(layout.text_rect, origin.x, origin.y);
        let summary_h = crate::history::summary_header::summary_reserved_height(
            msg.extra.summary_from_language.as_deref(),
            summary,
        ) as f32;
        let summary_shown = summary.is_some_and(|e| e.shown);
        let text_rect = if layout.has_bubble && summary_h > 0.0 {
            let hdr_h = summary_h.min((full_text_rect.height() - 14.0).max(24.0));
            let summary_rect = Rect::from_min_size(
                full_text_rect.min,
                Vec2::new(full_text_rect.width().max(inner_w), hdr_h),
            );
            let act = if summary_shown {
                crate::history::summary_header::paint_summary_header(
                    ui,
                    msg.id,
                    summary_rect,
                    layout.has_out_layout,
                    summary.is_some_and(|e| e.loading),
                    paused_chat_effects,
                    colors,
                )
            } else {
                crate::history::summary_header::paint_summary_pill(
                    ui,
                    msg.id,
                    summary_rect,
                    layout.has_out_layout,
                    summary,
                    colors,
                )
            };
            if let Some(a) = act {
                match a {
                    crate::history::summary_header::SummaryHeaderAction::Toggle(mid) => {
                        events.push(MessagePaintEvent::ToggleSummary(mid));
                    }
                    crate::history::summary_header::SummaryHeaderAction::AboutAi(mid) => {
                        events.push(MessagePaintEvent::AboutAiSummary(mid));
                    }
                }
            }
            Rect::from_min_max(
                Pos2::new(full_text_rect.left(), full_text_rect.top() + hdr_h),
                full_text_rect.max,
            )
        } else {
            full_text_rect
        };

        let summary_loading_fallback = tg_text::TextWithEntities::simple("Summarizing...");
        let active_text: &tg_text::TextWithEntities = if summary_shown {
            if let Some(s) = summary {
                if !s.text.is_empty() {
                    &s.text
                } else if s.loading {
                    &summary_loading_fallback
                } else {
                    &msg.text
                }
            } else {
                &msg.text
            }
        } else {
            &msg.text
        };

        let style = message_text_style();
        let text_pal = colors.text_palette(layout.has_out_layout);
        let uses_rich_entity_renderer = active_text.entities.iter().any(|e| {
            matches!(
                e.entity_type(),
                tg_text::EntityType::Pre
                    | tg_text::EntityType::Blockquote
                    | tg_text::EntityType::CustomEmoji
                    | tg_text::EntityType::Colorized
            )
        });

        if uses_rich_entity_renderer {
            let mut opts = RichTextOptions::new(colors.text_fg, text_rect.width().max(40.0))
                .selectable(true)
                .selection(tg_text::TextSelection::new(selection.from, selection.to));
            opts.syntax_palette = Some(if colors.is_night {
                SyntaxHighlightPalette::night()
            } else {
                SyntaxHighlightPalette::day()
            });
            let mut clicked_link: Option<(String, bool)> = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                let resp = render_text_with_entities(
                    ui,
                    Id::unique(("tg_msg_entity_text", msg.id.0)),
                    active_text,
                    &style,
                    &text_pal,
                    &opts,
                );
                if let Some(link) = resp.clicked_link {
                    let is_hidden = link.kind == tg_text::EntityType::CustomUrl;
                    clicked_link = Some((link.target, is_hidden));
                }
            });
            if let Some((url, is_hidden)) = clicked_link {
                if is_hidden {
                    events.push(MessagePaintEvent::OpenHiddenLink(url));
                } else {
                    events.push(MessagePaintEvent::OpenLink(url));
                }
            }
        } else {
            let rich = entities_to_rich_text(active_text);

            // Paint partial text-selection background rectangles before drawing the text galley.
            if !selection.is_empty() && !selection.is_full() {
                let job = build_layout_job(
                    &rich,
                    &style,
                    &text_pal,
                    colors.text_fg,
                    text_rect.width().max(40.0),
                    None,
                    true,
                );
                let sel_galley = ui.painter().layout_job(job);
                for sr in char_range_rects(
                    &sel_galley,
                    text_rect.min,
                    usize::from(selection.from),
                    usize::from(selection.to),
                ) {
                    ui.painter()
                        .rect_filled(sr, CornerRadius::same(2), colors.text_select_bg);
                }
            }

            // Render interactive rich text (links, bold, italic, mono, underline, strike, spoiler).
            let mut clicked_link = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                let resp = render_rich_text(
                    ui,
                    Id::unique(("tg_msg_text", msg.id.0)),
                    &rich,
                    &style,
                    &text_pal,
                    colors.text_fg,
                    text_rect.width().max(40.0),
                );
                clicked_link = resp.clicked_link;
            });
            if let Some(url) = clicked_link {
                let is_hidden = active_text
                    .entities
                    .iter()
                    .any(|e| e.entity_type() == tg_text::EntityType::CustomUrl && e.data() == url);
                if is_hidden {
                    events.push(MessagePaintEvent::OpenHiddenLink(url));
                } else {
                    events.push(MessagePaintEvent::OpenLink(url));
                }
            }

            // Drag-select text sensing over the text rect.
            let drag_resp = ui.interact(
                text_rect,
                Id::unique(("tg_msg_text_drag", msg.id.0)),
                Sense::drag(),
            );
            if drag_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
            if drag_resp.drag_started() || drag_resp.dragged() {
                let job = build_layout_job(
                    &rich,
                    &style,
                    &text_pal,
                    colors.text_fg,
                    text_rect.width().max(40.0),
                    None,
                    true,
                );
                let drag_galley = ui.painter().layout_job(job);
                if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                    let (char_index, _) = pointer_to_char_index(&drag_galley, text_rect.min, ptr);
                    if drag_resp.drag_started() {
                        events.push(MessagePaintEvent::TextDragStart {
                            msg_id: msg.id,
                            char_index,
                        });
                    } else {
                        events.push(MessagePaintEvent::TextDragUpdate {
                            msg_id: msg.id,
                            char_index,
                        });
                    }
                }
            }
        }
    }

    // 4b. Link preview / Invoice / Paid Media interactive card inside the bubble.
    if layout.has_bubble
        && let Some(ref lp) = msg.extra.link_preview
    {
        let card_h = link_preview_card_height(lp) as f32;
        let date_room = if msg.reactions.is_empty() { 20.0 } else { 4.0 };
        let card_top = if !layout.text_rect.is_empty() {
            let text_rect = hist_rect_to_egui(layout.text_rect, origin.x, origin.y);
            (text_rect.bottom() - date_room - card_h).max(text_rect.top())
        } else {
            (bubble_rect.bottom() - 24.0 - card_h).max(bubble_rect.top() + 8.0)
        };
        let card_rect = Rect::from_min_size(
            Pos2::new(inner_left, card_top),
            Vec2::new(inner_w.max(180.0), card_h),
        );
        if let Some(ev) = paint_link_preview_card(
            ui,
            peer_id,
            msg.id,
            lp,
            card_rect,
            layout.has_out_layout,
            colors,
        ) {
            events.push(ev);
        }
    }

    // 5. Reactions row (`Reactions::InlineList`: emoji + count, or reactor userpics in
    // private chats). The full picker opens from the context menu reaction strip.
    if !msg.reactions.is_empty() && !layout.reactions_rect.is_empty() {
        let react_rect = hist_rect_to_egui(layout.reactions_rect, origin.x, origin.y);
        if let Some(ReactionRowAction::Toggle(emoji)) = paint_reactions_row(
            ui,
            msg.id,
            &msg.reactions,
            react_rect,
            layout.has_out_layout,
            colors,
            reaction_userpics,
            assets,
        ) {
            events.push(MessagePaintEvent::ToggleReaction {
                msg_id: msg.id,
                emoji,
            });
        }
    }

    // 5b. Inline translation, via-bot/effect badges, and channel comments footer bar.
    if layout.has_bubble {
        if let Some((ref lang, ref translated)) = msg.extra.translated_text {
            let tr_rect = Rect::from_min_size(
                Pos2::new(inner_left, bubble_rect.bottom() - 40.0),
                Vec2::new(inner_w, 20.0),
            );
            let tr_galley = ui.painter().layout_no_wrap(
                format!("({}) {}", lang.to_uppercase(), translated),
                FontId::proportional(11.5),
                colors.active_text_fg,
            );
            let tr_painter = ui.painter().with_clip_rect(tr_rect);
            tg_app_core::ui_icons::paint_style_tinted(
                &tr_painter,
                Rect::from_center_size(
                    Pos2::new(
                        tr_rect.left() + 7.0,
                        tr_rect.top() + tr_galley.size().y * 0.5,
                    ),
                    Vec2::splat(13.0),
                ),
                tg_ui::tg_style::st::menu_icon_translate,
                colors.active_text_fg,
            );
            tr_painter.galley(
                Pos2::new(tr_rect.left() + 17.0, tr_rect.top()),
                tr_galley,
                colors.active_text_fg,
            );
            let orig_resp = ui.interact(
                tr_rect,
                Id::unique(("tg_msg_tr_clear", msg.id.0)),
                Sense::click(),
            );
            if orig_resp.on_hover_text("Click to show original").clicked() {
                events.push(MessagePaintEvent::ClearTranslation(msg.id));
            }
        }
        if let Some(count) = msg.extra.replies_count
            && msg.is_post
        {
            let comm_rect = if !layout.comments_rect.is_empty() {
                hist_rect_to_egui(layout.comments_rect, origin.x, origin.y)
            } else {
                Rect::from_min_size(
                    Pos2::new(bubble_rect.left() + 8.0, bubble_rect.bottom() - 26.0),
                    Vec2::new((bubble_rect.width() - 16.0).max(80.0), 22.0),
                )
            };
            let comm_resp = ui.interact(
                comm_rect,
                Id::unique(("tg_post_comments", msg.id.0)),
                Sense::click(),
            );
            let comm_text = if count == 0 {
                "Leave a comment".to_owned()
            } else if count == 1 {
                "1 comment".to_owned()
            } else {
                format!("{count} comments")
            };
            comm_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, comm_text.clone())
            });
            if comm_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    comm_rect,
                    CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 10,
                        se: 10,
                    },
                    colors.bar_bg_over,
                );
            }
            ui.painter().line_segment(
                [
                    Pos2::new(comm_rect.left(), comm_rect.top()),
                    Pos2::new(comm_rect.right(), comm_rect.top()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(comm_rect.left() + 18.0, comm_rect.center().y),
                    Vec2::splat(14.0),
                ),
                tg_ui::tg_style::st::menu_icon_chat_bubble,
                colors.active_text_fg,
            );
            ui.painter().text(
                Pos2::new(comm_rect.left() + 30.0, comm_rect.center().y),
                Align2::LEFT_CENTER,
                &comm_text,
                FontId::proportional(12.5),
                colors.active_text_fg,
            );
            ui.painter().text(
                Pos2::new(comm_rect.right() - 12.0, comm_rect.center().y),
                Align2::RIGHT_CENTER,
                "›",
                FontId::proportional(16.0),
                colors.active_text_fg,
            );
            if comm_resp.clicked() {
                events.push(MessagePaintEvent::OpenComments(msg.id));
            }
        }
    }

    // 6. Bottom info bar (views, edited, time, outgoing sent/read ticks).
    if !layout.bottom_info_rect.is_empty() {
        let info_rect = hist_rect_to_egui(layout.bottom_info_rect, origin.x, origin.y);
        paint_bottom_info(
            ui.painter(),
            msg,
            info_rect,
            layout.bottom_info_display,
            layout.has_out_layout,
            colors,
        );
    }

    // 7. Inline bot keyboard directly beneath the bubble (`layout.keyboard_rect`).
    if let Some(ref markup) = msg.extra.reply_markup
        && markup.is_inline()
        && !layout.keyboard_rect.is_empty()
        && let Some(kb_ctx) = inline_kb
    {
        let kb_rect = hist_rect_to_egui(layout.keyboard_rect, origin.x, origin.y);
        let params = InlineKeyboardParams::for_message(
            to_bot_full(FullMsgId {
                peer: peer_id,
                msg: msg.id,
            }),
            kb_ctx.bot_id,
            kb_ctx.bot_verified,
            kb_rect.width(),
            ui.ctx().input(|i| i.time),
        );
        let mut kb_act = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(kb_rect), |ui| {
            kb_act = show_inline_keyboard(ui, markup, params, kb_ctx.state, kb_ctx.palette);
        });
        if let Some(act) = kb_act {
            events.push(MessagePaintEvent::InlineKeyboard(act));
        }
    }

    events
}

fn paint_link_preview_card(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    lp: &LinkPreviewData,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MessagePaintEvent> {
    let is_invoice = is_invoice_link_preview(lp);
    let paid_stars = parse_paid_media_stars(lp);
    let accent = if paid_stars.is_some() {
        Color32::from_rgb(0xE8, 0xA8, 0x24)
    } else if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.msg_in_reply_bar
    };
    let card_bg = if outgoing {
        colors.reaction_out_bg
    } else {
        colors.reaction_in_bg
    };

    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), card_bg);
    let bar_rect = Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
    ui.painter()
        .rect_filled(bar_rect, CornerRadius::same(2), accent);

    let text_x = rect.left() + 10.0;
    let clip_w = (rect.width() - 18.0).max(40.0);
    let site_label = if lp.site_name.is_empty() {
        "Link".to_owned()
    } else {
        lp.site_name.clone()
    };
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 5.0),
        Align2::LEFT_TOP,
        &site_label,
        FontId::proportional(11.5),
        accent,
    );

    if !lp.title.is_empty() {
        let title_galley = ui.painter().layout_no_wrap(
            lp.title.clone(),
            FontId::proportional(12.5),
            colors.bold_fg,
        );
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_x, rect.top() + 20.0),
                Vec2::new(clip_w, 18.0),
            ))
            .galley(
                Pos2::new(text_x, rect.top() + 20.0),
                title_galley,
                colors.bold_fg,
            );
    }

    if !lp.description.is_empty() {
        let desc_galley = ui.painter().layout_no_wrap(
            lp.description.clone(),
            FontId::proportional(11.5),
            colors.sub_text_fg,
        );
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_x, rect.top() + 37.0),
                Vec2::new(clip_w, 16.0),
            ))
            .galley(
                Pos2::new(text_x, rect.top() + 37.0),
                desc_galley,
                colors.sub_text_fg,
            );
    }

    if is_invoice {
        let btn_label = if lp.site_name == "Payment Receipt" {
            "View Receipt"
        } else {
            "Pay"
        };
        let btn_rect = Rect::from_min_max(
            Pos2::new(text_x, rect.bottom() - 26.0),
            Pos2::new(rect.right() - 8.0, rect.bottom() - 5.0),
        );
        let btn_resp = ui.interact(
            btn_rect,
            Id::unique(("tg_invoice_btn", msg_id.0)),
            Sense::click(),
        );
        btn_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("{btn_label}: {}", lp.title),
            )
        });
        let card_resp = ui.interact(
            rect,
            Id::unique(("tg_invoice_card", msg_id.0)),
            Sense::click(),
        );
        if btn_resp.hovered() || card_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(btn_rect, CornerRadius::same(6), colors.accent);
        ui.painter().text(
            btn_rect.center(),
            Align2::CENTER_CENTER,
            btn_label,
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        if btn_resp.clicked() || card_resp.clicked() {
            let invoice_id = if !lp.url.trim().is_empty() {
                lp.url.clone()
            } else {
                format!("msg:{}:{}", peer_id.0, msg_id.0)
            };
            return Some(MessagePaintEvent::OpenInvoice { msg_id, invoice_id });
        }
        return None;
    }

    if let Some(stars_amount) = paid_stars {
        let btn_label = if lp.title.is_empty() {
            format!("Unlock for {stars_amount} Stars")
        } else {
            lp.title.replace(" ⭐", " Stars").replace("⭐", "Stars")
        };
        let btn_rect = Rect::from_min_max(
            Pos2::new(text_x, rect.bottom() - 26.0),
            Pos2::new(rect.right() - 8.0, rect.bottom() - 5.0),
        );
        let btn_resp = ui.interact(
            btn_rect,
            Id::unique(("tg_paid_media_btn", msg_id.0)),
            Sense::click(),
        );
        btn_resp
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, btn_label.clone()));
        let card_resp = ui.interact(
            rect,
            Id::unique(("tg_paid_media_card", msg_id.0)),
            Sense::click(),
        );
        if btn_resp.hovered() || card_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(btn_rect, CornerRadius::same(6), accent);
        let btn_galley = ui.painter().layout_no_wrap(
            btn_label.clone(),
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        let total_w = 12.0 + 4.0 + btn_galley.size().x;
        let left_x = btn_rect.center().x - total_w * 0.5;
        tg_app_core::ui_icons::paint_star(
            ui.painter(),
            Pos2::new(left_x + 6.0, btn_rect.center().y),
            5.0,
            Color32::WHITE,
        );
        ui.painter().galley(
            Pos2::new(
                left_x + 16.0,
                btn_rect.center().y - btn_galley.size().y * 0.5,
            ),
            btn_galley,
            Color32::WHITE,
        );
        if btn_resp.clicked() || card_resp.clicked() {
            return Some(MessagePaintEvent::UnlockPaidMedia {
                msg_id,
                stars_amount,
                title: btn_label,
            });
        }
        return None;
    }

    if !lp.url.is_empty() {
        let card_resp = ui.interact(
            rect,
            Id::unique(("tg_link_preview_card", msg_id.0)),
            Sense::click(),
        );
        if card_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if card_resp.clicked() {
            return Some(MessagePaintEvent::OpenLink(lp.url.clone()));
        }
    }
    None
}

enum ReactionRowAction {
    Toggle(String),
}

/// Private-chat reactors shown as userpics inside reaction buttons
/// (`Reactions::InlineList`, "always show userpics" for users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionUserpics {
    /// The other side of the private chat (`peer`, display name).
    pub peer: (PeerId, String),
    /// The current account (`peer`, display name).
    pub me: (PeerId, String),
}

/// `st::reactionInlinePadding`.
const REACTION_PAD_LEFT: f32 = 5.0;
const REACTION_PAD_RIGHT: f32 = 7.0;
const REACTION_PAD_V: f32 = 2.0;
/// `st::reactionInlineSize`.
const REACTION_SIZE: f32 = 18.0;
/// `st::reactionInlineSkip`.
const REACTION_SKIP: f32 = 3.0;
/// `st::reactionInlineBetween`.
const REACTION_BETWEEN: f32 = 4.0;
/// `st::reactionInlineUserpics.size` / `.shift`.
const REACTION_USERPIC: f32 = 20.0;
const REACTION_USERPIC_SHIFT: f32 = 7.0;
/// `st::reactionInlineUserpicsPadding` (left + right).
const REACTION_USERPICS_PAD: f32 = 2.0;

/// Number of userpics a private-chat reaction button shows (`InlineListDataFromMessage`).
/// One line of `text` elided with "…" to `max_w` (`Ui::Text::String` `elisionLines = 1`).
fn elided_line(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.replace('\n', " "),
        egui::TextFormat::simple(font, color),
    );
    job.wrap.max_width = max_w.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

fn reaction_userpic_count(r: &ReactionCounter) -> usize {
    usize::from(!r.chosen || r.count > 1) + usize::from(r.chosen)
}

fn reaction_count_text(r: &ReactionCounter) -> String {
    r.count.to_string()
}

fn reaction_button_width(r: &ReactionCounter, private_chat: bool, count_w: f32) -> f32 {
    if private_chat {
        let n = reaction_userpic_count(r) as f32;
        REACTION_PAD_LEFT
            + REACTION_SIZE
            + REACTION_USERPICS_PAD
            + REACTION_USERPIC
            + (n - 1.0).max(0.0) * (REACTION_USERPIC - REACTION_USERPIC_SHIFT)
    } else {
        REACTION_PAD_LEFT + REACTION_SIZE + REACTION_SKIP + REACTION_PAD_RIGHT + count_w
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_reactions_row(
    ui: &mut Ui,
    msg_id: MsgId,
    reactions: &[ReactionCounter],
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
    userpics: Option<&ReactionUserpics>,
    assets: &mut MediaAssetCache,
) -> Option<ReactionRowAction> {
    let mut toggled = None;
    let mut x = rect.left();
    let y = rect.top();
    let pill_h = REACTION_PAD_V * 2.0 + REACTION_SIZE;
    let ui_colors = colors.ui_colors();

    for (idx, r) in reactions.iter().enumerate() {
        let fg = if r.chosen {
            colors.reaction_chosen_fg
        } else if outgoing {
            colors.msg_out_date_fg
        } else {
            colors.active_text_fg
        };
        let bg = if r.chosen {
            colors.reaction_chosen_bg
        } else if outgoing {
            colors.reaction_out_bg
        } else {
            colors.reaction_in_bg
        };
        let count_galley = ui.painter().layout_no_wrap(
            reaction_count_text(r),
            FontId::new(12.0, tg_ui::fonts::family_semibold()),
            fg,
        );
        let pill_w = reaction_button_width(r, userpics.is_some(), count_galley.size().x);
        let pill_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(pill_w, pill_h));

        let resp = ui.interact(
            pill_rect,
            Id::unique(("tg_reaction", msg_id.0, idx)),
            Sense::click(),
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Button,
                true,
                r.chosen,
                format!("{} {}", r.emoji, r.count),
            )
        });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter()
            .rect_filled(pill_rect, CornerRadius::same((pill_h * 0.5) as u8), bg);
        let emoji_rect = Rect::from_min_size(
            Pos2::new(x + REACTION_PAD_LEFT, y + REACTION_PAD_V),
            Vec2::splat(REACTION_SIZE),
        );
        let ctx = ui.ctx().clone();
        let _ = assets.emoji().paint_emoji_str(
            ui.painter(),
            &ctx,
            &r.emoji,
            emoji_rect,
            Color32::WHITE,
        );
        if let Some(pics) = userpics {
            let mut list: Vec<&(PeerId, String)> = Vec::with_capacity(2);
            if !r.chosen || r.count > 1 {
                list.push(&pics.peer);
            }
            if r.chosen {
                list.push(&pics.me);
            }
            let mut px = emoji_rect.right() + REACTION_USERPICS_PAD * 0.5;
            for (peer, name) in list {
                let pic = Rect::from_min_size(
                    Pos2::new(px, pill_rect.center().y - REACTION_USERPIC * 0.5),
                    Vec2::splat(REACTION_USERPIC),
                );
                ui.painter()
                    .circle_filled(pic.center(), REACTION_USERPIC * 0.5 + 1.0, bg);
                EmptyUserpic::for_peer(peer.bare(), name, &ui_colors)
                    .paint_circle(ui.painter(), pic);
                px += REACTION_USERPIC - REACTION_USERPIC_SHIFT;
            }
        } else {
            ui.painter().galley(
                Pos2::new(
                    emoji_rect.right() + REACTION_SKIP,
                    pill_rect.center().y - count_galley.size().y * 0.5,
                ),
                count_galley,
                fg,
            );
        }

        if resp.clicked() {
            toggled = Some(ReactionRowAction::Toggle(r.emoji.clone()));
        }
        x += pill_w + REACTION_BETWEEN;
    }
    toggled
}

fn paint_bottom_info(
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
    let fg = if pill {
        colors.date_img_fg
    } else if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };

    if pill {
        let r = (rect.height() * 0.5).ceil() as u8;
        painter.rect_filled(rect, CornerRadius::same(r), colors.date_img_bg);
    }

    let mut parts = Vec::new();
    if let Some(ref bot) = msg.extra.via_bot {
        parts.push(format!("via @{bot}"));
    }
    if let Some(views) = msg.views {
        parts.push(format_compact_views(views));
    }
    if msg.edited {
        parts.push("edited".to_owned());
    }
    parts.push(crate::history::inner::message_time_text(msg));
    let text = parts.join("  ");

    let reserve_ticks = if outgoing { 16.0 } else { 0.0 };
    let pad_x = if pill { 7.0 } else { 0.0 };
    let text_right = rect.right() - pad_x - reserve_ticks;

    let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), fg);
    let mut cursor_x = text_right - galley.size().x;
    painter.galley(
        Pos2::new(cursor_x, rect.center().y - galley.size().y * 0.5),
        galley,
        fg,
    );
    if msg.views.is_some() {
        cursor_x -= 14.0;
        tg_app_core::ui_icons::paint_eye(
            painter,
            Pos2::new(cursor_x + 5.0, rect.center().y),
            11.0,
            fg,
        );
    }
    if msg.is_pinned {
        cursor_x -= 14.0;
        tg_app_core::ui_icons::paint_style_tinted(
            painter,
            Rect::from_center_size(
                Pos2::new(cursor_x + 5.0, rect.center().y),
                Vec2::splat(11.0),
            ),
            tg_ui::tg_style::st::menu_icon_pin,
            fg,
        );
    }
    if msg.extra.silent {
        cursor_x -= 14.0;
        tg_app_core::ui_icons::paint_style_tinted(
            painter,
            Rect::from_center_size(
                Pos2::new(cursor_x + 5.0, rect.center().y),
                Vec2::splat(11.0),
            ),
            tg_ui::tg_style::st::menu_icon_mute,
            fg,
        );
    }
    if msg.extra.effect_id.is_some() {
        cursor_x -= 13.0;
        tg_app_core::ui_icons::paint_sparkle(
            painter,
            Pos2::new(cursor_x + 5.0, rect.center().y),
            10.0,
            fg,
        );
    }

    // Outgoing send / read ticks painted as crisp vector strokes.
    if outgoing {
        let tx = rect.right() - pad_x - 12.0;
        let ty = rect.center().y;
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
        } else {
            // First check
            painter.add(Shape::line(
                vec![
                    Pos2::new(tx, ty + 0.5),
                    Pos2::new(tx + 2.8, ty + 3.2),
                    Pos2::new(tx + 7.8, ty - 3.0),
                ],
                stroke,
            ));
            // Second check when read
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
    }
}

/// Formats channel view counts compactly (`1.2K`, `4.8M`).
#[must_use]
pub fn format_compact_views(views: u32) -> String {
    if views >= 1_000_000 {
        format!("{:.1}M", f64::from(views) / 1_000_000.0)
    } else if views >= 1_000 {
        format!("{:.1}K", f64::from(views) / 1_000.0)
    } else {
        views.to_string()
    }
}

/// Computes the exact size of a message's reactions row for `MessageInput::reactions_size`
/// (`InlineList::countOptimalSize`; private chats show userpics instead of counts).
#[must_use]
pub fn measure_reactions_size(
    reactions: &[ReactionCounter],
    private_chat: bool,
) -> tg_history::Size {
    if reactions.is_empty() {
        return tg_history::Size::ZERO;
    }
    let mut w = 0.0f32;
    for (i, r) in reactions.iter().enumerate() {
        let digits = reaction_count_text(r).len() as f32;
        w += reaction_button_width(r, private_chat, digits * 7.0);
        if i + 1 < reactions.len() {
            w += REACTION_BETWEEN;
        }
    }
    tg_history::Size::new(
        w.ceil() as i32,
        (REACTION_PAD_V * 2.0 + REACTION_SIZE) as i32,
    )
}
