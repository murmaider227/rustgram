//! Message element, service message, bottom info, reply header, date badge, unread bar,
//! and attach-to-previous/next grouping logic.
//!
//! Port of:
//! - `Telegram/SourceFiles/history/view/history_view_element.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_message.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_service_message.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_bottom_info.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_reply.cpp`

use crate::consts::{
    ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA, FontMetrics, HISTORY_COMMENTS_BUTTON_HEIGHT,
    HISTORY_COMMENTS_SKIP_LEFT, HISTORY_COMMENTS_SKIP_RIGHT, HISTORY_COMMENTS_SKIP_TEXT,
    HISTORY_COMMENTS_USERPIC_SHIFT, HISTORY_COMMENTS_USERPIC_SIZE, HISTORY_EPHEMERAL_STATE_WIDTH,
    HISTORY_FAST_SHARE_SIZE, HISTORY_PIN_WIDTH, HISTORY_PSA_ICON_SKIP1, HISTORY_REPLY_BOTTOM,
    HISTORY_REPLY_PADDING, HISTORY_REPLY_PREVIEW, HISTORY_REPLY_PREVIEW_MARGIN, HISTORY_REPLY_TOP,
    HISTORY_SEND_STATE_SPACE, HISTORY_SHORTCUT_STATE_SPACE, HISTORY_SILENT_WIDTH,
    HISTORY_UNREAD_BAR_HEIGHT, HISTORY_UNREAD_BAR_MARGIN, HISTORY_VIEWS_SPACE, HISTORY_VIEWS_WIDTH,
    LINE_WIDTH, MAX_RECENT_REPLIERS, MAX_SIGNATURE_SIZE, MEDIA_IN_BUBBLE_SKIP,
    MSG_BOT_KB_BUTTON_MARGIN, MSG_DATE_DELTA, MSG_DATE_IMG_DELTA, MSG_DATE_IMG_PADDING,
    MSG_DATE_SPACE, MSG_MARGIN, MSG_MARGIN_TOP_ATTACHED, MSG_MAX_WIDTH, MSG_MIN_WIDTH, MSG_PADDING,
    MSG_PHOTO_SKIP, MSG_SERVICE_MARGIN, MSG_SERVICE_PADDING, REACTION_INFO_BETWEEN,
    REACTION_INFO_SIZE, REACTION_INFO_SKIP, REPLY_NON_EXPANDED_LINES_LIMIT,
    TOPIC_BUTTON_ARROW_SKIP, TOPIC_BUTTON_PADDING, TOPIC_BUTTON_SKIP,
};
use crate::datetime::{CivilDate, LocalDateTime};
use crate::geometry::{BubbleCornerRounding, BubbleRounding, Rect, Size};
use crate::media::{MediaInput, MediaLayout};
use tg_data::{MsgId, PeerId, TimeId};

/// Chat layout mode affecting bubble alignment and max width (`HistoryView::ElementChatMode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ElementChatMode {
    /// Standard desktop chat (outgoing right-aligned, incoming left-aligned).
    #[default]
    Default,
    /// Wide/adaptive chat mode (all messages left-aligned within `WideChatWidth()`).
    Wide,
    /// Narrow column mode (smaller right margin: `msgMargin.left()` instead of `msgMargin.right()`).
    Narrow,
}

/// How the message's bottom timestamp / views / ticks info is displayed (`HistoryView::InfoDisplayType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InfoDisplayType {
    /// Inside the bottom-right of a text/document bubble.
    #[default]
    Default,
    /// Floating dark pill overlaid on the bottom-right of a photo/video/location.
    Image,
    /// Floating pill forced left-to-right.
    ImageLeftToRight,
    /// Floating service-styled pill outside an unwrapped sticker/round video.
    Background,
}

// ---------------------------------------------------------------------------
// Date separator & unread bar (history_view_element.cpp:764-847)
// ---------------------------------------------------------------------------

/// Height of a centered date separator badge (`DateBadge::height()`, `history_view_element.cpp:832-838`).
#[must_use]
pub const fn date_badge_height(fonts: &FontMetrics) -> i32 {
    MSG_SERVICE_MARGIN.top
        + MSG_SERVICE_PADDING.top
        + fonts.semibold.height
        + MSG_SERVICE_PADDING.bottom
        + MSG_SERVICE_MARGIN.bottom
}

/// Total height of the "Unread messages" bar (`UnreadBar::height()`, `history_view_element.cpp:769-771`).
#[must_use]
pub const fn unread_bar_height() -> i32 {
    HISTORY_UNREAD_BAR_HEIGHT + HISTORY_UNREAD_BAR_MARGIN
}

/// Top margin above the unread bar background (`UnreadBar::marginTop()`, `history_view_element.cpp:773-775`).
#[must_use]
pub const fn unread_bar_margin_top() -> i32 {
    LINE_WIDTH + HISTORY_UNREAD_BAR_MARGIN
}

/// Returns `true` if two local dates fall on the same calendar day.
#[must_use]
pub const fn is_same_day(a: CivilDate, b: CivilDate) -> bool {
    a.year == b.year && a.month == b.month && a.day == b.day
}

/// Computes whether `current` should display a date separator badge above it,
/// given the previous displayed item (`Element::recountDisplayDateInBlocks`,
/// `history_view_element.cpp:2684-2701`).
#[must_use]
pub fn should_display_date(previous: Option<&AttachMetadata>, current: &AttachMetadata) -> bool {
    if current.is_hidden || current.is_empty || current.is_sponsored || current.hide_display_date {
        return false;
    }
    match previous {
        None => true,
        Some(prev) => {
            prev.hide_display_date || !is_same_day(prev.date_time.date, current.date_time.date)
        }
    }
}

// ---------------------------------------------------------------------------
// Attach-to-previous / attach-to-next rules (history_view_element.cpp:2391-2475)
// ---------------------------------------------------------------------------

/// Metadata used to decide date separators and consecutive-message attachment
/// (`Element::computeIsAttachToPrevious` and `Element::recountDisplayDateInBlocks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachMetadata {
    /// Unix timestamp in seconds (`item->date()`).
    pub date: TimeId,
    /// Parsed local date/time used for calendar-day date separators.
    pub date_time: LocalDateTime,
    /// Sender peer ID (`item->from()`).
    pub from: PeerId,
    /// Effective display sender peer ID (`item->displayFrom()`, may differ in Saved Messages).
    pub display_from: Option<PeerId>,
    /// Chat peer is Saved Messages (`peer->isSelf()`), Replies, or VerifyCodes.
    pub is_saved_or_special_chat: bool,
    /// Chat peer is a megagroup (`peer->isMegagroup()`).
    pub is_megagroup: bool,
    /// Sender peer is a channel (`from->isChannel()`).
    pub from_is_channel: bool,
    /// Author is the megagroup itself (anonymous admin).
    pub author_is_megagroup: bool,
    /// Anonymous admin rank title (empty if not an anonymous admin with custom title).
    pub anonymous_rank: String,
    /// True if message has outgoing layout (`view->hasOutLayout()`).
    pub has_out_layout: bool,
    /// True if item is a service message (`item->isService()`).
    pub is_service: bool,
    /// True if item is empty (`item->isEmpty()`).
    pub is_empty: bool,
    /// True if item is hidden from display (e.g. non-leader album member).
    pub is_hidden: bool,
    /// True if item is a sponsored message (`item->isSponsored()`).
    pub is_sponsored: bool,
    /// True if channel post hides author (`item->isPostHidingAuthor()`).
    pub is_post_hiding_author: bool,
    /// True if guest chat bot message (`item->isGuestChatBotMessage()`).
    pub is_guest_chat_bot: bool,
    /// True if date badge is suppressed for this item (`item->hideDisplayDate()`).
    pub hide_display_date: bool,
    /// True if item has a visible inline reply keyboard (`previousMarkup && !hiddenBy(media)`).
    pub has_visible_inline_markup: bool,
    /// Forum topic root message ID (`0` if not in a forum).
    pub topic_root_id: MsgId,
    /// True if ephemeral (self-destruct / bot business ephemeral).
    pub is_ephemeral: bool,
    /// True if this item is forwarded.
    pub is_forwarded: bool,
    /// True if this item is an imported forwarded message.
    pub is_imported_forward: bool,
    /// Hidden forwarded sender name/key when `display_from` is `None`.
    pub hidden_sender_key: Option<String>,
    /// True if rendered unwrapped (stickers, unwrapped round videos) — affects bubble attachment.
    pub unwrapped: bool,
    /// True when rendered in WelcomeMessages context (ignores the 900s gap).
    pub ignores_date_gap: bool,
}

