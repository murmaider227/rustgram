//! The sender name line at the top of a group message bubble (`Message::paintFromName`,
//! `history/view/history_view_message.cpp`): the name, then the premium star or the custom
//! emoji status, the inline bot ("via @bot", `HistoryMessageVia`), and the rank badge flush
//! right (`HistoryView::RightBadge`). Without a name line the inline bot gets a line of its own
//! (`Message::paintViaBotIdInfo`).

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Vec2};

use tg_ui::peer_color::{PeerAccent, peer_accent_name_color, peer_accent_name_color_selected};
use tg_ui::theme::Colors;

use crate::history::backend::{HistoryMessage, ReplyHeader, SenderBadge, SenderBadgeRole};
use crate::history::theme::HistoryColors;

/// `st::dialogsPremiumIcon.icon.width()`: room the status takes after the name.
pub const NAME_STATUS_WIDTH: i32 = 18;
/// `st::dialogsPremiumIcon` height.
const NAME_STATUS_HEIGHT: f32 = 16.0;
/// `st::msgPadding.right()`: gap between the name and the badge.
const BADGE_GAP: i32 = 11;
/// `st::msgTagBadgePadding` (`margins(5px, -1px, 5px, 0px)`).
const BADGE_PAD_X: i32 = 5;
const BADGE_PAD_TOP: i32 = -1;
const BADGE_PAD_BOTTOM: i32 = 0;
/// `st::msgFont->height`.
const MSG_FONT_HEIGHT: i32 = 18;
/// `st::msgNameFont->height`.
const NAME_FONT_HEIGHT: i32 = 18;
/// `rankAdminFg`.
const RANK_ADMIN_FG: Color32 = Color32::from_rgb(0x49, 0xa3, 0x55);
/// `rankOwnerFg`.
const RANK_OWNER_FG: Color32 = Color32::from_rgb(0x95, 0x6a, 0xc8);
/// `msgOutServiceFg` of the classic theme.
const MSG_OUT_SERVICE_FG: Color32 = Color32::from_rgb(0x45, 0xa3, 0x2d);
/// `st::emojiSize` and `Ui::Text::AdjustCustomEmojiSize(emojiSize)` (`* 1.12`).
const EMOJI_SIZE: f32 = 18.0;
const CUSTOM_EMOJI_SIZE: f32 = 20.0;

/// `st::msgNameFont` (13px semibold).
#[must_use]
pub fn name_font() -> FontId {
    FontId::new(13.0, tg_ui::fonts::family_semibold())
}

/// `st::defaultTextStyle` used for the badge text (13px regular).
fn badge_font() -> FontId {
    FontId::proportional(13.0)
}

/// Single-line badge text without emoji (`TextUtilities::SingleLine` + `RemoveEmoji`).
#[must_use]
pub fn badge_display_text(text: &str) -> String {
    let single: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !is_emoji_char(*c))
        .collect();
    single.trim().to_owned()
}

fn is_emoji_char(c: char) -> bool {
    matches!(u32::from(c),
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D | 0xE0020..=0xE007F)
}

fn text_width(painter: &Painter, text: &str, font: FontId) -> i32 {
    if text.is_empty() {
        return 0;
    }
    painter
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
        .ceil() as i32
}

struct PillGeometry {
    text_width: i32,
    width: i32,
    height: i32,
}

/// `ComputeBadgePillGeometry`.
fn pill_geometry(text_width: i32) -> PillGeometry {
    let height = BADGE_PAD_TOP + MSG_FONT_HEIGHT + BADGE_PAD_BOTTOM;
    PillGeometry {
        text_width,
        width: (BADGE_PAD_X + text_width + BADGE_PAD_X).max(height),
        height,
    }
}

/// Badge width (`RightBadge::width`, `Message::refreshRightBadge`): plain text for member
/// ranks, a pill for admins and the owner.
#[must_use]
pub fn badge_width(painter: &Painter, badge: &SenderBadge) -> i32 {
    let text = badge_display_text(&badge.text);
    let w = text_width(painter, &text, badge_font());
    match badge.role {
        SenderBadgeRole::User => w,
        SenderBadgeRole::Admin | SenderBadgeRole::Creator => pill_geometry(w).width,
    }
}

