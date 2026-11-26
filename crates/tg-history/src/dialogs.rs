//! Dialogs list row layout, badge placement, text elision, and date formatting.
//!
//! Port of:
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_layout.h` & `dialogs_layout.cpp`
//! - `Telegram/SourceFiles/dialogs/dialogs_row.h` & `dialogs_row.cpp`
//! - `Telegram/SourceFiles/ui/text/format_values.cpp` (`Ui::FormatDialogsDate`)
//! - `Telegram/SourceFiles/ui/unread_counter_format.cpp` (`FormatUnreadCounter`, `FormatUnreadCounterShort`)

use crate::consts::{
    COLUMN_MINIMAL_WIDTH_LEFT, DIALOGS_CALL_BADGE_SIZE, DIALOGS_CALL_BADGE_SKIP,
    DIALOGS_CHAT_TYPE_ICON_WIDTH, DIALOGS_CHAT_TYPE_SKIP, DIALOGS_DATE_RECENTLY_SECONDS,
    DIALOGS_DATE_SKIP, DIALOGS_ICON_BADGE_SIZE, DIALOGS_IMPORTANT_BAR_HEIGHT,
    DIALOGS_MUTE_ICON_SIZE, DIALOGS_MUTE_ICON_SKIP, DIALOGS_ONLINE_BADGE_SIZE,
    DIALOGS_ONLINE_BADGE_SKIP, DIALOGS_PINNED_ICON_SIZE, DIALOGS_ROW_HEIGHT,
    DIALOGS_SEND_STATE_SKIP, DIALOGS_TOP_BAR_LEFT_PADDING, DIALOGS_UNREAD_HEIGHT,
    DIALOGS_UNREAD_PADDING, FontMetrics,
};
use crate::datetime::{LocalDateTime, format_time_short};
use crate::geometry::{Margins, Rect};

/// Row style metrics (`DialogRow` struct in `dialogs/dialogs.style:14-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogRowStyle {
    /// Total height of the row in logical pixels.
    pub height: i32,
    /// Outer padding (`left`, `top`, `right`, `bottom`).
    pub padding: Margins,
    /// Avatar / userpic square diameter.
    pub photo_size: i32,
    /// X coordinate of the chat title / name.
    pub name_left: i32,
    /// Y coordinate of the chat title / name.
    pub name_top: i32,
    /// X coordinate of the message preview / typing line.
    pub text_left: i32,
    /// Y coordinate of the message preview / typing line.
    pub text_top: i32,
    /// Gap between topic pills in forum rows.
    pub topics_skip: i32,
    /// Large gap for forum topics row.
    pub topics_skip_big: i32,
    /// Extra height reserved for forum topic chips (`0` for non-forum rows).
    pub topics_height: i32,
    /// Diameter of dot unread mark when unread without counter (`8px` in forum topics).
    pub unread_mark_diameter: i32,
    /// Y coordinate of folder filter tags (`0` when untagged).
    pub tag_top: i32,
}

impl DialogRowStyle {
    /// `defaultDialogRow` (`dialogs/dialogs.style:95-103`), height 62px.
    pub const DEFAULT: Self = Self {
        height: DIALOGS_ROW_HEIGHT,
        padding: Margins::new(10, 8, 10, 8),
        photo_size: 46,
        name_left: 68,
        name_top: 10,
        text_left: 68,
        text_top: 34,
        topics_skip: 0,
        topics_skip_big: 0,
        topics_height: 0,
        unread_mark_diameter: 0,
        tag_top: 0,
    };

    /// `taggedDialogRow` (`dialogs/dialogs.style:114-118`), height 72px.
    pub const TAGGED: Self = Self {
        height: 72,
        text_top: 30,
        tag_top: 52,
        ..Self::DEFAULT
    };

    /// `forumDialogRow` (`dialogs/dialogs.style:119-125`), height 80px.
    pub const FORUM: Self = Self {
        height: 80,
        text_top: 32,
        topics_skip: 8,
        topics_skip_big: 14,
        topics_height: 21,
        ..Self::DEFAULT
    };

    /// `taggedForumDialogRow` (`dialogs/dialogs.style:126-129`), height 96px.
    pub const TAGGED_FORUM: Self = Self {
        height: 96,
        tag_top: 77,
        ..Self::FORUM
    };

    /// `forumTopicRow` (`dialogs/dialogs.style:632-641`), compact 54px topic row.
    pub const FORUM_TOPIC: Self = Self {
        height: 54,
        padding: Margins::new(8, 7, 10, 7),
        photo_size: 20,
        name_left: 39,
        name_top: 7,
        text_left: 39,
        text_top: 29,
        topics_skip: 0,
        topics_skip_big: 0,
        topics_height: 0,
        unread_mark_diameter: 8,
        tag_top: 0,
    };