impl Default for AttachMetadata {
    fn default() -> Self {
        Self {
            date: 0,
            date_time: LocalDateTime::from_unix_utc(0),
            from: PeerId(0),
            display_from: None,
            is_saved_or_special_chat: false,
            is_megagroup: false,
            from_is_channel: false,
            author_is_megagroup: false,
            anonymous_rank: String::new(),
            has_out_layout: false,
            is_service: false,
            is_empty: false,
            is_hidden: false,
            is_sponsored: false,
            is_post_hiding_author: false,
            is_guest_chat_bot: false,
            hide_display_date: false,
            has_visible_inline_markup: false,
            topic_root_id: MsgId(0),
            is_ephemeral: false,
            is_forwarded: false,
            is_imported_forward: false,
            hidden_sender_key: None,
            unwrapped: false,
            ignores_date_gap: false,
        }
    }
}

fn may_be_attached(meta: &AttachMetadata) -> bool {
    !meta.is_service
        && !meta.is_empty
        && !meta.is_sponsored
        && !meta.is_post_hiding_author
        && !meta.is_guest_chat_bot
        && (!meta.is_megagroup || !meta.has_out_layout || !meta.from_is_channel)
}

fn is_attached_to_previous_in_saved_messages(prev: &AttachMetadata, cur: &AttachMetadata) -> bool {
    if prev.is_forwarded != cur.is_forwarded {
        return false;
    }
    if prev.display_from != cur.display_from {
        return false;
    }
    if !prev.is_forwarded || prev.display_from.is_some() {
        return true;
    }
    prev.hidden_sender_key.is_some() && prev.hidden_sender_key == cur.hidden_sender_key
}

/// Computes whether `current` attaches visually to `previous`
/// (`Element::computeIsAttachToPrevious` in `history_view_element.cpp:2391-2475`).
///
/// `has_top_separator` must be `true` if `current` has a `DateBadge`, `UnreadBar`,
/// `ServicePreMessage`, or `ForumThreadBar` above it.
#[must_use]
pub fn compute_is_attach_to_previous(
    previous: &AttachMetadata,
    current: &AttachMetadata,
    has_top_separator: bool,
) -> bool {
    if has_top_separator {
        return false;
    }
    let delta = (i64::from(previous.date) - i64::from(current.date)).abs();
    let possible = (current.ignores_date_gap
        || delta < i64::from(ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA))
        && (current.is_ephemeral == previous.is_ephemeral)
        && may_be_attached(current)
        && may_be_attached(previous)
        && !previous.has_visible_inline_markup
        && (current.topic_root_id == previous.topic_root_id);

    if !possible {
        return false;
    }

    if current.is_saved_or_special_chat
        || current.is_imported_forward
        || previous.is_imported_forward
    {
        return is_attached_to_previous_in_saved_messages(previous, current);
    }
    if previous.from != current.from {
        return false;
    }
    if !current.author_is_megagroup {
        return true;
    }
    current.anonymous_rank == previous.anonymous_rank
}

/// Computed attachment & date-separator state for one item in a sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SequenceAttachState {
    /// True if a date separator badge is shown above this item.
    pub display_date: bool,
    /// True if attached to the immediately preceding displayed item.
    pub attached_to_previous: bool,
    /// True if attached to the immediately following displayed item.
    pub attached_to_next: bool,
    /// True if attached to previous AND previous has a bubble (`!previous.unwrapped`).
    pub bubble_attached_to_previous: bool,
    /// True if attached to next AND next has a bubble (`!next.unwrapped`).
    pub bubble_attached_to_next: bool,
}

/// Recalculates date separators and attachment flags for an ordered slice of messages.
///
/// `unread_bar_index`, if `Some(i)`, marks item `i` as carrying the `UnreadBar` above it
/// (preventing attachment to item `i - 1`).
#[must_use]
pub fn compute_sequence_attach_states(
    items: &[AttachMetadata],
    unread_bar_index: Option<usize>,
) -> Vec<SequenceAttachState> {
    let mut states = vec![SequenceAttachState::default(); items.len()];
    let mut prev_displayed: Option<usize> = None;

    for (i, meta) in items.iter().enumerate() {
        if meta.is_hidden || meta.is_empty {
            continue;
        }
        let prev_meta = prev_displayed.map(|idx| &items[idx]);
        let display_date = should_display_date(prev_meta, meta);
        states[i].display_date = display_date;

        if let Some(prev_idx) = prev_displayed {
            let has_sep = display_date || unread_bar_index == Some(i);
            let attached = compute_is_attach_to_previous(&items[prev_idx], meta, has_sep);
            states[i].attached_to_previous = attached;
            states[i].bubble_attached_to_previous = attached && !items[prev_idx].unwrapped;
            states[prev_idx].attached_to_next = attached;
            states[prev_idx].bubble_attached_to_next = attached && !meta.unwrapped;
        }

        prev_displayed = Some(i);
    }

    states
}

// ---------------------------------------------------------------------------
// BottomInfo (date/time, edited, signature, views, replies, ticks, effect)
// Port of history/view/history_view_bottom_info.cpp
// ---------------------------------------------------------------------------

/// Input parameters for computing a message's bottom info bar (`HistoryView::BottomInfo::Data`).
#[derive(Debug, Clone, Default)]
pub struct BottomInfoInput {
    /// Width of the formatted date + edited + signed-author string (after author elision).
    pub author_edited_date_width: i32,
    /// Width of signed author name before elision (`0` if unsigned).
    pub author_width: i32,
    /// Width of the `, <edited> <time>` suffix after the author name.
    pub after_author_width: i32,
    /// Width of formatted views count string (`0` if no views counter).
    pub views_text_width: i32,
    /// Width of formatted replies count string (`0` if no replies counter).
    pub replies_text_width: i32,
    /// True if outgoing layout (reserves `HISTORY_SEND_STATE_SPACE = 24px`).
    pub out_layout: bool,
    /// True if currently sending or failed (reserves `HISTORY_SEND_STATE_SPACE`).
    pub sending: bool,
    /// True if pinned icon is shown in bottom info (`+24px`).
    pub pinned: bool,
    /// True if silent scheduled icon is shown (`+12px`).
    pub silent: bool,
    /// True if ephemeral timer icon is shown (`+16px`).
    pub ephemeral: bool,
    /// True if a message effect icon is displayed (`+15 + 3 = 18px`).
    pub has_effect: bool,
    /// True if this is a sponsored message (date line height = 0).
    pub sponsored: bool,
    /// True if this is a business quick-reply shortcut (`width = 18px`).
    pub shortcut: bool,
}