/// Widths the name line adds to the bubble (`performCountOptimalSize`): the name with the
/// status room, and the badge (`0` when none). `MessageInput::right_badge_width` takes the
/// second one; the layout adds `msgPadding.right()` before it.
#[must_use]
pub fn name_line_widths(painter: &Painter, msg: &HistoryMessage) -> (i32, i32) {
    let mut name = text_width(painter, &msg.sender_name, name_font());
    if has_status(msg) {
        name += NAME_STATUS_WIDTH;
    }
    let badge = msg
        .extra
        .sender_badge
        .as_ref()
        .map_or(0, |b| badge_width(painter, b));
    (name, badge)
}

/// Username of the inline bot a message was sent through, without the `@`.
fn via_bot_username(msg: &HistoryMessage) -> Option<&str> {
    let bot = msg.extra.via_bot.as_deref()?.trim_start_matches('@');
    (!bot.is_empty()).then_some(bot)
}

/// `HistoryMessageVia::text`: `via @bot` for a message sent through an inline bot.
#[must_use]
pub fn via_text(msg: &HistoryMessage) -> Option<String> {
    via_bot_username(msg).map(|bot| tg_lang::tr::lng_inline_bot_via(&format!("@{bot}")))
}

/// Forward header text (`HistoryMessageForwarded::create`): `lng_forwarded_via` names the inline
/// bot of a forwarded message there instead of on the name line.
#[must_use]
pub fn forwarded_header_text(
    msg: &HistoryMessage,
    forward: &crate::history::backend::ForwardHeader,
) -> String {
    match via_bot_username(msg) {
        Some(bot) => tg_lang::tr::lng_forwarded_via(&forward.from_name, &format!("@{bot}")),
        None => format!("Forwarded from {}", forward.from_name),
    }
}

/// `st::msgServiceFont->spacew`.
fn space_width(painter: &Painter) -> i32 {
    (text_width(painter, "x x", name_font()) - text_width(painter, "xx", name_font())).max(1)
}

/// Width the inline bot adds to the header (`MessageInput::via_bot_width`): `via->maxWidth`,
/// plus one more space after a status when it shares the name line (`performCountOptimalSize`).
#[must_use]
pub fn via_bot_width(painter: &Painter, msg: &HistoryMessage, on_name_line: bool) -> Option<i32> {
    let text = via_text(msg)?;
    let mut width = text_width(painter, &text, name_font());
    if on_name_line && has_status(msg) {
        width += space_width(painter);
    }
    Some(width)
}

/// `msgServiceFg`: the inline bot's colour (`msgInServiceFg` is `windowActiveTextFg`).
#[must_use]
pub fn via_fg(colors: &HistoryColors, outgoing: bool) -> Color32 {
    match (outgoing, colors.is_night) {
        (false, _) => colors.active_text_fg,
        (true, false) => MSG_OUT_SERVICE_FG,
        (true, true) => colors.msg_out_reply_bar,
    }
}

/// Paints `text` elided to `max_width` at `pos` in the name font; returns the painted width.
fn paint_elided(painter: &Painter, text: &str, pos: Pos2, max_width: f32, fg: Color32) -> f32 {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(name_font(), fg),
    );
    job.wrap.max_width = max_width.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    let width = galley.size().x.min(max_width);
    painter.galley(pos, galley, fg);
    width
}

/// `Message::paintViaBotIdInfo`: the inline bot alone on the header line when the bubble shows
/// no sender name.
pub fn paint_via_line(painter: &Painter, msg: &HistoryMessage, line: Rect, fg: Color32) {
    if let Some(text) = via_text(msg) {
        paint_elided(painter, &text, line.left_top(), line.width(), fg);
    }
}

fn has_status(msg: &HistoryMessage) -> bool {
    msg.extra
        .sender_status
        .is_some_and(|s| s.premium || s.emoji_status_id.is_some())
}