    /// Reads a [`tg_style::structs::DialogRow`] scaled to `scale`.
    #[must_use]
    pub fn from_style(style: &tg_style::structs::DialogRow, scale: tg_style::Scale) -> Self {
        let p = style.padding.scaled_at(scale);
        Self {
            height: style.height.scaled_at(scale),
            padding: Margins::new(p.left, p.top, p.right, p.bottom),
            photo_size: style.photo_size.scaled_at(scale),
            name_left: style.name_left.scaled_at(scale),
            name_top: style.name_top.scaled_at(scale),
            text_left: style.text_left.scaled_at(scale),
            text_top: style.text_top.scaled_at(scale),
            topics_skip: style.topics_skip.scaled_at(scale),
            topics_skip_big: style.topics_skip_big.scaled_at(scale),
            topics_height: style.topics_height.scaled_at(scale),
            unread_mark_diameter: style.unread_mark_diameter.scaled_at(scale),
            tag_top: style.tag_top.scaled_at(scale),
        }
    }
}

/// Compact vs normal row presentation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogRowMode {
    /// Full-width row showing avatar, title, date, preview, and badges.
    Normal,
    /// Narrow/compact column mode showing only avatar and overlaid counter badges.
    Narrow,
    /// Collapsed bar (e.g. collapsed Archive folder, 37px tall).
    Collapsed,
}

/// Classification of how a dialog timestamp should be formatted (`Ui::FormatDialogsDate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogsDateKind {
    /// Same calendar day or within the last 20 hours (`HH:MM` / short time).
    Time,
    /// Within the last 6 calendar days (`Mon`, `Tue`, ..., `Sun`).
    Weekday,
    /// Older than a week (`DD.MM.YY` / short date).
    Date,
}

/// Determines whether `last_time` relative to `now` formats as a time, weekday, or calendar date.
///
/// Exact port of the branching rules in `Ui::FormatDialogsDate`
/// (`Telegram/SourceFiles/ui/text/format_values.cpp:724-740`):
/// - Same day OR `|last_time - now| < 20 hours` -> [`DialogsDateKind::Time`]
/// - `|days_between(last_time.date, now.date)| < 7` -> [`DialogsDateKind::Weekday`]
/// - Otherwise -> [`DialogsDateKind::Date`]
#[must_use]
pub fn classify_dialogs_date(last_time: LocalDateTime, now: LocalDateTime) -> DialogsDateKind {
    if last_time.date == now.date || last_time.secs_to(now).abs() < DIALOGS_DATE_RECENTLY_SECONDS {
        DialogsDateKind::Time
    } else if last_time.date.days_to(now.date).abs() < 7 {
        DialogsDateKind::Weekday
    } else {
        DialogsDateKind::Date
    }
}

/// Formats a dialog row timestamp relative to `now` (`Ui::FormatDialogsDate`).
///
/// - [`DialogsDateKind::Time`][]: `"H:mm"` (no leading zero on single-digit hours, matching
///   `QLocale::ShortFormat`) when `use_24h` is `true`, otherwise `"H:MM AM/PM"`.
/// - [`DialogsDateKind::Weekday`][]: short English weekday (`"Mon"`..`"Sun"`).
/// - [`DialogsDateKind::Date`][]: `"DD.MM.YY"`.
#[must_use]
pub fn format_dialogs_date(last_time: LocalDateTime, now: LocalDateTime, use_24h: bool) -> String {
    match classify_dialogs_date(last_time, now) {
        DialogsDateKind::Time => {
            if use_24h {
                format_time_short(last_time)
            } else {
                let (hour12, suffix) = match last_time.hour {
                    0 => (12, "AM"),
                    1..=11 => (last_time.hour, "AM"),
                    12 => (12, "PM"),
                    h => (h - 12, "PM"),
                };
                format!("{hour12}:{:02} {suffix}", last_time.minute)
            }
        }
        DialogsDateKind::Weekday => last_time.date.weekday().short_name().to_string(),
        DialogsDateKind::Date => {
            let yy = last_time.date.year.rem_euclid(100);
            format!(
                "{:02}.{:02}.{:02}",
                last_time.date.day, last_time.date.month, yy
            )
        }
    }
}

/// Formats an unread message counter for display in a dialog badge
/// (`FormatUnreadCounter` in `ui/unread_counter_format.cpp:10-27`).
#[must_use]
pub fn format_unread_counter(
    unread_counter: i32,
    has_mention_or_reaction: bool,
    narrow: bool,
) -> String {
    if unread_counter <= 0 {
        return String::new();
    }
    if !narrow {
        return unread_counter.to_string();
    }
    if has_mention_or_reaction && unread_counter > 999 {
        return "99+".to_string();
    }
    if unread_counter > 999_999 {
        return "99999+".to_string();
    }
    unread_counter.to_string()
}