/// Computed layout for a message's bottom info bar (`HistoryView::BottomInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BottomInfoLayout {
    /// Optimal single-line size (`countOptimalSize`).
    pub optimal_size: Size,
    /// Current size at the allocated width (`countCurrentSize`).
    pub current_size: Size,
    /// Width occupied by the effect icon (`_effectMaxWidth`).
    pub effect_max_width: i32,
    /// Width of the first (date/views/ticks) line (`firstLineWidth`).
    pub first_line_width: i32,
    /// True if the signed author exceeded `MAX_SIGNATURE_SIZE` and had to be elided.
    pub author_elided: bool,
}

impl BottomInfoLayout {
    /// Computes optimal and current layout for bottom info (`history_view_bottom_info.cpp:468-627`).
    #[must_use]
    pub fn compute(input: &BottomInfoInput, available_width: i32, fonts: &FontMetrics) -> Self {
        let author_elided = input.author_width > 0
            && (input.author_width + input.after_author_width > MAX_SIGNATURE_SIZE);

        if input.shortcut {
            let size = Size::new(HISTORY_SHORTCUT_STATE_SPACE, fonts.date.height);
            return Self {
                optimal_size: size,
                current_size: size,
                effect_max_width: 0,
                first_line_width: size.width,
                author_elided,
            };
        }

        let mut width = 0;
        if input.out_layout || input.sending {
            width += HISTORY_SEND_STATE_SPACE;
        }
        let date_part_width = if author_elided {
            MAX_SIGNATURE_SIZE
        } else if input.author_edited_date_width > 0 {
            input.author_edited_date_width
        } else {
            input.author_width + input.after_author_width
        };
        width += date_part_width;

        if input.views_text_width > 0 && !input.sending {
            width += HISTORY_VIEWS_SPACE + input.views_text_width + HISTORY_VIEWS_WIDTH;
        }
        if input.replies_text_width > 0 && !input.sending {
            width += HISTORY_VIEWS_SPACE + input.replies_text_width + HISTORY_VIEWS_WIDTH;
        }
        if input.pinned {
            width += HISTORY_PIN_WIDTH;
        }
        if input.silent {
            width += HISTORY_SILENT_WIDTH;
        }
        if input.ephemeral {
            width += HISTORY_EPHEMERAL_STATE_WIDTH;
        }

        let effect_max_width = if input.has_effect {
            REACTION_INFO_SIZE
                + REACTION_INFO_BETWEEN
                + (REACTION_INFO_SKIP - REACTION_INFO_BETWEEN)
        } else {
            0
        };
        width += effect_max_width;

        let date_height = if input.sponsored {
            0
        } else {
            fonts.date.height
        };
        let optimal_size = Size::new(width, date_height);

        let cur_size = if available_width >= optimal_size.width {
            optimal_size
        } else {
            let no_reactions_width = optimal_size.width - effect_max_width;
            let clamped_w = available_width.min(no_reactions_width.max(effect_max_width));
            let effect_h = if input.has_effect && clamped_w > 0 {
                fonts.date.height
            } else {
                0
            };
            Size::new(clamped_w, date_height + effect_h)
        };

        let first_line_width = if cur_size.height == optimal_size.height {
            cur_size.width
        } else {
            optimal_size.width - effect_max_width
        };

        Self {
            optimal_size,
            current_size: cur_size,
            effect_max_width,
            first_line_width,
            author_elided,
        }
    }
}

// ---------------------------------------------------------------------------
// Reply header (history_view_reply.cpp:746-810)
// ---------------------------------------------------------------------------

/// Input parameters for sizing an in-bubble reply header (`HistoryView::Reply`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplyInput {
    /// Width of the replied-to author name.
    pub name_width: i32,
    /// Width of the replied-to message/quote text (single line).
    pub text_width: i32,
    /// Height of the replied-to text when wrapped to the available reply width.
    pub text_wrapped_height: i32,
    /// True if the reply includes a 32x32 media preview thumbnail.
    pub has_preview: bool,
    /// True if replying to a quote (allows multiline text up to 5 lines).
    pub multiline: bool,
    /// True if a long quote has been expanded beyond 5 lines.
    pub expanded: bool,
    /// Extra icon width on the name row (quote icon / background gift icon).
    pub name_icons_skip: i32,
    /// Outline bar thickness (`st::messageQuoteStyle.outline`, default 3px).
    pub outline_width: i32,
}

/// Computed layout for a reply header (`HistoryView::Reply`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyLayout {
    /// Optimal width of the reply box.
    pub max_width: i32,
    /// Inner box height (without `HISTORY_REPLY_TOP` / `HISTORY_REPLY_BOTTOM`).
    pub inner_height: i32,
    /// Total height consumed inside the bubble (`inner_height + 4px`).
    pub total_height: i32,
    /// Horizontal offset reserved for the 32x32 preview thumbnail (`0` if hidden/absent).
    pub preview_skip: i32,
}

impl ReplyLayout {
    /// Computes reply header dimensions at `available_width`
    /// (`Reply::resizeToWidth` in `history_view_reply.cpp:746-810`).
    #[must_use]
    pub fn compute(input: &ReplyInput, available_width: i32, fonts: &FontMetrics) -> Self {
        let outline = if input.outline_width > 0 {
            input.outline_width
        } else {
            3
        };
        let preview_skip = if input.has_preview {
            outline
                + HISTORY_REPLY_PREVIEW_MARGIN.left
                + HISTORY_REPLY_PREVIEW
                + HISTORY_REPLY_PREVIEW_MARGIN.right
                - HISTORY_REPLY_PADDING.left
        } else {
            0
        };
        let name_row_w = preview_skip + input.name_width + input.name_icons_skip;
        let text_row_w = preview_skip + input.text_width;
        let max_width = HISTORY_REPLY_PADDING.horizontal() + name_row_w.max(text_row_w);

        let inner_w = (available_width - HISTORY_REPLY_PADDING.horizontal()).max(0);
        let name_two_lines = input.multiline
            && available_width < max_width
            && (input.name_width > (inner_w - preview_skip - input.name_icons_skip));
        let name_h = (if name_two_lines { 2 } else { 1 }) * fonts.semibold.height;

        let raw_text_h = if input.multiline {
            let max_h = if input.expanded {
                i32::MAX
            } else {
                REPLY_NON_EXPANDED_LINES_LIMIT * fonts.normal.height
            };
            input
                .text_wrapped_height
                .max(fonts.normal.height)
                .min(max_h)
        } else {
            fonts.normal.height
        };
        let mut inner_height =
            HISTORY_REPLY_PADDING.top + name_h + raw_text_h + HISTORY_REPLY_PADDING.bottom;
        if input.has_preview {
            let min_preview_h = HISTORY_REPLY_PREVIEW + HISTORY_REPLY_PREVIEW_MARGIN.vertical();
            inner_height = inner_height.max(min_preview_h);
        }
        let total_height = inner_height + HISTORY_REPLY_TOP + HISTORY_REPLY_BOTTOM;
        Self {
            max_width,
            inner_height,
            total_height,
            preview_skip,
        }
    }
}

// ---------------------------------------------------------------------------
// Message layout (history_view_message.cpp)
// ---------------------------------------------------------------------------