/// Paints the name line into `line` (the bubble's text column at the name row, `trect`).
/// `via_fg` is the inline bot's colour; `None` hides the bot (a forwarded message names it in
/// the forward header instead).
pub fn paint_from_name(
    painter: &Painter,
    msg: &HistoryMessage,
    line: Rect,
    name_fg: Color32,
    via_fg: Option<Color32>,
    colors: &HistoryColors,
    time_secs: f64,
) {
    let badge = msg.extra.sender_badge.as_ref();
    let badge_w = badge.map_or(0, |b| badge_width(painter, b)) as f32;
    let mut available = line.width();
    if badge_w > 0.0 {
        available -= BADGE_GAP as f32 + badge_w;
    }
    let status_w = if has_status(msg) {
        NAME_STATUS_WIDTH as f32
    } else {
        0.0
    };
    let via = via_fg.and_then(|fg| via_text(msg).map(|text| (text, fg)));
    let space_w = space_width(painter) as f32;
    let via_skip = via.as_ref().map_or(0.0, |(text, _)| {
        text_width(painter, text, name_font()) as f32 + space_w
    });
    let name_available = (if status_w > 0.0 && available > status_w {
        available - status_w
    } else {
        available
    } - via_skip)
        .max(0.0);

    let mut job = egui::text::LayoutJob::single_section(
        msg.sender_name.replace('\n', " "),
        egui::TextFormat::simple(name_font(), name_fg),
    );
    job.wrap.max_width = name_available.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    let name_w = galley.size().x.min(name_available);
    painter.galley(line.left_top(), galley, name_fg);

    if status_w > 0.0
        && available > status_w
        && let Some(status) = msg.extra.sender_status
    {
        let x = line.left() + name_w;
        let y = line.top();
        let tint = name_fg.gamma_multiply(115.0 / 255.0);
        let painted = status.emoji_status_id.is_some_and(|id| {
            // `FromNameStatus::skip = (emojiSize - AdjustCustomEmojiSize(emojiSize)) / 2`.
            let skip = ((EMOJI_SIZE - CUSTOM_EMOJI_SIZE) / 2.0).trunc();
            let rect = Rect::from_min_size(
                Pos2::new(x - 2.0 * skip, y + skip),
                Vec2::splat(CUSTOM_EMOJI_SIZE),
            );
            let id = id.to_string();
            tg_ui::custom_emoji::has_registered_custom_emoji(&id)
                && tg_ui::custom_emoji::paint_registered_custom_emoji(
                    &painter.ctx().clone(),
                    painter,
                    &id,
                    rect,
                    status_paint_context(painter, msg, &id, time_secs, tint),
                )
        });
        // An emoji status whose document is not loaded yet keeps the premium star's place.
        if !painted && (status.premium || status.emoji_status_id.is_some()) {
            tg_app_core::ui_icons::paint_style_tinted(
                painter,
                Rect::from_min_size(
                    Pos2::new(x, y),
                    Vec2::new(NAME_STATUS_WIDTH as f32, NAME_STATUS_HEIGHT),
                ),
                tg_ui::tg_style::st::dialogs_premium_icon.icon,
                tint,
            );
        }
    }

    // `skipWidth = nameWidth + (status ? icon.width + spacew : 0) + spacew`.
    if let Some((text, fg)) = via {
        let mut skip = name_w + space_w;
        if has_status(msg) {
            skip += NAME_STATUS_WIDTH as f32 + space_w;
        }
        let via_available = available - skip;
        if via_available > 0.0 {
            paint_elided(
                painter,
                &text,
                Pos2::new(line.left() + skip, line.top()),
                via_available,
                fg,
            );
        }
    }

    if let Some(badge) = badge {
        paint_badge(painter, badge, line, badge_w, colors);
    }
}

/// `kPlayStatusLimit`: the name-line status is a `LimitedLoopsEmoji` that plays this many loops.
const PLAY_STATUS_LIMIT: f64 = 2.0;

/// `LimitedLoopsEmoji(…, kPlayStatusLimit)` (`stopOnLast = false`): the status plays from its
/// first frame when it is first painted, twice, then stays on its first frame.
fn status_paint_context(
    painter: &Painter,
    msg: &HistoryMessage,
    emoji_id: &str,
    time_secs: f64,
    tint: Color32,
) -> tg_ui::custom_emoji::CustomEmojiPaintContext {
    let key = egui::Id::unique(("tg_from_name_status", msg.peer_id.0, msg.id.0, emoji_id));
    let started = painter
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or_insert_with(key, || time_secs));
    let elapsed = (time_secs - started).max(0.0);
    let done = tg_ui::custom_emoji::registered_custom_emoji_loop_secs(emoji_id)
        .is_none_or(|secs| elapsed >= secs * PLAY_STATUS_LIMIT);
    tg_ui::custom_emoji::CustomEmojiPaintContext {
        time_secs: elapsed,
        paused: done,
        force_first_frame: done,
        force_last_frame: false,
        text_color: tint,
    }
}