/// Formats a large counter with `K`/`M`/`B` suffix
/// (`FormatUnreadCounterShort` in `ui/unread_counter_format.cpp:29-37`).
#[must_use]
pub fn format_unread_counter_short(unread_counter: i32) -> String {
    if unread_counter < 1_000 {
        unread_counter.to_string()
    } else if unread_counter < 1_000_000 {
        format!("{}K", unread_counter / 1_000)
    } else if unread_counter < 1_000_000_000 {
        format!("{}M", unread_counter / 1_000_000)
    } else {
        format!("{}B", unread_counter / 1_000_000_000)
    }
}

/// Elides single-line text on the right with `'…'` (`U+2026`) so its measured width fits
/// within `max_width` (`QFontMetrics::elidedText(..., Qt::ElideRight)`).
///
/// `measure` returns the pixel width of any string slice and must not shrink when the slice
/// grows, which holds for every font. The result keeps the longest prefix that fits together
/// with the ellipsis. The cut is searched from a first guess proportional to the widths, so it
/// takes a handful of `measure` calls where measuring every prefix took one per character.
#[must_use]
pub fn elide_text_right<F>(text: &str, max_width: i32, measure: F) -> String
where
    F: FnMut(&str) -> i32,
{
    elide_right_from(text, max_width, None, measure)
}

/// [`elide_text_right`] for a caller that already knows where the cut should be, for example
/// from the glyph positions of the whole string laid out once.
///
/// `hint_end` is the byte length of the prefix expected to be kept. It only tells the search
/// where to start: the result is the same as [`elide_text_right`]'s for any hint, and a correct
/// hint is confirmed with two `measure` calls.
#[must_use]
pub fn elide_text_right_hinted<F>(text: &str, max_width: i32, hint_end: usize, measure: F) -> String
where
    F: FnMut(&str) -> i32,
{
    elide_right_from(text, max_width, Some(hint_end), measure)
}

fn elide_right_from<F>(
    text: &str,
    max_width: i32,
    hint_end: Option<usize>,
    mut measure: F,
) -> String
where
    F: FnMut(&str) -> i32,
{
    if max_width <= 0 || text.is_empty() {
        return String::new();
    }
    let full_width = measure(text);
    if full_width <= max_width {
        return text.to_string();
    }
    const ELLIPSIS: &str = "\u{2026}";
    let ellipsis_w = measure(ELLIPSIS);
    if ellipsis_w > max_width {
        return String::new();
    }
    let budget = max_width - ellipsis_w;
    let first_probe = hint_end.unwrap_or_else(|| {
        let scaled = text.len() as i64 * i64::from(budget) / i64::from(full_width.max(1));
        usize::try_from(scaled).unwrap_or(0)
    });

    // The prefix ending at `fits_end` fits (the empty one always does); the one ending at
    // `wide_end` does not (the whole text is wider than `max_width`). The search narrows the
    // two to neighbouring characters.
    let mut fits_end = 0usize;
    let mut wide_end = text.len();
    let mut search = CutSearch::Start(first_probe);
    while let Some(probe) = boundary_between(text, fits_end, wide_end, search.target()) {
        let fits = measure(&text[..probe]) <= budget;
        if fits {
            fits_end = probe;
        } else {
            wide_end = probe;
        }
        search = search.after(fits, fits_end, wide_end);
    }

    let mut out = String::with_capacity(fits_end + ELLIPSIS.len());
    out.push_str(&text[..fits_end]);
    out.push_str(ELLIPSIS);
    out
}

/// Where [`elide_right_from`] measures next.
#[derive(Clone, Copy)]
enum CutSearch {
    /// The first guess.
    Start(usize),
    /// Every probe so far fell on the same side of the cut: step away from the last one, twice
    /// as far each time. A good first guess ends here after one step.
    Gallop {
        target: usize,
        step: usize,
        upward: bool,
    },
    /// Probes on both sides of the cut: halve the range between them.
    Bisect(usize),
}

impl CutSearch {
    const fn target(self) -> usize {
        match self {
            Self::Start(target) | Self::Bisect(target) | Self::Gallop { target, .. } => target,
        }
    }

    fn after(self, fits: bool, fits_end: usize, wide_end: usize) -> Self {
        let gallop = |step: usize| Self::Gallop {
            target: if fits {
                fits_end.saturating_add(step)
            } else {
                wide_end.saturating_sub(step)
            },
            step,
            upward: fits,
        };
        match self {
            Self::Start(_) => gallop(1),
            Self::Gallop { step, upward, .. } if upward == fits => gallop(step.saturating_mul(2)),
            Self::Gallop { .. } | Self::Bisect(_) => {
                Self::Bisect(fits_end + (wide_end - fits_end) / 2)
            }
        }
    }
}