/// Input description of a regular (non-service) chat message for pure layout computation.
#[derive(Debug, Clone, Default)]
pub struct MessageInput {
    /// Outgoing message flag (`item->out()`).
    pub is_out: bool,
    /// Channel broadcast post (`item->isPost()` — never right-aligned even when `is_out`).
    pub is_post: bool,
    /// Hidden item (e.g. non-leader item in an album).
    pub is_hidden: bool,
    /// Empty item.
    pub is_empty: bool,
    /// Centered special view (bot about / comments root).
    pub centered_view: bool,
    /// True if the sender avatar column is reserved on the left (`hasFromPhoto()`).
    pub has_from_photo: bool,
    /// True if a right-side fast-share / forward action button is shown.
    pub has_right_action: bool,
    /// Chat layout mode (`Default`, `Wide`, `Narrow`).
    pub chat_mode: ElementChatMode,
    /// Attachment flags computed by [`compute_sequence_attach_states`].
    pub attach: SequenceAttachState,
    /// True if an `UnreadBar` sits above this message.
    pub has_unread_bar: bool,
    /// Optional forum thread bar height above this message (`0` if none).
    pub forum_thread_bar_height: i32,
    /// Optional pre/post service message height (`(height, below)`).
    pub service_pre_message: Option<(i32, bool)>,
    /// Extra top/bottom added margins.
    pub added_margin_top: i32,
    pub added_margin_bottom: i32,

    /// Sender name header width (`None` if not displaying from-name).
    pub from_name_width: Option<i32>,
    /// Right badge width on the name row (admin title / tag), `0` if none.
    pub right_badge_width: i32,
    /// Via-bot header width (`None` if not sent via inline bot).
    pub via_bot_width: Option<i32>,
    /// Topic button name width (`None` if hidden).
    pub topic_button_name_width: Option<i32>,
    /// Forwarded header text width (`None` if not forwarded).
    pub forwarded_width: Option<i32>,
    /// True if forwarded header is a PSA (adds `HISTORY_PSA_ICON_SKIP1`).
    pub forwarded_is_psa: bool,
    /// Optional reply header.
    pub reply: Option<ReplyInput>,
    /// Optional summary header height & width `(width, height)`.
    pub summary_header: Option<(i32, i32)>,

    /// Message text single-line max width (`0` if no visible text).
    pub text_max_width: i32,
    /// Message text natural single-line min height (`0` if no visible text).
    pub text_min_height: i32,
    /// Message text wrapped height at the bubble's inner text width.
    pub text_wrapped_height: i32,
    /// Actual longest wrapped line width (`textRealWidth()`), `0` to skip shrinking.
    pub text_real_width: i32,
    /// Monospace code-block max width override (`0` if none).
    pub monospace_max_width: i32,

    /// Optional attached media.
    pub media: Option<MediaInput>,
    /// True if media is placed above caption text (`_invertMedia`).
    pub invert_media: bool,

    /// Bottom info bar (date/time, edited, views, ticks, effect).
    pub bottom_info: BottomInfoInput,
    /// True if bottom info is hidden (e.g. certain service-like states).
    pub hides_bottom_info: bool,

    /// Reactions bar optimal size (`Size::ZERO` if no reactions).
    pub reactions_size: Size,
    /// True if reactions are embedded inside the bubble rather than below it.
    pub reactions_in_bubble: bool,

    /// True if a channel comments button bar is shown at the bottom of the bubble.
    pub has_comments_button: bool,
    /// Comments button text width (used in `minWidthForMedia`).
    pub comments_text_width: i32,
    /// Optional inline keyboard `(natural_width, natural_height)`.
    pub inline_keyboard: Option<Size>,
}

/// Complete computed layout of a message element (`HistoryView::Message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLayout {
    /// Whether the message draws a bubble background.
    pub has_bubble: bool,
    /// Whether the message is unwrapped (no bubble background, e.g. sticker).
    pub unwrapped: bool,
    /// Whether the message uses outgoing styling (`out && !post`).
    pub has_out_layout: bool,
    /// Whether the message is right-aligned (`has_out_layout && chat_mode != Wide`).
    pub has_right_layout: bool,
    /// Optimal size `(max_width, min_height)` (`performCountOptimalSize`).
    pub optimal_size: Size,
    /// Total element size `(viewport_width, total_height)` (`performCountCurrentSize`).
    pub current_size: Size,
    /// Top margin including date badge / unread bar / attached-vs-detached spacing.
    pub margin_top: i32,
    /// Bottom margin.
    pub margin_bottom: i32,
    /// Outer bubble / content rectangle in element coordinates (`countGeometry()`).
    pub geometry: Rect,
    /// Corner rounding for the message content (`countMessageRounding`).
    pub message_rounding: BubbleRounding,
    /// Corner rounding for the bubble shell (small bottom corners when inline keyboard present).
    pub bubble_rounding: BubbleRounding,
    /// Height of each header section inside the bubble.
    pub header_heights: MessageHeaderHeights,
    /// Text content rectangle inside the bubble (empty if no visible text).
    pub text_rect: Rect,
    /// Media rectangle in element coordinates (empty if no media).
    pub media_rect: Rect,
    /// Bottom info (date/views/ticks) placement rectangle and display style.
    pub bottom_info_rect: Rect,
    /// Display mode of the bottom info pill/text.
    pub bottom_info_display: InfoDisplayType,
    /// Detailed bottom info sizing.
    pub bottom_info: BottomInfoLayout,
    /// Reactions bar rectangle (empty if no reactions).
    pub reactions_rect: Rect,
    /// Inline keyboard rectangle below the bubble (empty if no keyboard).
    pub keyboard_rect: Rect,
    /// Comments button rectangle at the bottom of the bubble (empty if absent).
    pub comments_rect: Rect,
    /// Computed media layout details, if media is present.
    pub media_layout: Option<MediaLayout>,
}

/// Breakdown of header heights stacked at the top of a message bubble.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageHeaderHeights {
    /// Sender name or standalone via-bot height (`msgNameFont.height`).
    pub from_name_or_via: i32,
    /// Topic chip button height.
    pub topic_button: i32,
    /// Forwarded-from header height (1 or 2 lines of `semiboldFont.height`).
    pub forwarded: i32,
    /// Reply preview header height.
    pub reply: i32,
    /// Summary header height.
    pub summary: i32,
}

impl MessageHeaderHeights {
    /// Sum of all header heights.
    #[must_use]
    pub const fn total(self) -> i32 {
        self.from_name_or_via + self.topic_button + self.forwarded + self.reply + self.summary
    }
}

/// Computes bubble corner rounding (`Message::countMessageRounding`,
/// `history_view_message.cpp:6641-6677`).
#[must_use]
pub const fn count_message_rounding(
    bubble_attached_to_previous: bool,
    bubble_attached_to_next: bool,
    has_right_layout: bool,
    skip_tail: bool,
) -> BubbleRounding {
    use BubbleCornerRounding as C;
    let small_top = bubble_attached_to_previous;
    let small_bottom = bubble_attached_to_next;
    let effective_skip_tail = small_bottom || skip_tail;

    BubbleRounding {
        top_left: if small_top && !has_right_layout {
            C::Small
        } else {
            C::Large
        },
        top_right: if small_top && has_right_layout {
            C::Small
        } else {
            C::Large
        },
        bottom_left: if small_bottom && !has_right_layout {
            C::Small
        } else if !effective_skip_tail && !has_right_layout {
            C::Tail
        } else {
            C::Large
        },
        bottom_right: if small_bottom && has_right_layout {
            C::Small
        } else if !effective_skip_tail && has_right_layout {
            C::Tail
        } else {
            C::Large
        },
    }
}

/// Adjusts bubble bottom corners to `Small` when an inline reply keyboard sits underneath
/// (`Message::countBubbleRounding`, `history_view_message.cpp:6679-6687`).
#[must_use]
pub const fn count_bubble_rounding(
    mut message_rounding: BubbleRounding,
    has_inline_keyboard: bool,
) -> BubbleRounding {
    if has_inline_keyboard {
        message_rounding.bottom_left = BubbleCornerRounding::Small;
        message_rounding.bottom_right = BubbleCornerRounding::Small;
    }
    message_rounding
}

