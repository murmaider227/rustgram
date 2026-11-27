//! Pure-logic view-model and layout engine for Telegram Desktop chat history and dialogs.
//!
//! Port of Telegram Desktop's `Telegram/SourceFiles/history/`, `Telegram/SourceFiles/ui/grouped_layout.cpp`,
//! `Telegram/SourceFiles/layout/layout_selection.cpp`, and `Telegram/SourceFiles/dialogs/ui/dialogs_layout.cpp`.
//!
//! This crate intentionally has **no GUI framework dependency**: all geometry is represented with
//! plain [`Point`], [`Size`], [`Rect`], and [`Margins`] types so `tg-app` can paint the computed
//! layouts with `egui` (or any renderer).

pub mod consts;
pub mod datetime;
pub mod dialogs;
pub mod element;
pub mod geometry;
pub mod grouped;
pub mod list;
pub mod media;
pub mod reactions;
pub mod selection;
pub mod text_cut;

pub use consts::{FontMetrics, LayoutConsts};
pub use datetime::{CivilDate, LocalDateTime, Weekday};
pub use dialogs::{
    AvatarCornerBadge, DialogBadgesState, DialogChatTypeIcon, DialogRowInput, DialogRowLayout,
    DialogRowMode, DialogRowStyle, DialogSendState, DialogsDateKind, classify_dialogs_date,
    elide_text_right, format_dialogs_date, format_unread_counter, format_unread_counter_short,
    layout_dialog_row,
};
pub use element::{
    AttachMetadata, BottomInfoInput, BottomInfoLayout, ElementChatMode, InfoDisplayType,
    MessageHeaderHeights, MessageInput, MessageLayout, ReplyInput, ReplyLayout,
    SequenceAttachState, ServiceInput, ServiceLayout, compute_is_attach_to_previous,
    compute_sequence_attach_states, count_bubble_rounding, count_message_rounding,
    count_service_date_rect, count_service_line_widths, date_badge_height, is_same_day,
    layout_message, layout_service_message, min_width_for_media, should_display_date,
    skip_block_height, skip_block_width, unread_bar_height, unread_bar_margin_top, wide_chat_width,
};
pub use geometry::{
    BubbleCornerRounding, BubbleRounding, BubbleSelectionInterval, Margins, Point, Rect, RectParts,
    Size, downscaled_size, get_corners_from_sides, get_image_scale_size_for_geometry,
    non_empty_size, safe_round,
};
pub use grouped::{
    GroupMediaLayout, GroupPointState, GroupedMediaInput, GroupedMediaLayout, GroupedMode,
    apply_rounding_sides, grouped_padding, layout_media_group, layout_playlist,
};
pub use list::{
    CollapseGap, ElementHighlighter, HighlightPaintState, HighlightTarget, HistoryList,
    HistoryListItem, PreloadDecision, ScrollTopState, UserpicPlacement, VisibleItem,
    is_message_position_valid,
};
pub use media::{
    CallInput, ContactInput, CustomEmojiInput, DocumentInput, DocumentKind, ExpandDecision,
    GifInput, GifKind, LargeEmojiInput, LocationInput, MediaInput, MediaLayout, PhotoInput,
    PollAnswerInput, PollInput, StickerInput, StickerKind, WebPageInput, adjust_custom_emoji_size,
    count_call_optimal_size, count_contact_optimal_size, count_desired_media_size,
    count_document_current_size, count_document_optimal_size, count_gif_current_size,
    count_gif_optimal_size, count_gif_thumb_size, count_location_current_size,
    count_location_optimal_size, count_media_size, count_photo_current_size,
    count_photo_media_size, count_photo_optimal_size, count_poll_current_size,
    count_poll_optimal_size, count_sticker_optimal_size, count_web_page_current_size,
    count_web_page_optimal_size, decide_frame_resize, frame_resize_may_expand,
    large_emoji_inner_rect, large_emoji_size, sticker_emoji_size, sticker_max_size,
    unwrapped_content_rect, waveform_bar_count,
};
pub use reactions::{
    InlineReactionsLayout, format_count_to_short, inline_reactions_optimal_size,
    layout_inline_reactions, reaction_count_button_width,
};
pub use selection::{
    ALL_TEXT_SELECTION, FULL_SELECTION, FlatSelectionEndpoint, HistorySelection,
    ListSelectionEndpoint, MAX_GROUP_SELECTION_ITEMS, MessageSelection, TextSelection,
    add_group_item_selection, first_group_item_index, is_group_item_selection,
    is_sub_group_selection, shift_item_selection, unshift_item_selection,
};
pub use tg_data::{FullMsgId, MessagePosition, MsgId, PeerId, TimeId};