/// The character boundary nearest to `target` that lies strictly between `after` and `before`,
/// or `None` when no character ends between them.
fn boundary_between(text: &str, after: usize, before: usize, target: usize) -> Option<usize> {
    if before <= after + 1 {
        return None;
    }
    let target = target.clamp(after + 1, before - 1);
    let mut down = target;
    while !text.is_char_boundary(down) {
        down -= 1;
    }
    if down > after {
        return Some(down);
    }
    let mut up = target;
    while !text.is_char_boundary(up) {
        up += 1;
    }
    (up < before).then_some(up)
}

/// Unread, mention, reaction, and poll badge flags for a dialog row (`Dialogs::BadgesState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialogBadgesState {
    /// Unread message count (0 may still show an unread dot/mark if `unread` is true).
    pub unread_counter: i32,
    /// True if the dialog has unread messages or is marked unread.
    pub unread: bool,
    /// True if unread notifications are muted.
    pub unread_muted: bool,
    /// True if there is an unread `@` mention.
    pub mention: bool,
    /// True if mention badge is muted.
    pub mention_muted: bool,
    /// True if there is an unread reaction (`♥`).
    pub reaction: bool,
    /// True if reaction badge is muted.
    pub reaction_muted: bool,
    /// True if there is an unread poll badge.
    pub poll: bool,
    /// True if poll badge is muted.
    pub poll_muted: bool,
}

impl DialogBadgesState {
    /// Returns `true` if no badges should be displayed.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.unread && !self.mention && !self.reaction && !self.poll
    }
}

/// Outgoing message delivery indicator on the top-right of a dialog row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DialogSendState {
    /// No check/clock icon.
    #[default]
    None,
    /// Message is currently sending (clock icon).
    Sending,
    /// Message sent, not yet read by recipient (single check).
    Sent,
    /// Message read by recipient (double check).
    Received,
    /// Closed forum topic lock icon.
    Locked,
}

/// Chat type icon displayed before the chat title.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DialogChatTypeIcon {
    /// No leading icon (ordinary private user chat or monoforum).
    #[default]
    None,
    /// Group chat icon.
    Group,
    /// Broadcast channel icon.
    Channel,
    /// Forum / community icon.
    Forum,
    /// Bot icon.
    Bot,
}

/// Corner indicator overlaid on the bottom-right of the avatar (`dialogs_row.cpp:471-489`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AvatarCornerBadge {
    /// No corner badge.
    #[default]
    None,
    /// Green online dot for a user (`dialogsOnlineBadgeSize = 10px`).
    Online,
    /// Active group call indicator for a channel (`dialogsCallBadgeSize = 16px`).
    ActiveCall,
}

/// Input parameters for computing a dialog row's layout (`dialogs_layout.cpp`).
#[derive(Debug, Clone)]
pub struct DialogRowInput {
    /// Row style preset (`DEFAULT`, `TAGGED`, `FORUM`, `TAGGED_FORUM`, `FORUM_TOPIC`).
    pub style: DialogRowStyle,
    /// Presentation mode (`Normal`, `Narrow`, `Collapsed`).
    pub mode: DialogRowMode,
    /// Full row width in logical pixels.
    pub width: i32,
    /// Leading chat type icon (bot/group/channel/forum).
    pub chat_type_icon: DialogChatTypeIcon,
    /// Measured width of the chat title text.
    pub name_text_width: i32,
    /// Width of peer badge after the name (verified/premium/scam), `0` if none.
    pub peer_badge_width: i32,
    /// Whether the muted speaker icon is drawn next to the name.
    pub show_mute_icon: bool,
    /// Measured width of the formatted date/time string (`0` if no date shown, e.g. folders).
    pub date_text_width: i32,
    /// Outgoing message delivery indicator.
    pub send_state: DialogSendState,
    /// Unread / mention / reaction / poll badge state.
    pub badges: DialogBadgesState,
    /// Measured width of the formatted unread counter text (used when `badges.unread_counter > 0`).
    pub unread_counter_text_width: i32,
    /// True if the dialog is pinned (shown when badges are empty and no right button).
    pub display_pinned_icon: bool,
    /// Optional right action button width (e.g. "Open" bot button, only shown when `width >= 260`).
    pub right_button_width: i32,
    /// True if a typing/recording send-action indicator replaces the message preview.
    pub is_typing: bool,
    /// Avatar corner indicator (online dot / active call).
    pub avatar_badge: AvatarCornerBadge,
    /// True if this row has an active draft (suppresses narrow unread counter overlay).
    pub has_draft: bool,
    /// True if the row has a non-empty last message.
    pub has_item: bool,
    /// True if this row is a forum/monoforum showing an extra topics line.
    pub has_topics_line: bool,
}