/// Computes `Message::minWidthForMedia()` (`history_view_message.cpp:5948-5975`).
#[must_use]
pub fn min_width_for_media(
    hides_bottom_info: bool,
    info_width: i32,
    has_comments_button: bool,
    comments_text_width: i32,
) -> i32 {
    let mut result = if hides_bottom_info {
        0
    } else {
        info_width + 2 * (MSG_DATE_IMG_DELTA + MSG_DATE_IMG_PADDING.x)
    };
    if has_comments_button && comments_text_width > 0 {
        let single = HISTORY_COMMENTS_USERPIC_SIZE;
        let shift = HISTORY_COMMENTS_USERPIC_SHIFT;
        let open_icon_w = 16; // st::historyCommentsOpenOutSelected.width()
        let added = single
            + (MAX_RECENT_REPLIERS - 1) * (single - shift)
            + HISTORY_COMMENTS_SKIP_LEFT
            + HISTORY_COMMENTS_SKIP_RIGHT
            + HISTORY_COMMENTS_SKIP_TEXT
            + open_icon_w
            + HISTORY_COMMENTS_SKIP_RIGHT
            + comments_text_width;
        result = result.max(added);
    }
    result
}

/// Computes the complete layout of a chat message at `viewport_width`
/// (`Message::performCountOptimalSize`, `Message::resizeContentGetHeight`, `Message::countGeometry`).
#[must_use]
pub fn layout_message(
    input: &MessageInput,
    viewport_width: i32,
    fonts: &FontMetrics,
) -> MessageLayout {
    let has_out_layout = input.is_out && !input.is_post;
    let has_right_layout = has_out_layout && !matches!(input.chat_mode, ElementChatMode::Wide);

    let margin_top = {
        let mut r = if input.is_hidden {
            0
        } else if input.attach.attached_to_previous {
            MSG_MARGIN_TOP_ATTACHED
        } else {
            MSG_MARGIN.top
        };
        if input.attach.display_date {
            r += date_badge_height(fonts);
        }
        if input.has_unread_bar {
            r += unread_bar_height();
        }
        r += input.forum_thread_bar_height;
        if let Some((h, below)) = input.service_pre_message
            && !below
        {
            r += h;
        }
        r += input.added_margin_top;
        r
    };

    let margin_bottom = if input.is_hidden {
        0
    } else {
        let mut r = MSG_MARGIN.bottom;
        if let Some((h, below)) = input.service_pre_message
            && below
        {
            r += h;
        }
        r + input.added_margin_bottom
    };

    if input.is_hidden {
        return MessageLayout {
            has_bubble: false,
            unwrapped: true,
            has_out_layout,
            has_right_layout,
            optimal_size: Size::ZERO,
            current_size: Size::new(viewport_width, margin_top + margin_bottom),
            margin_top,
            margin_bottom,
            geometry: Rect::ZERO,
            message_rounding: BubbleRounding::NONE,
            bubble_rounding: BubbleRounding::NONE,
            header_heights: MessageHeaderHeights::default(),
            text_rect: Rect::ZERO,
            media_rect: Rect::ZERO,
            bottom_info_rect: Rect::ZERO,
            bottom_info_display: InfoDisplayType::Default,
            bottom_info: BottomInfoLayout::default(),
            reactions_rect: Rect::ZERO,
            keyboard_rect: Rect::ZERO,
            comments_rect: Rect::ZERO,
            media_layout: None,
        };
    }

    let has_visible_text = input.text_max_width > 0 || input.text_min_height > 0;
    let has_header = input.from_name_width.is_some()
        || input.via_bot_width.is_some()
        || input.topic_button_name_width.is_some()
        || input.forwarded_width.is_some()
        || input.reply.is_some()
        || input.summary_header.is_some();

    let bottom_info_opt = BottomInfoLayout::compute(&input.bottom_info, i32::MAX, fonts);
    let min_w_media = min_width_for_media(
        input.hides_bottom_info,
        bottom_info_opt.optimal_size.width,
        input.has_comments_button,
        input.comments_text_width,
    );

    let probe_media = input
        .media
        .as_ref()
        .map(|m| m.layout(MSG_MAX_WIDTH, has_header, has_visible_text, fonts));

    let draw_bubble = match &probe_media {
        Some(ml) => has_visible_text || ml.needs_bubble || has_header,
        None => !input.is_empty,
    };
    let unwrapped = match &probe_media {
        Some(ml) => !has_visible_text && ml.unwrapped && !has_header,
        None => input.is_empty,
    };

    let has_reactions = !input.reactions_size.is_empty();
    let reactions_in_bubble = has_reactions && input.reactions_in_bubble && draw_bubble;

    // 1. Optimal size (performCountOptimalSize, history_view_message.cpp:1514-1733).
    let textual_max_width = if has_visible_text {
        let info_skip = if !input.hides_bottom_info && !has_reactions && probe_media.is_none() {
            MSG_DATE_SPACE + bottom_info_opt.optimal_size.width
        } else {
            0
        };
        MSG_PADDING.left + input.text_max_width + info_skip + MSG_PADDING.right
    } else {
        MSG_PADDING.horizontal()
    };

    let mut max_width: i32;
    let mut non_text_max = 0;
    let mut min_height = 0;

    if draw_bubble {
        max_width = textual_max_width;
        if has_visible_text {
            min_height = input.text_min_height.max(fonts.normal.height);
        }
        if reactions_in_bubble {
            let reactions_max_w = MSG_PADDING.horizontal() + input.reactions_size.width;
            let limited = MSG_MAX_WIDTH.min(reactions_max_w);
            max_width = max_width.max(limited);
            non_text_max = non_text_max.max(limited);
            min_height += MEDIA_IN_BUBBLE_SKIP + input.reactions_size.height;
        }
        let media_on_bottom = probe_media.as_ref().is_some_and(|m| m.is_bubble_bottom);
        let media_on_top = probe_media.as_ref().is_some_and(|m| m.is_bubble_top);
        if !media_on_bottom {
            min_height += MSG_PADDING.bottom;
            if probe_media.is_some() {
                min_height += MEDIA_IN_BUBBLE_SKIP;
            }
        }
        if !media_on_top {
            min_height += MSG_PADDING.top;
            if probe_media.is_some() {
                min_height += MEDIA_IN_BUBBLE_SKIP;
            }
        }

        if let Some(ref ml) = probe_media {
            if ml.enforce_bubble_width {
                max_width = ml.optimal_size.width;
            } else {
                max_width = max_width.max(ml.optimal_size.width);
            }
            min_height += ml.optimal_size.height;
        } else {
            if let Some(name_w) = input.from_name_width {
                let mut w = MSG_PADDING.left + name_w + MSG_PADDING.right;
                if let Some(via_w) = input.via_bot_width
                    && input.forwarded_width.is_none()
                {
                    w += fonts.semibold.space_width + via_w;
                }
                if input.right_badge_width > 0 {
                    w += MSG_PADDING.right + input.right_badge_width;
                }
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            } else if let Some(via_w) = input.via_bot_width
                && input.forwarded_width.is_none()
            {
                let w = MSG_PADDING.horizontal() + via_w;
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            }
            if let Some(topic_w) = input.topic_button_name_width {
                let pad = MSG_PADDING + TOPIC_BUTTON_PADDING;
                let w = pad.horizontal() + topic_w + TOPIC_BUTTON_ARROW_SKIP;
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            }
            if let Some(fwd_w) = input.forwarded_width {
                let skip1 = if input.forwarded_is_psa {
                    HISTORY_PSA_ICON_SKIP1
                } else {
                    0
                };
                let mut w = MSG_PADDING.horizontal() + fwd_w + skip1;
                if let Some(via_w) = input.via_bot_width {
                    w += fonts.semibold.space_width + via_w;
                }
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            }
            if let Some(ref reply) = input.reply {
                let rl = ReplyLayout::compute(reply, MSG_MAX_WIDTH, fonts);
                let w = MSG_PADDING.horizontal() + rl.max_width;
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            }
            if let Some((sw, _)) = input.summary_header {
                let w = MSG_PADDING.horizontal() + sw;
                max_width = max_width.max(w);
                non_text_max = non_text_max.max(w);
            }
        }
        max_width = max_width.max(min_w_media);
        non_text_max = non_text_max.max(min_w_media);
    } else if let Some(ref ml) = probe_media {
        max_width = ml.optimal_size.width;
        min_height = ml.optimal_size.height;
    } else {
        max_width = MSG_MIN_WIDTH;
        min_height = 0;
    }

    if let Some(kb) = input.inline_keyboard
        && has_visible_text
    {
        max_width = max_width.max(kb.width);
        if draw_bubble {
            non_text_max = non_text_max.max(kb.width);
        }
    }

    let optimal_size = Size::new(max_width, min_height);

    // 2. Current width & content geometry (resizeContentGetHeight / countGeometry,
    //    history_view_message.cpp:6572-6639, 6722-6967).
    let use_more_space = matches!(input.chat_mode, ElementChatMode::Narrow);
    let wide_skip = if use_more_space {
        MSG_MARGIN.left
    } else {
        MSG_MARGIN.right
    };
    let available_width = (viewport_width
        - MSG_MARGIN.left
        - if input.centered_view {
            MSG_MARGIN.left
        } else {
            wide_skip
        })
    .max(1);

    let mut content_left = if has_right_layout {
        wide_skip
    } else {
        MSG_MARGIN.left
    };
    let mut content_width = available_width;
    if input.has_from_photo {
        content_left += MSG_PHOTO_SKIP;
        if input.has_right_action {
            content_width -= HISTORY_FAST_SHARE_SIZE + (MSG_PHOTO_SKIP - HISTORY_FAST_SHARE_SIZE);
        }
    }

    let bubble_width_limit = MSG_MAX_WIDTH.max(input.monospace_max_width);
    content_width = content_width.min(max_width).min(bubble_width_limit).max(1);

    let final_media = input
        .media
        .as_ref()
        .map(|m| m.layout(content_width, has_header, has_visible_text, fonts));

    if let Some(ref ml) = final_media {
        if ml.current_size.width < content_width {
            if ml.current_size.width < textual_max_width && !ml.enforce_bubble_width {
                content_width = content_width.min(textual_max_width);
            } else {
                content_width = ml.current_size.width;
            }
        }
    } else if draw_bubble && has_visible_text && input.text_real_width > 0 {
        let shrunk = (input.text_real_width + MSG_PADDING.horizontal()).max(non_text_max);
        content_width = content_width.min(shrunk);
    }

    if content_width < available_width && !matches!(input.chat_mode, ElementChatMode::Wide) {
        if has_out_layout {
            content_left += available_width - content_width;
        } else if input.centered_view {
            content_left += (available_width - content_width) / 2;
        }
    } else if content_width < available_width && input.centered_view {
        content_left += ((MSG_MAX_WIDTH + 2 * MSG_PHOTO_SKIP - content_width) / 2).max(0);
    }

    // 3. Bottom info resize & vertical stack inside bubble.
    let inner_width = (content_width - MSG_PADDING.horizontal()).max(1);
    let bottom_info = BottomInfoLayout::compute(
        &input.bottom_info,
        (inner_width - 2 * MSG_DATE_DELTA.x).max(1),
        fonts,
    );

    let mut headers = MessageHeaderHeights::default();
    let mut bubble_height = 0;
    let mut text_rect = Rect::ZERO;
    let mut media_rect = Rect::ZERO;
    let mut reactions_rect = Rect::ZERO;
    let mut comments_rect = Rect::ZERO;

    if draw_bubble {
        let media_on_top = final_media.as_ref().is_some_and(|m| m.is_bubble_top);
        let media_on_bottom = final_media.as_ref().is_some_and(|m| m.is_bubble_bottom);

        if input.from_name_width.is_some()
            || (input.via_bot_width.is_some() && input.forwarded_width.is_none())
        {
            headers.from_name_or_via = fonts.semibold.height;
        }
        if input.topic_button_name_width.is_some() {
            headers.topic_button = TOPIC_BUTTON_SKIP
                + TOPIC_BUTTON_PADDING.top
                + fonts.semibold.height
                + TOPIC_BUTTON_PADDING.bottom
                + TOPIC_BUTTON_SKIP;
        }
        if let Some(fwd_w) = input.forwarded_width {
            let skip1 = if input.forwarded_is_psa {
                HISTORY_PSA_ICON_SKIP1
            } else {
                0
            };
            let lines = if fwd_w > (inner_width - skip1) { 2 } else { 1 };
            headers.forwarded = lines * fonts.semibold.height;
        }
        if let Some(ref reply) = input.reply {
            headers.reply = ReplyLayout::compute(reply, inner_width, fonts).total_height;
        }
        if let Some((_, sh)) = input.summary_header {
            headers.summary = sh;
        }

        let mut y = margin_top;
        if !media_on_top {
            y += MSG_PADDING.top;
            y += headers.total();
        }

        let text_h = if has_visible_text {
            if input.text_wrapped_height > 0 {
                input.text_wrapped_height
            } else {
                input.text_min_height.max(fonts.normal.height)
            }
        } else {
            0
        };

        if let Some(ref ml) = final_media {
            if input.invert_media {
                if !media_on_top {
                    y += MEDIA_IN_BUBBLE_SKIP;
                }
                media_rect = Rect::new(
                    content_left,
                    y,
                    ml.current_size.width,
                    ml.current_size.height,
                );
                y += ml.current_size.height;
                if has_visible_text {
                    y += MEDIA_IN_BUBBLE_SKIP;
                    text_rect = Rect::new(content_left + MSG_PADDING.left, y, inner_width, text_h);
                    y += text_h;
                }
            } else {
                if has_visible_text {
                    text_rect = Rect::new(content_left + MSG_PADDING.left, y, inner_width, text_h);
                    y += text_h;
                }
                if !media_on_top {
                    y += MEDIA_IN_BUBBLE_SKIP;
                }
                media_rect = Rect::new(
                    content_left,
                    y,
                    ml.current_size.width,
                    ml.current_size.height,
                );
                y += ml.current_size.height;
            }
        } else if has_visible_text {
            text_rect = Rect::new(content_left + MSG_PADDING.left, y, inner_width, text_h);
            y += text_h;
        }

        if !input.hides_bottom_info {
            y += (bottom_info.current_size.height - fonts.date.height).max(0);
        }

        if reactions_in_bubble {
            y += MEDIA_IN_BUBBLE_SKIP;
            reactions_rect = Rect::new(
                content_left + MSG_PADDING.left,
                y,
                inner_width.min(input.reactions_size.width),
                input.reactions_size.height,
            );
            y += input.reactions_size.height;
        }

        if !media_on_bottom {
            y += MSG_PADDING.bottom;
            if final_media.is_some() && !has_visible_text {
                y += MEDIA_IN_BUBBLE_SKIP;
            }
        }

        if input.has_comments_button {
            comments_rect = Rect::new(
                content_left,
                y,
                content_width,
                HISTORY_COMMENTS_BUTTON_HEIGHT,
            );
            y += HISTORY_COMMENTS_BUTTON_HEIGHT;
        }

        bubble_height = y - margin_top;
    } else if let Some(ref ml) = final_media {
        media_rect = Rect::new(
            content_left,
            margin_top,
            ml.current_size.width,
            ml.current_size.height,
        );
        bubble_height = ml.current_size.height;
    }

    let mut total_content_height = bubble_height;
    let mut keyboard_rect = Rect::ZERO;
    if let Some(kb) = input.inline_keyboard {
        let kb_top = margin_top + total_content_height + MSG_BOT_KB_BUTTON_MARGIN;
        keyboard_rect = Rect::new(content_left, kb_top, content_width, kb.height);
        total_content_height += MSG_BOT_KB_BUTTON_MARGIN + kb.height;
    }
    if has_reactions && !reactions_in_bubble {
        let r_top = margin_top + total_content_height + MEDIA_IN_BUBBLE_SKIP;
        let r_w = input.reactions_size.width.min(content_width);
        let r_x = if has_right_layout {
            content_left + content_width - r_w
        } else {
            content_left
        };
        reactions_rect = Rect::new(r_x, r_top, r_w, input.reactions_size.height);
        total_content_height += MEDIA_IN_BUBBLE_SKIP + input.reactions_size.height;
    }

    let total_height = margin_top + total_content_height + margin_bottom;
    let geometry = Rect::new(
        content_left,
        margin_top,
        content_width,
        total_content_height,
    );

    // 4. Bottom info placement rectangle.
    let (bottom_info_display, bottom_info_rect) = if input.hides_bottom_info {
        (InfoDisplayType::Default, Rect::ZERO)
    } else if unwrapped {
        let pill_w = bottom_info.current_size.width + 2 * MSG_DATE_IMG_PADDING.x;
        let pill_h = bottom_info.current_size.height + 2 * MSG_DATE_IMG_PADDING.y;
        let x = content_left + content_width - pill_w - MSG_DATE_IMG_DELTA;
        let y = margin_top + bubble_height - pill_h - MSG_DATE_IMG_DELTA;
        (InfoDisplayType::Background, Rect::new(x, y, pill_w, pill_h))
    } else if final_media
        .as_ref()
        .is_some_and(|m| m.is_bubble_bottom && m.skip_bubble_tail)
    {
        let pill_w = bottom_info.current_size.width + 2 * MSG_DATE_IMG_PADDING.x;
        let pill_h = bottom_info.current_size.height + 2 * MSG_DATE_IMG_PADDING.y;
        let x = media_rect.right() - pill_w - MSG_DATE_IMG_DELTA;
        let y = media_rect.bottom() - pill_h - MSG_DATE_IMG_DELTA;
        (InfoDisplayType::Image, Rect::new(x, y, pill_w, pill_h))
    } else {
        let info_bottom = if input.has_comments_button {
            comments_rect.y
        } else {
            margin_top + bubble_height
        };
        let x = content_left + content_width - MSG_PADDING.right + MSG_DATE_DELTA.x
            - bottom_info.current_size.width;
        let y =
            info_bottom - MSG_PADDING.bottom + MSG_DATE_DELTA.y - bottom_info.current_size.height;
        (
            InfoDisplayType::Default,
            Rect::new(
                x,
                y,
                bottom_info.current_size.width,
                bottom_info.current_size.height,
            ),
        )
    };

    let skip_tail = final_media.as_ref().is_some_and(|m| m.skip_bubble_tail)
        || input.inline_keyboard.is_some()
        || input.centered_view;
    let message_rounding = count_message_rounding(
        input.attach.bubble_attached_to_previous,
        input.attach.bubble_attached_to_next,
        has_right_layout,
        skip_tail,
    );
    let bubble_rounding = count_bubble_rounding(message_rounding, input.inline_keyboard.is_some());

    MessageLayout {
        has_bubble: draw_bubble,
        unwrapped,
        has_out_layout,
        has_right_layout,
        optimal_size,
        current_size: Size::new(viewport_width, total_height),
        margin_top,
        margin_bottom,
        geometry,
        message_rounding,
        bubble_rounding,
        header_heights: headers,
        text_rect,
        media_rect,
        bottom_info_rect,
        bottom_info_display,
        bottom_info,
        reactions_rect,
        keyboard_rect,
        comments_rect,
        media_layout: final_media,
    }
}