fn paint_badge(
    painter: &Painter,
    badge: &SenderBadge,
    line: Rect,
    badge_w: f32,
    colors: &HistoryColors,
) {
    let text = badge_display_text(&badge.text);
    let left = line.right() - badge_w;
    match badge.role {
        SenderBadgeRole::User => {
            painter.text(
                Pos2::new(left, line.top()),
                Align2::LEFT_TOP,
                text,
                badge_font(),
                colors.sub_text_fg,
            );
        }
        SenderBadgeRole::Admin | SenderBadgeRole::Creator => {
            let fg = if badge.role == SenderBadgeRole::Creator {
                RANK_OWNER_FG
            } else {
                RANK_ADMIN_FG
            };
            let pill = pill_geometry(text_width(painter, &text, badge_font()));
            let top = line.top() + ((NAME_FONT_HEIGHT - pill.height) / 2) as f32;
            let rect = Rect::from_min_size(
                Pos2::new(left, top),
                Vec2::new(pill.width as f32, pill.height as f32),
            );
            let radius = (pill.height / 2) as u8;
            painter.rect_filled(rect, CornerRadius::same(radius), fg.gamma_multiply(0.15));
            painter.text(
                Pos2::new(
                    left + ((pill.width - pill.text_width) / 2) as f32,
                    top + BADGE_PAD_TOP as f32,
                ),
                Align2::LEFT_TOP,
                text,
                badge_font(),
                fg,
            );
        }
    }
}

/// The sender's accent (`Element::colorIndex` / `colorCollectible`): the server's, or the
/// id-based default when the message carries none.
#[must_use]
pub fn sender_accent(msg: &HistoryMessage) -> PeerAccent {
    msg.extra
        .sender_accent
        .unwrap_or_else(|| PeerAccent::for_peer_id(msg.from_id.bare()))
}

/// Picks the name colour of `accent` for the active theme (`Ui::FromNameFg`).
fn accent_name_fg(
    accent: PeerAccent,
    colors: &HistoryColors,
    ui_colors: &dyn Colors,
    selected: bool,
) -> Color32 {
    if selected {
        peer_accent_name_color_selected(
            accent,
            ui_colors,
            colors.is_night,
            colors.msg_in_bg_selected,
        )
    } else {
        peer_accent_name_color(accent, ui_colors, colors.is_night)
    }
}

/// Colour of the sender name line (`Message::paintFromName`: `FromNameFg(context, colorIndex(),
/// colorCollectible())`). `ui_colors` must be `colors.ui_colors()`.
#[must_use]
pub fn sender_name_fg(
    msg: &HistoryMessage,
    colors: &HistoryColors,
    ui_colors: &dyn Colors,
    selected: bool,
) -> Color32 {
    accent_name_fg(sender_accent(msg), colors, ui_colors, selected)
}

/// Colour of the author name in an incoming reply header (`Reply::paint`, `_colorPeer`).
#[must_use]
pub fn reply_author_name_fg(
    reply: &ReplyHeader,
    colors: &HistoryColors,
    ui_colors: &dyn Colors,
    selected: bool,
) -> Color32 {
    let accent = reply
        .author_accent
        .unwrap_or_else(|| PeerAccent::for_peer_id(reply.author_color_id));
    accent_name_fg(accent, colors, ui_colors, selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_text_is_single_line_without_emoji() {
        assert_eq!(badge_display_text("няша"), "няша");
        assert_eq!(badge_display_text("Оригинал\nМудрец 🔥"), "Оригинал Мудрец");
    }

    #[test]
    fn pill_geometry_matches_compute_badge_pill_geometry() {
        // height = -1 + 18 + 0; width = max(5 + text + 5, height).
        let p = pill_geometry(30);
        assert_eq!((p.width, p.height), (40, 17));
        assert_eq!(pill_geometry(3).width, 17);
    }
}