impl Default for DialogRowInput {
    fn default() -> Self {
        Self {
            style: DialogRowStyle::DEFAULT,
            mode: DialogRowMode::Normal,
            width: 300,
            chat_type_icon: DialogChatTypeIcon::None,
            name_text_width: 0,
            peer_badge_width: 0,
            show_mute_icon: false,
            date_text_width: 0,
            send_state: DialogSendState::None,
            badges: DialogBadgesState::default(),
            unread_counter_text_width: 0,
            display_pinned_icon: false,
            right_button_width: 0,
            is_typing: false,
            avatar_badge: AvatarCornerBadge::None,
            has_draft: false,
            has_item: true,
            has_topics_line: false,
        }
    }
}

/// Computed rectangles for all elements inside a dialog list row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogRowLayout {
    /// Total row bounding rectangle `(0, 0, width, height)`.
    pub row_rect: Rect,
    /// Avatar rectangle.
    pub photo_rect: Rect,
    /// Online dot or active call badge rectangle on the avatar corner, if visible.
    pub avatar_badge_rect: Option<Rect>,
    /// Leading chat type icon rectangle, if visible.
    pub chat_type_icon_rect: Option<Rect>,
    /// Available rectangle for the chat name (elided to 1 line within this width).
    pub name_rect: Rect,
    /// Peer badge (verified / premium / scam) rectangle placed immediately after the name.
    pub peer_badge_rect: Option<Rect>,
    /// Mute icon rectangle placed after name + peer badge, if enabled.
    pub mute_icon_rect: Option<Rect>,
    /// Date / time text rectangle at the top-right.
    pub date_rect: Option<Rect>,
    /// Sent / received / sending / lock icon rectangle to the left of the date.
    pub send_state_rect: Option<Rect>,
    /// Message preview rectangle (or `None` when replaced by typing indicator or narrow mode).
    pub message_rect: Option<Rect>,
    /// Typing / send-action indicator rectangle (replaces `message_rect` when typing).
    pub typing_rect: Option<Rect>,
    /// Unread counter pill or unread dot rectangle.
    pub unread_badge_rect: Option<Rect>,
    /// Pinned icon rectangle (only shown when unread badges are empty).
    pub pinned_icon_rect: Option<Rect>,
    /// Unread `@` mention badge rectangle.
    pub mention_badge_rect: Option<Rect>,
    /// Unread reaction (`♥`) badge rectangle.
    pub reaction_badge_rect: Option<Rect>,
    /// Unread poll badge rectangle.
    pub poll_badge_rect: Option<Rect>,
    /// Optional right action button rectangle (e.g. bot "Open").
    pub right_button_rect: Option<Rect>,
    /// Whether the chat name needs elision (`name_text_width > name_rect.width`).
    pub name_elided: bool,
}

/// Computes the width of an unread counter pill (`PaintUnreadBadge` / `CountUnreadBadgeSize`).
#[must_use]
pub fn unread_badge_width(counter_text_width: i32) -> i32 {
    let min_size = DIALOGS_UNREAD_HEIGHT;
    if counter_text_width <= 0 {
        min_size
    } else {
        min_size.max(counter_text_width + 2 * DIALOGS_UNREAD_PADDING)
    }
}

/// Computes the baseline Y offset for a collapsed dialog row (`CollapsedRowTextBaseline`,
/// `dialogs_layout.cpp:1401-1405`).
#[must_use]
pub const fn collapsed_row_text_baseline(fonts: &FontMetrics) -> i32 {
    (DIALOGS_IMPORTANT_BAR_HEIGHT - DIALOGS_UNREAD_HEIGHT) / 2
        + (DIALOGS_UNREAD_HEIGHT - fonts.dialogs_unread.height) / 2
        + fonts.dialogs_unread.ascent
}