// ---------------------------------------------------------------------------
// Service message layout & centering (history_view_service_message.cpp)
// ---------------------------------------------------------------------------

/// Maximum chat content width in Wide mode (`HistoryView::WideChatWidth()`,
/// `history_view_service_message.cpp:214-216`).
#[must_use]
pub const fn wide_chat_width() -> i32 {
    MSG_MAX_WIDTH + 2 * MSG_PHOTO_SKIP + 2 * MSG_MARGIN.left
}

/// Computes the centered pill rectangle for a date separator badge
/// (`PaintPreparedDate`, `history_view_service_message.cpp:149-188`).
#[must_use]
pub fn count_service_date_rect(
    date_text_width: i32,
    y: i32,
    viewport_width: i32,
    chat_wide: bool,
    fonts: &FontMetrics,
) -> Rect {
    let max_width = if chat_wide {
        viewport_width.min(wide_chat_width())
    } else {
        viewport_width
    };
    let w = max_width - MSG_SERVICE_MARGIN.left - MSG_SERVICE_MARGIN.left;
    let bubble_w = date_text_width + MSG_SERVICE_PADDING.left + MSG_SERVICE_PADDING.right;
    let left = MSG_SERVICE_MARGIN.left + (w - bubble_w) / 2;
    let height = MSG_SERVICE_PADDING.top + fonts.semibold.height + MSG_SERVICE_PADDING.bottom;
    Rect::new(left, y + MSG_SERVICE_MARGIN.top, bubble_w, height)
}

/// Smooths multiline service message line widths so adjacent lines differ by either `0`
/// or at least `minDelta` for clean inverted corners
/// (`ServiceMessagePainter::CountLineWidths`, `history_view_service_message.cpp:392-432`).
#[must_use]
pub fn count_service_line_widths(raw_line_widths: &[i32], fonts: &FontMetrics) -> Vec<i32> {
    let mut result: Vec<i32> = raw_line_widths.iter().map(|&w| w.max(0)).collect();
    let min_delta = 2
        * (fonts.history_service_msg_radius() + fonts.history_service_msg_inverted_radius()
            - fonts.history_service_msg_inverted_shrink());

    let count = result.len();
    let mut i: isize = 0;
    while (i as usize) < count {
        let idx = i as usize;
        let mut width = result[idx].max(0);
        if idx > 0 {
            let width_before = result[idx - 1];
            if width < width_before && width + min_delta > width_before {
                width = width_before;
            }
        }
        if idx + 1 < count {
            let width_after = result[idx + 1];
            if width < width_after && width + min_delta > width_after {
                width = width_after;
            }
        }
        if width > result[idx] {
            result[idx] = width;
            if idx > 0 {
                let width_before = result[idx - 1];
                if width_before != width
                    && width_before < width + min_delta
                    && width_before + min_delta > width
                {
                    i -= 2;
                }
            }
        }
        i += 1;
    }
    result
}