/// Computes the full geometry of a dialog row (`PaintRow` / `PaintBadges` in `dialogs_layout.cpp`
/// and `Row::PaintCornerBadgeFrame` in `dialogs_row.cpp`).
#[must_use]
pub fn layout_dialog_row(input: &DialogRowInput, fonts: &FontMetrics) -> DialogRowLayout {
    if matches!(input.mode, DialogRowMode::Collapsed) {
        return layout_collapsed_row(input, fonts);
    }

    let st = input.style;
    let narrow = matches!(input.mode, DialogRowMode::Narrow);
    let mut layout = DialogRowLayout {
        row_rect: Rect::new(0, 0, input.width, st.height),
        photo_rect: Rect::new(
            st.padding.left,
            st.padding.top,
            st.photo_size,
            st.photo_size,
        ),
        ..Default::default()
    };

    // In narrow mode, unread badges sit over the bottom-right corner of the avatar and
    // suppress the online/call corner badge (dialogs_layout.cpp:545-549, dialogs_row.cpp:472).
    let has_narrow_badges_above =
        narrow && !input.badges.is_empty() && !input.has_draft && input.has_item;

    if !has_narrow_badges_above {
        layout.avatar_badge_rect = match input.avatar_badge {
            AvatarCornerBadge::None => None,
            AvatarCornerBadge::Online => Some(Rect::new(
                layout.photo_rect.x + st.photo_size
                    - DIALOGS_ONLINE_BADGE_SKIP.x
                    - DIALOGS_ONLINE_BADGE_SIZE,
                layout.photo_rect.y + st.photo_size
                    - DIALOGS_ONLINE_BADGE_SKIP.y
                    - DIALOGS_ONLINE_BADGE_SIZE,
                DIALOGS_ONLINE_BADGE_SIZE,
                DIALOGS_ONLINE_BADGE_SIZE,
            )),
            AvatarCornerBadge::ActiveCall => Some(Rect::new(
                layout.photo_rect.x + st.photo_size
                    - DIALOGS_CALL_BADGE_SKIP.x
                    - DIALOGS_CALL_BADGE_SIZE,
                layout.photo_rect.y + st.photo_size
                    - DIALOGS_CALL_BADGE_SKIP.y
                    - DIALOGS_CALL_BADGE_SIZE,
                DIALOGS_CALL_BADGE_SIZE,
                DIALOGS_CALL_BADGE_SIZE,
            )),
        };
    }

    if narrow {
        if !input.has_draft && input.has_item {
            let top = st.padding.top + st.photo_size - DIALOGS_UNREAD_HEIGHT;
            let right = st.padding.left + st.photo_size;
            place_badges(input, right, top, 0, true, &mut layout);
        }
        return layout;
    }

    let name_left = st.name_left;
    let name_width = (input.width - name_left - st.padding.right).max(0);
    let mut rect_for_name = Rect::new(name_left, st.name_top, name_width, fonts.semibold.height);

    // Leading chat type icon (dialogs_layout.cpp:600-607).
    if !matches!(input.chat_type_icon, DialogChatTypeIcon::None) {
        layout.chat_type_icon_rect = Some(Rect::new(
            rect_for_name.x,
            rect_for_name.y,
            DIALOGS_CHAT_TYPE_ICON_WIDTH,
            fonts.semibold.height,
        ));
        let shift = DIALOGS_CHAT_TYPE_ICON_WIDTH + DIALOGS_CHAT_TYPE_SKIP;
        rect_for_name.x += shift;
        rect_for_name.width = (rect_for_name.width - shift).max(0);
    }

    // Top-right date text (PaintRowTopRight, dialogs_layout.cpp:87-107).
    if input.date_text_width > 0 {
        rect_for_name.width =
            (rect_for_name.width - input.date_text_width - DIALOGS_DATE_SKIP).max(0);
        let date_x = rect_for_name.right() + DIALOGS_DATE_SKIP;
        layout.date_rect = Some(Rect::new(
            date_x,
            rect_for_name.y,
            input.date_text_width,
            fonts.date.height,
        ));
    }

    // Send state icon to the left of date (dialogs_layout.cpp:861-864).
    if !matches!(input.send_state, DialogSendState::None) {
        rect_for_name.width = (rect_for_name.width - DIALOGS_SEND_STATE_SKIP).max(0);
        layout.send_state_rect = Some(Rect::new(
            rect_for_name.right(),
            rect_for_name.y,
            DIALOGS_SEND_STATE_SKIP,
            fonts.semibold.height,
        ));
    }

    // Mute icon reservation (dialogs_layout.cpp:939-948).
    if input.show_mute_icon {
        rect_for_name.width =
            (rect_for_name.width - DIALOGS_MUTE_ICON_SIZE - DIALOGS_MUTE_ICON_SKIP).max(0);
    }

    // Peer badge reservation (verified/premium/scam, dialogs_layout.cpp:867-902).
    let badge_width = input.peer_badge_width.max(0);
    if badge_width > 0 {
        rect_for_name.width = (rect_for_name.width - badge_width).max(0);
        let clamped_name_w = input.name_text_width.min(rect_for_name.width);
        layout.peer_badge_rect = Some(Rect::new(
            rect_for_name.x + clamped_name_w,
            rect_for_name.y,
            badge_width,
            fonts.semibold.height,
        ));
    }

    // Mute icon placed right after clamped name + badge (dialogs_layout.cpp:965-980).
    if input.show_mute_icon {
        let clamped_name_w = input.name_text_width.min(rect_for_name.width);
        let mute_left = rect_for_name.x + clamped_name_w + badge_width + DIALOGS_MUTE_ICON_SKIP;
        let mute_top = rect_for_name.y + (fonts.semibold.height - DIALOGS_MUTE_ICON_SIZE) / 2;
        layout.mute_icon_rect = Some(Rect::new(
            mute_left,
            mute_top,
            DIALOGS_MUTE_ICON_SIZE,
            DIALOGS_MUTE_ICON_SIZE,
        ));
    }

    layout.name_rect = rect_for_name;
    layout.name_elided = input.name_text_width > rect_for_name.width;

    // Second row: badges on the right, preview or typing indicator on the left
    // (PaintWideCounter, dialogs_layout.cpp:322-343).
    let text_top = st.text_top;
    let badge_top = text_top + fonts.normal.ascent
        - fonts.dialogs_unread.ascent
        - (DIALOGS_UNREAD_HEIGHT - fonts.dialogs_unread.height) / 2;
    let right_edge = input.width - st.padding.right;
    let used = place_badges(input, right_edge, badge_top, text_top, false, &mut layout);
    let available_width = (name_width - used).max(0);
    let preview_height = if input.has_topics_line && st.topics_height > 0 {
        st.topics_height + fonts.normal.height
    } else {
        fonts.normal.height
    };
    let preview_rect = Rect::new(st.text_left, text_top, available_width, preview_height);
    if input.is_typing {
        layout.typing_rect = Some(Rect::new(
            st.text_left,
            text_top,
            available_width,
            fonts.normal.height,
        ));
    } else {
        layout.message_rect = Some(preview_rect);
    }

    layout
}