/// Input parameters for laying out a centered service message (`HistoryView::Service`).
#[derive(Debug, Clone, Default)]
pub struct ServiceInput {
    /// True if hidden.
    pub is_hidden: bool,
    /// Chat layout mode (`Default`, `Wide`, `Narrow`).
    pub chat_mode: ElementChatMode,
    /// True if a date separator badge sits above this service message.
    pub display_date: bool,
    /// True if an unread bar sits above this service message.
    pub has_unread_bar: bool,
    /// Optional forum thread bar height above this item.
    pub forum_thread_bar_height: i32,
    /// Optional service pre-message height.
    pub service_pre_message_height: i32,
    /// Extra added top/bottom margins.
    pub added_margin_top: i32,
    pub added_margin_bottom: i32,
    /// Single-line max width of service text (`0` if empty).
    pub text_max_width: i32,
    /// Single-line min height of service text (`0` if empty).
    pub text_min_height: i32,
    /// Wrapped height of service text at the available content width.
    pub text_wrapped_height: i32,
    /// Optional attached service media (e.g. 100x100 chat photo update, gift box).
    pub media: Option<MediaInput>,
    /// True if the media hides the service text line (`media->hideServiceText()`).
    pub media_hides_service_text: bool,
    /// Optional reactions bar size.
    pub reactions_size: Size,
    /// Optional inline reply keyboard size.
    pub inline_keyboard: Option<Size>,
}

/// Computed layout for a centered service message (`HistoryView::Service`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLayout {
    /// Optimal size `(max_width, min_height)` (`Service::performCountOptimalSize`).
    pub optimal_size: Size,
    /// Current element size `(viewport_width, total_height)` (`Service::performCountCurrentSize`).
    pub current_size: Size,
    /// Top margin (`Service::marginTop`).
    pub margin_top: i32,
    /// Bottom margin (`Service::marginBottom`).
    pub margin_bottom: i32,
    /// Inner service area (`Service::countGeometry`).
    pub geometry: Rect,
    /// Centered text bubble rectangle (empty if text hidden/empty).
    pub text_bubble_rect: Rect,
    /// Centered media rectangle below text (empty if no media).
    pub media_rect: Rect,
    /// Centered inline keyboard rectangle (empty if none).
    pub keyboard_rect: Rect,
}

/// Computes the layout of a service message (`Service::performCountOptimalSize`,
/// `Service::performCountCurrentSize`, `Service::countGeometry` in
/// `history_view_service_message.cpp:470-610`).
#[must_use]
pub fn layout_service_message(
    input: &ServiceInput,
    viewport_width: i32,
    fonts: &FontMetrics,
) -> ServiceLayout {
    let margin_top = {
        let mut r = if input.is_hidden {
            0
        } else {
            MSG_SERVICE_MARGIN.top
        };
        if input.display_date {
            r += date_badge_height(fonts);
        }
        if input.has_unread_bar {
            r += unread_bar_height();
        }
        r += input.forum_thread_bar_height;
        r += input.service_pre_message_height;
        r += input.added_margin_top;
        r
    };
    let margin_bottom = MSG_SERVICE_MARGIN.bottom + input.added_margin_bottom;

    let opt_media = input
        .media
        .as_ref()
        .map(|m| m.layout(MSG_MAX_WIDTH, false, false, fonts));

    let optimal_size = if let Some(ref ml) = opt_media
        && input.media_hides_service_text
    {
        ml.optimal_size
    } else {
        Size::new(
            input.text_max_width + MSG_SERVICE_PADDING.horizontal(),
            input.text_min_height,
        )
    };

    if input.is_hidden {
        return ServiceLayout {
            optimal_size,
            current_size: Size::new(viewport_width, margin_top),
            margin_top,
            margin_bottom,
            geometry: Rect::ZERO,
            text_bubble_rect: Rect::ZERO,
            media_rect: Rect::ZERO,
            keyboard_rect: Rect::ZERO,
        };
    }

    let outer_w = if matches!(input.chat_mode, ElementChatMode::Wide) {
        viewport_width.min(wide_chat_width())
    } else {
        viewport_width
    };
    let content_width = (outer_w - MSG_SERVICE_MARGIN.left - MSG_SERVICE_MARGIN.left)
        .max(MSG_SERVICE_PADDING.horizontal() + 1);

    let mut new_height = margin_top;
    let mut text_bubble_rect = Rect::ZERO;
    let mut media_rect = Rect::ZERO;
    let mut final_media_width = 0;

    if let Some(ref media_in) = input.media
        && input.media_hides_service_text
    {
        let ml = media_in.layout(viewport_width, false, false, fonts);
        final_media_width = ml.current_size.width;
        let m_left = MSG_SERVICE_MARGIN.left + (content_width - ml.current_size.width) / 2;
        media_rect = Rect::new(
            m_left,
            new_height,
            ml.current_size.width,
            ml.current_size.height,
        );
        new_height += ml.current_size.height + margin_bottom;
    } else if input.text_max_width > 0 || input.text_min_height > 0 {
        let nwidth = (content_width - MSG_SERVICE_PADDING.horizontal()).max(0);
        let text_h = if content_width >= optimal_size.width {
            optimal_size.height.max(fonts.semibold.height)
        } else if input.text_wrapped_height > 0 {
            input.text_wrapped_height
        } else {
            fonts.semibold.height
        };
        let bubble_w = (input.text_max_width + MSG_SERVICE_PADDING.horizontal()).min(content_width);
        let bubble_h = text_h + MSG_SERVICE_PADDING.vertical();
        let bubble_left = MSG_SERVICE_MARGIN.left + (content_width - bubble_w) / 2;
        text_bubble_rect = Rect::new(bubble_left, new_height, bubble_w, bubble_h);
        new_height += bubble_h;

        if let Some(ref media_in) = input.media {
            let max_m_w = opt_media
                .as_ref()
                .map_or(nwidth, |m| m.optimal_size.width.min(nwidth));
            let ml = media_in.layout(max_m_w, false, false, fonts);
            final_media_width = ml.current_size.width;
            let m_top = new_height + MSG_SERVICE_MARGIN.top;
            let m_left = MSG_SERVICE_MARGIN.left + (content_width - ml.current_size.width) / 2;
            media_rect = Rect::new(m_left, m_top, ml.current_size.width, ml.current_size.height);
            new_height += MSG_SERVICE_MARGIN.top + ml.current_size.height;
        }
        new_height += margin_bottom;
    } else {
        new_height -= MSG_SERVICE_MARGIN.top;
    }

    if !input.reactions_size.is_empty() {
        new_height += MEDIA_IN_BUBBLE_SKIP + input.reactions_size.height;
    }

    let mut keyboard_rect = Rect::ZERO;
    if let Some(kb) = input.inline_keyboard {
        let kb_w = if final_media_width > 0 {
            final_media_width
        } else {
            content_width
        };
        let kb_left = MSG_SERVICE_MARGIN.left + (content_width - kb_w) / 2;
        let kb_top = new_height - margin_bottom + MSG_BOT_KB_BUTTON_MARGIN;
        keyboard_rect = Rect::new(kb_left, kb_top, kb_w, kb.height);
        new_height += MSG_BOT_KB_BUTTON_MARGIN + kb.height;
    }

    let geometry = Rect::new(
        MSG_SERVICE_MARGIN.left,
        margin_top,
        outer_w - MSG_SERVICE_MARGIN.left - MSG_SERVICE_MARGIN.right,
        (new_height - margin_top - MSG_SERVICE_MARGIN.bottom).max(0),
    );

    ServiceLayout {
        optimal_size,
        current_size: Size::new(viewport_width, new_height),
        margin_top,
        margin_bottom,
        geometry,
        text_bubble_rect,
        media_rect,
        keyboard_rect,
    }
}