fn layout_collapsed_row(input: &DialogRowInput, fonts: &FontMetrics) -> DialogRowLayout {
    let height = DIALOGS_IMPORTANT_BAR_HEIGHT;
    let unread_top = (height - DIALOGS_UNREAD_HEIGHT) / 2;
    let mut layout = DialogRowLayout {
        row_rect: Rect::new(0, 0, input.width, height),
        ..Default::default()
    };
    let mut name_right = input.width - input.style.padding.right;
    if input.badges.unread_counter > 0 {
        let badge_w = unread_badge_width(input.unread_counter_text_width);
        let badge_x = name_right - badge_w;
        layout.unread_badge_rect = Some(Rect::new(
            badge_x,
            unread_top,
            badge_w,
            DIALOGS_UNREAD_HEIGHT,
        ));
        name_right = badge_x - DIALOGS_UNREAD_PADDING;
    }
    let name_left = DIALOGS_TOP_BAR_LEFT_PADDING;
    let name_w = (name_right - name_left).max(0);
    let name_top = collapsed_row_text_baseline(fonts) - fonts.semibold.ascent;
    layout.name_rect = Rect::new(name_left, name_top, name_w, fonts.semibold.height);
    layout.name_elided = input.name_text_width > name_w;
    layout
}

/// Exact port of `PaintBadges` (`dialogs/ui/dialogs_layout.cpp:160-286`).
/// Returns the total horizontal width consumed from `right`.
fn place_badges(
    input: &DialogRowInput,
    initial_right: i32,
    top: i32,
    pinned_icon_top: i32,
    narrow: bool,
    layout: &mut DialogRowLayout,
) -> i32 {
    let mut right = initial_right;
    let mut painted = 0;
    let b = input.badges;
    let unread_size = DIALOGS_UNREAD_HEIGHT;

    if b.unread && b.unread_counter == 0 && input.style.unread_mark_diameter > 0 {
        let d = input.style.unread_mark_diameter;
        let rect = Rect::new(
            right - unread_size + (unread_size - d) / 2,
            top + (unread_size - d) / 2,
            d,
            d,
        );
        layout.unread_badge_rect = Some(rect);
        right -= unread_size + DIALOGS_UNREAD_PADDING;
        painted += 1;
    } else if b.unread {
        let w = unread_badge_width(input.unread_counter_text_width);
        layout.unread_badge_rect = Some(Rect::new(right - w, top, w, unread_size));
        right -= w + DIALOGS_UNREAD_PADDING;
        painted += 1;
    } else if !narrow && input.right_button_width > 0 && input.width >= COLUMN_MINIMAL_WIDTH_LEFT {
        let btn_w = input.right_button_width;
        layout.right_button_rect = Some(Rect::new(right - btn_w, top, btn_w, 20));
        return btn_w;
    } else if !narrow && input.display_pinned_icon {
        let w = DIALOGS_PINNED_ICON_SIZE;
        layout.pinned_icon_rect = Some(Rect::new(right - w, pinned_icon_top, w, w));
        right -= w + DIALOGS_UNREAD_PADDING;
    }

    if (!narrow || painted < 2) && (b.mention || b.reaction) {
        let icon_w = if narrow {
            unread_size
        } else {
            DIALOGS_ICON_BADGE_SIZE
        };
        let rect = Rect::new(right - icon_w, top, icon_w, icon_w);
        if b.mention {
            layout.mention_badge_rect = Some(rect);
        } else {
            layout.reaction_badge_rect = Some(rect);
        }
        right -= icon_w + DIALOGS_UNREAD_PADDING;
        painted += 1;
    }

    if (!narrow || painted < 2) && b.poll {
        let icon_w = if narrow {
            unread_size
        } else {
            DIALOGS_ICON_BADGE_SIZE
        };
        layout.poll_badge_rect = Some(Rect::new(right - icon_w, top, icon_w, icon_w));
        right -= icon_w + DIALOGS_UNREAD_PADDING;
    }

    initial_right - right
}

#[cfg(test)]
mod elide_tests {
    use super::{elide_text_right, elide_text_right_hinted};

    /// The previous implementation: one measurement per character from the start.
    fn elide_every_prefix(
        text: &str,
        max_width: i32,
        mut measure: impl FnMut(&str) -> i32,
    ) -> String {
        if max_width <= 0 || text.is_empty() {
            return String::new();
        }
        if measure(text) <= max_width {
            return text.to_string();
        }
        let ellipsis_w = measure("\u{2026}");
        if ellipsis_w > max_width {
            return String::new();
        }
        let mut best = 0usize;
        for (idx, ch) in text.char_indices() {
            let end = idx + ch.len_utf8();
            if measure(&text[..end]) + ellipsis_w <= max_width {
                best = end;
            } else {
                break;
            }
        }
        format!("{}\u{2026}", &text[..best])
    }

    /// A proportional "font": every character has its own width, wide scripts are wider.
    fn width(text: &str) -> i32 {
        text.chars()
            .map(|c| match c {
                'i' | 'l' | '.' | ',' | '\'' => 3,
                ' ' => 4,
                'm' | 'w' | 'M' | 'W' => 11,
                '\u{2026}' => 9,
                c if c.is_ascii() => 7,
                c if (c as u32) < 0x800 => 8,
                c if (c as u32) < 0x1_0000 => 13,
                _ => 16,
            })
            .sum()
    }

    const CORPUS: &[&str] = &[
        "",
        "a",
        "Hello Telegram World",
        "illlliiii.....,,,,'.'.'.'",
        "MMMMWWWWmmmmwwww wide letters first",
        "Planning sync: please review the agenda before the call on Monday morning",
        "Привет, как дела? Завтра созвон в десять утра по поводу релиза",
        "日本語のタイトルはとても長い場合があります、省略されるはずです",
        "mixed ascii и кириллица と 日本語 together in one row",
        "🙂🙃🙂🙃 emoji 🚀 in the middle 🎉 and at the end 🎈🎈🎈",
        "a\u{0301}e\u{0301}i\u{0301} combining marks o\u{0301}u\u{0301} everywhere",
        "x",
        "The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. \
         The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.",
    ];

    #[test]
    fn same_cut_as_measuring_every_prefix() {
        for text in CORPUS {
            // Every width for the short strings, a sample of widths for the long one.
            let step = if text.len() > 120 { 13 } else { 1 };
            for max_width in (-3..=(width(text) + 12)).step_by(step) {
                let expected = elide_every_prefix(text, max_width, width);
                assert_eq!(
                    elide_text_right(text, max_width, width),
                    expected,
                    "text {text:?} at {max_width}px"
                );
                // Any hint, also a wrong one or one inside a character, gives the same cut.
                for hint in [
                    0,
                    1,
                    text.len() / 3,
                    text.len() / 2,
                    text.len(),
                    text.len() + 7,
                ] {
                    assert_eq!(
                        elide_text_right_hinted(text, max_width, hint, width),
                        expected,
                        "text {text:?} at {max_width}px, hint {hint}"
                    );
                }
            }
        }
    }

    #[test]
    fn measures_a_handful_of_prefixes_not_every_one() {
        let text = CORPUS[12];
        let chars = text.chars().count();
        assert!(chars > 150);
        let mut worst = 0usize;
        for max_width in (20..width(text)).step_by(7) {
            let mut calls = 0usize;
            let _ = elide_text_right(text, max_width, |s| {
                calls += 1;
                width(s)
            });
            worst = worst.max(calls);
        }
        // Whole text, ellipsis, then a search that halves the range: far below one per character.
        assert!(worst <= 24, "{worst} measurements for {chars} characters");
    }

    #[test]
    fn a_correct_hint_costs_two_confirming_measurements() {
        let text = CORPUS[5];
        let max_width = 200;
        let expected = elide_every_prefix(text, max_width, width);
        let kept = expected.len() - "\u{2026}".len();
        let mut calls = 0usize;
        let out = elide_text_right_hinted(text, max_width, kept, |s| {
            calls += 1;
            width(s)
        });
        assert_eq!(out, expected);
        // Whole text, ellipsis, the hinted prefix, the prefix one character longer.
        assert_eq!(calls, 4);
    }
}
