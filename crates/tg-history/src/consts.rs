//! Numeric layout constants extracted from Telegram Desktop `.style` and `.cpp` files.
//!
//! These constants mirror tdesktop's 1x logical pixel values so layout calculations are
//! exact and testable standalone; `tg-style` will supply scaled runtime values later.
//!
//! Sources:
//! - `Telegram/SourceFiles/ui/chat/chat.style`
//! - `Telegram/SourceFiles/dialogs/dialogs.style`
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_layout.style`
//! - `Telegram/SourceFiles/boxes/polls.style`
//! - `Telegram/SourceFiles/boxes/boxes.style`
//! - `Telegram/SourceFiles/window/window.style`
//! - `Telegram/SourceFiles/chat_helpers/chat_helpers.style`
//! - `Telegram/lib_ui/ui/basic.style`

use crate::geometry::{Margins, Point, Size};

// ---------------------------------------------------------------------------
// Basic / global style constants (lib_ui/ui/basic.style)
// ---------------------------------------------------------------------------

/// `fsize: 13px;` (`lib_ui/ui/basic.style:53`)
pub const FSIZE: i32 = 13;
/// `emojiSize: 18px;` (`lib_ui/ui/basic.style:59`)
pub const EMOJI_SIZE: i32 = 18;
/// `st::largeEmojiSize` (`chat.style`): an emoji of an emoji-only message.
pub const LARGE_EMOJI_SIZE: i32 = 36;
/// `st::largeEmojiOutline`: the white outline around each large emoji.
pub const LARGE_EMOJI_OUTLINE: i32 = 1;
/// `st::largeEmojiSkip`: the step between large emoji, outlines included.
pub const LARGE_EMOJI_SKIP: i32 = 4;
/// `Ui::Text::kIsolatedEmojiLimit`: most emoji a message can have to be drawn large.
pub const ISOLATED_EMOJI_LIMIT: usize = 3;
/// `lineWidth: 1px;` (`lib_ui/ui/basic.style:62`)
pub const LINE_WIDTH: i32 = 1;
/// `roundRadiusLarge: 6px;` (`lib_ui/ui/basic.style:107`)
pub const ROUND_RADIUS_LARGE: i32 = 6;
/// `roundRadiusSmall: 3px;` (`lib_ui/ui/basic.style:108`)
pub const ROUND_RADIUS_SMALL: i32 = 3;
/// `activeFadeInDuration: 500;` ms (`lib_ui/ui/basic.style:116`)
pub const ACTIVE_FADE_IN_DURATION_MS: i64 = 500;
/// `activeFadeOutDuration: 3000;` ms (`lib_ui/ui/basic.style:117`)
pub const ACTIVE_FADE_OUT_DURATION_MS: i64 = 3000;
/// `fadeWrapDuration: 200;` ms (`lib_ui/ui/basic.style:101`)
pub const FADE_WRAP_DURATION_MS: i64 = 200;
/// `locationSize: size(320px, 240px);` (`lib_ui/ui/basic.style:126`)
pub const LOCATION_SIZE: Size = Size::new(320, 240);

// ---------------------------------------------------------------------------
// Message bubble & chat geometry (ui/chat/chat.style)
// ---------------------------------------------------------------------------

/// `msgMaxWidth: 430px;` (`ui/chat/chat.style:14`)
pub const MSG_MAX_WIDTH: i32 = 430;
/// `msgServicePhotoWidth: 100px;` (`ui/chat/chat.style:20`)
pub const MSG_SERVICE_PHOTO_WIDTH: i32 = 100;
/// `msgMinWidth: 160px;` (`ui/chat/chat.style:22`)
pub const MSG_MIN_WIDTH: i32 = 160;
/// `msgPhotoSize: 33px;` (`ui/chat/chat.style:23`)
pub const MSG_PHOTO_SIZE: i32 = 33;
/// `msgPhotoSkip: 40px;` (`ui/chat/chat.style:24`)
pub const MSG_PHOTO_SKIP: i32 = 40;
/// `msgPadding: margins(11px, 8px, 11px, 8px);` (`ui/chat/chat.style:25`)
pub const MSG_PADDING: Margins = Margins::new(11, 8, 11, 8);
/// `msgTagBadgePadding: margins(5px, -1px, 5px, 0px);` (`ui/chat/chat.style:26`)
pub const MSG_TAG_BADGE_PADDING: Margins = Margins::new(5, -1, 5, 0);
/// `msgMargin: margins(16px, 6px, 56px, 2px);` (`ui/chat/chat.style:52`)
pub const MSG_MARGIN: Margins = Margins::new(16, 6, 56, 2);
/// `msgMarginTopAttached: 0px;` (`ui/chat/chat.style:53`)
pub const MSG_MARGIN_TOP_ATTACHED: i32 = 0;
/// `msgShadow: 2px;` (`ui/chat/chat.style:54`)
pub const MSG_SHADOW: i32 = 2;

/// `historyReplyTop: 2px;` (`ui/chat/chat.style:63`)
pub const HISTORY_REPLY_TOP: i32 = 2;
/// `historyReplyBottom: 2px;` (`ui/chat/chat.style:64`)
pub const HISTORY_REPLY_BOTTOM: i32 = 2;
/// `historyReplyPreview: 32px;` (`ui/chat/chat.style:65`)
pub const HISTORY_REPLY_PREVIEW: i32 = 32;
/// `historyReplyPreviewMargin: margins(7px, 4px, 4px, 4px);` (`ui/chat/chat.style:66`)
pub const HISTORY_REPLY_PREVIEW_MARGIN: Margins = Margins::new(7, 4, 4, 4);
/// `historyReplyPadding: margins(11px, 2px, 6px, 2px);` (`ui/chat/chat.style:73`)
pub const HISTORY_REPLY_PADDING: Margins = Margins::new(11, 2, 6, 2);
/// `msgReplyPadding: margins(6px, 6px, 11px, 6px);` (`ui/chat/chat.style:91`)
pub const MSG_REPLY_PADDING: Margins = Margins::new(6, 6, 11, 6);
/// `msgReplyBarSkip: 10px;` (`ui/chat/chat.style:94`)
pub const MSG_REPLY_BAR_SKIP: i32 = 10;
/// `msgServicePadding: margins(12px, 3px, 12px, 4px);` (`ui/chat/chat.style:95`)
pub const MSG_SERVICE_PADDING: Margins = Margins::new(12, 3, 12, 4);
/// `msgServiceMargin: margins(10px, 10px, 10px, 2px);` (`ui/chat/chat.style:96`)
pub const MSG_SERVICE_MARGIN: Margins = Margins::new(10, 10, 10, 2);

/// `msgDateSpace: 12px;` (`ui/chat/chat.style:99`)
pub const MSG_DATE_SPACE: i32 = 12;
/// `msgDateDelta: point(2px, 5px);` (`ui/chat/chat.style:100`)
pub const MSG_DATE_DELTA: Point = Point::new(2, 5);
/// `msgDateImgDelta: 4px;` (`ui/chat/chat.style:102`)
pub const MSG_DATE_IMG_DELTA: i32 = 4;
/// `msgDateImgPadding: point(8px, 2px);` (`ui/chat/chat.style:103`)
pub const MSG_DATE_IMG_PADDING: Point = Point::new(8, 2);

/// `historyPagePreview.padding: margins(10px, 5px, 7px, 7px);` (`ui/chat/chat.style:108`)
pub const HISTORY_PAGE_PREVIEW_PADDING: Margins = Margins::new(10, 5, 7, 7);

/// `mediaCaptionSkip: 5px;` (`ui/chat/chat.style:155`)
pub const MEDIA_CAPTION_SKIP: i32 = 5;
/// `mediaInBubbleSkip: 5px;` (`ui/chat/chat.style:156`)
pub const MEDIA_IN_BUBBLE_SKIP: i32 = 5;
/// `mediaUnreadSize: 7px;` (`ui/chat/chat.style:157`)
pub const MEDIA_UNREAD_SIZE: i32 = 7;
/// `mediaUnreadSkip: 5px;` (`ui/chat/chat.style:158`)
pub const MEDIA_UNREAD_SKIP: i32 = 5;

/// `minPhotoSize: 100px;` (`ui/chat/chat.style:168`)
pub const MIN_PHOTO_SIZE: i32 = 100;
/// `maxMediaSize: 430px;` (`ui/chat/chat.style:169`)
pub const MAX_MEDIA_SIZE: i32 = 430;
/// `maxStickerSize: 224px;` (`ui/chat/chat.style:170`)
pub const MAX_STICKER_SIZE: i32 = 224;
/// `maxAnimatedEmojiSize: 112px;` (`ui/chat/chat.style:171`)
pub const MAX_ANIMATED_EMOJI_SIZE: i32 = 112;
/// `maxGifSize: 320px;` (`ui/chat/chat.style:172`)
pub const MAX_GIF_SIZE: i32 = 320;
/// `maxVideoMessageSize: 240px;` (`ui/chat/chat.style:173`)
pub const MAX_VIDEO_MESSAGE_SIZE: i32 = 240;
/// `maxSignatureSize: 144px;` (`ui/chat/chat.style:174`)
pub const MAX_SIGNATURE_SIZE: i32 = 144;
/// `historyMinimalWidth: 380px;` (`ui/chat/chat.style:179`)
pub const HISTORY_MINIMAL_WIDTH: i32 = 380;
/// `historyPaddingBottom: 8px;` (`ui/chat/chat.style:205`)
pub const HISTORY_PADDING_BOTTOM: i32 = 8;

/// `historySendStateSpace: 24px;` (`ui/chat/chat.style:238`)
pub const HISTORY_SEND_STATE_SPACE: i32 = 24;
/// `historySendStatePosition: point(-17px, -19px);` (`ui/chat/chat.style:239`)
pub const HISTORY_SEND_STATE_POSITION: Point = Point::new(-17, -19);
/// `historyShortcutStateSpace: 18px;` (`ui/chat/chat.style:240`)
pub const HISTORY_SHORTCUT_STATE_SPACE: i32 = 18;
/// `historyViewsSpace: 8px;` (`ui/chat/chat.style:242`)
pub const HISTORY_VIEWS_SPACE: i32 = 8;
/// `historyViewsWidth: 20px;` (`ui/chat/chat.style:243`)
pub const HISTORY_VIEWS_WIDTH: i32 = 20;
/// `historyPinWidth: 24px;` (`ui/chat/chat.style:244`)
pub const HISTORY_PIN_WIDTH: i32 = 24;
/// `historySilentWidth: 12px;` (`ui/chat/chat.style:247`)
pub const HISTORY_SILENT_WIDTH: i32 = 12;
/// `historyEphemeralStateWidth: 16px;` (`ui/chat/chat.style:341`)
pub const HISTORY_EPHEMERAL_STATE_WIDTH: i32 = 16;

/// `topicButtonSkip: 3px;` (`ui/chat/chat.style:260`)
pub const TOPIC_BUTTON_SKIP: i32 = 3;
/// `topicButtonPadding: margins(6px, 3px, 8px, 3px);` (`ui/chat/chat.style:261`)
pub const TOPIC_BUTTON_PADDING: Margins = Margins::new(6, 3, 8, 3);
/// `topicButtonArrowSkip: 8px;` (`ui/chat/chat.style:262`)
pub const TOPIC_BUTTON_ARROW_SKIP: i32 = 8;

/// `msgBotKbButton.margin: 2px;` (`ui/chat/chat.style:282`)
pub const MSG_BOT_KB_BUTTON_MARGIN: i32 = 2;
/// `msgBotKbButton.padding: 10px;` (`ui/chat/chat.style:283`)
pub const MSG_BOT_KB_BUTTON_PADDING: i32 = 10;
/// `msgBotKbButton.height: 36px;` (`ui/chat/chat.style:284`)
pub const MSG_BOT_KB_BUTTON_HEIGHT: i32 = 36;

/// `bubbleRadiusSmall: roundRadiusLarge;` (6px) (`ui/chat/chat.style:326`)
pub const BUBBLE_RADIUS_SMALL: i32 = ROUND_RADIUS_LARGE;
/// `bubbleRadiusLarge: 16px;` (`ui/chat/chat.style:327`)
pub const BUBBLE_RADIUS_LARGE: i32 = 16;

/// `historyPhotoLeft: 14px;` (`ui/chat/chat.style:329`)
pub const HISTORY_PHOTO_LEFT: i32 = 14;
/// `historyPhotoBubbleMinWidth: 200px;` (`ui/chat/chat.style:330`)
pub const HISTORY_PHOTO_BUBBLE_MIN_WIDTH: i32 = 200;
/// `historyPsaIconSkip1: 23px;` (`ui/chat/chat.style:334`)
pub const HISTORY_PSA_ICON_SKIP1: i32 = 23;

/// `historyUnreadBarHeight: 32px;` (`ui/chat/chat.style:353`)
pub const HISTORY_UNREAD_BAR_HEIGHT: i32 = 32;
/// `historyUnreadBarMargin: 8px;` (`ui/chat/chat.style:354`)
pub const HISTORY_UNREAD_BAR_MARGIN: i32 = 8;

/// `historyCallWidth: 240px;` (`ui/chat/chat.style:360`)
pub const HISTORY_CALL_WIDTH: i32 = 240;
/// `historyCallHeight: 56px;` (`ui/chat/chat.style:361`)
pub const HISTORY_CALL_HEIGHT: i32 = 56;
/// `historyCallIconPosition: point(12px, 10px);` (`ui/chat/chat.style:362`)
pub const HISTORY_CALL_ICON_POSITION: Point = Point::new(12, 10);
/// `historyCallLeft: 16px;` (`ui/chat/chat.style:363`)
pub const HISTORY_CALL_LEFT: i32 = 16;
/// `historyCallTop: 9px;` (`ui/chat/chat.style:364`)
pub const HISTORY_CALL_TOP: i32 = 9;
/// `historyCallStatusTop: 29px;` (`ui/chat/chat.style:365`)
pub const HISTORY_CALL_STATUS_TOP: i32 = 29;
/// `historyCallStatusSkip: 4px;` (`ui/chat/chat.style:366`)
pub const HISTORY_CALL_STATUS_SKIP: i32 = 4;
/// `historyCallIconSkip: 8px;` (`ui/chat/chat.style:367`)
pub const HISTORY_CALL_ICON_SKIP: i32 = 8;

/// File / document row layout constants (`HistoryFileLayout` in `ui/chat/chat.style:370-406`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLayoutStyle {
    /// Outer padding inside the file media block.
    pub padding: Margins,
    /// Top offset of the filename line.
    pub name_top: i32,
    /// Top offset of the status/size line.
    pub status_top: i32,
    /// Top offset of the link/action line (thumbed files).
    pub link_top: i32,
    /// Thumbnail / circular action button diameter.
    pub thumb_size: i32,
    /// Horizontal gap between thumbnail and text.
    pub thumb_skip: i32,
}

/// `msgFileLayout` (`ui/chat/chat.style:381-387`)
pub const MSG_FILE_LAYOUT: FileLayoutStyle = FileLayoutStyle {
    padding: Margins::new(12, 8, 10, 8),
    name_top: 12,
    status_top: 34,
    link_top: 0,
    thumb_size: 44,
    thumb_skip: 11,
};

/// `msgFileThumbLayout` (`ui/chat/chat.style:388-395`)
pub const MSG_FILE_THUMB_LAYOUT: FileLayoutStyle = FileLayoutStyle {
    padding: Margins::new(6, 6, 10, 6),
    name_top: 11,
    status_top: 31,
    link_top: 55,
    thumb_size: 72,
    thumb_skip: 14,
};

/// `msgFileLayoutGrouped` (`ui/chat/chat.style:396-400`)
pub const MSG_FILE_LAYOUT_GROUPED: FileLayoutStyle = FileLayoutStyle {
    padding: Margins::new(12, 5, 10, 5),
    name_top: 9,
    status_top: 31,
    link_top: 0,
    thumb_size: 44,
    thumb_skip: 11,
};

/// `msgFileThumbLayoutGrouped` (`ui/chat/chat.style:401-406`)
pub const MSG_FILE_THUMB_LAYOUT_GROUPED: FileLayoutStyle = FileLayoutStyle {
    padding: Margins::new(6, 3, 10, 3),
    name_top: 8,
    status_top: 28,
    link_top: 52,
    thumb_size: 72,
    thumb_skip: 14,
};

/// `msgFileMinWidth: 268px;` (`ui/chat/chat.style:424`)
pub const MSG_FILE_MIN_WIDTH: i32 = 268;
/// `msgFileTopMinus: 6px;` (`ui/chat/chat.style:425`)
pub const MSG_FILE_TOP_MINUS: i32 = 6;

/// `msgWaveformBar: 2px;` (`ui/chat/chat.style:430`)
pub const MSG_WAVEFORM_BAR: i32 = 2;
/// `msgWaveformSkip: 1px;` (`ui/chat/chat.style:431`)
pub const MSG_WAVEFORM_SKIP: i32 = 1;
/// `msgWaveformMin: 3px;` (`ui/chat/chat.style:432`)
pub const MSG_WAVEFORM_MIN: i32 = 3;
/// `msgWaveformMax: 17px;` (`ui/chat/chat.style:433`)
pub const MSG_WAVEFORM_MAX: i32 = 17;
/// `Media::Player::kWaveformSamplesCount` (`media/player/media_player_instance.h`)
pub const WAVEFORM_SAMPLES_COUNT: i32 = 100;

/// `historyTranscribeSkip: 10px;` (`ui/chat/chat.style:441`)
pub const HISTORY_TRANSCRIBE_SKIP: i32 = 10;
/// `historyTranscribeSize: size(28px, 22px);` (`ui/chat/chat.style:442`)
pub const HISTORY_TRANSCRIBE_SIZE: Size = Size::new(28, 22);

/// `topPeersSelectorUserpicSize: 31px;` -> `historyFastShareSize` (`chat_helpers.style:1868`, `chat.style:496`)
pub const HISTORY_FAST_SHARE_SIZE: i32 = 31;
/// `historyFastShareLeft: 13px;` (`ui/chat/chat.style:497`)
pub const HISTORY_FAST_SHARE_LEFT: i32 = 13;
/// `historyFastShareBottom: 5px;` (`ui/chat/chat.style:498`)
pub const HISTORY_FAST_SHARE_BOTTOM: i32 = 5;

/// `historyGroupWidthMax: maxMediaSize;` (`ui/chat/chat.style:509`)
pub const HISTORY_GROUP_WIDTH_MAX: i32 = MAX_MEDIA_SIZE;
/// `historyGroupWidthMin: minPhotoSize;` (`ui/chat/chat.style:510`)
pub const HISTORY_GROUP_WIDTH_MIN: i32 = MIN_PHOTO_SIZE;
/// `historyGroupSkip: 4px;` (`ui/chat/chat.style:511`)
pub const HISTORY_GROUP_SKIP: i32 = 4;

/// `webPagePhotoDelta: 8px;` (`ui/chat/chat.style:536`)
pub const WEB_PAGE_PHOTO_DELTA: i32 = 8;
/// `historyPageButtonHeight: 36px;` (`ui/chat/chat.style:550`)
pub const HISTORY_PAGE_BUTTON_HEIGHT: i32 = 36;
/// `historyPageButtonPadding: margins(13px, 8px, 13px, 8px);` (`ui/chat/chat.style:551`)
pub const HISTORY_PAGE_BUTTON_PADDING: Margins = Margins::new(13, 8, 13, 8);
/// `historyCommentsButtonHeight: 40px;` (`ui/chat/chat.style:558`)
pub const HISTORY_COMMENTS_BUTTON_HEIGHT: i32 = 40;
/// `historyCommentsSkipLeft: 9px;` (`ui/chat/chat.style:559`)
pub const HISTORY_COMMENTS_SKIP_LEFT: i32 = 9;
/// `historyCommentsSkipText: 10px;` (`ui/chat/chat.style:560`)
pub const HISTORY_COMMENTS_SKIP_TEXT: i32 = 10;
/// `historyCommentsSkipRight: 8px;` (`ui/chat/chat.style:561`)
pub const HISTORY_COMMENTS_SKIP_RIGHT: i32 = 8;
/// `historyCommentsUserpics.size: 25px;` (`ui/chat/chat.style:563`)
pub const HISTORY_COMMENTS_USERPIC_SIZE: i32 = 25;
/// `historyCommentsUserpics.shift: 6px;` (`ui/chat/chat.style:564`)
pub const HISTORY_COMMENTS_USERPIC_SHIFT: i32 = 6;
/// `HistoryMessageViews::kMaxRecentRepliers = 3`
pub const MAX_RECENT_REPLIERS: i32 = 3;

/// `reactionInfoSize: 15px;` (`ui/chat/chat.style:701`)
pub const REACTION_INFO_SIZE: i32 = 15;
/// `reactionInfoSkip: 3px;` (`ui/chat/chat.style:702`)
pub const REACTION_INFO_SKIP: i32 = 3;
/// `reactionInfoBetween: 3px;` (`ui/chat/chat.style:703`)
pub const REACTION_INFO_BETWEEN: i32 = 3;
/// `effectInfoImage: 12px;` (`ui/chat/chat.style:727`)
pub const EFFECT_INFO_IMAGE: i32 = 12;

// ---------------------------------------------------------------------------
// Polls & contacts (boxes/polls.style, boxes/boxes.style)
// ---------------------------------------------------------------------------

/// `contactsPhotoSize: 42px;` (`boxes/boxes.style:45`)
pub const CONTACTS_PHOTO_SIZE: i32 = 42;
/// `historyPollQuestionTop: 7px;` (`boxes/polls.style:21`)
pub const HISTORY_POLL_QUESTION_TOP: i32 = 7;
/// `historyPollSubtitleSkip: 4px;` (`boxes/polls.style:27`)
pub const HISTORY_POLL_SUBTITLE_SKIP: i32 = 4;
/// `historyPollAnswerPadding: margins(32px, 6px, 0px, 6px);` (`boxes/polls.style:28`)
pub const HISTORY_POLL_ANSWER_PADDING: Margins = Margins::new(32, 6, 0, 6);
/// `historyPollAnswerPaddingNoMedia: margins(32px, 10px, 0px, 10px);` (`boxes/polls.style:29`)
pub const HISTORY_POLL_ANSWER_PADDING_NO_MEDIA: Margins = Margins::new(32, 10, 0, 10);
/// `historyPollAnswersSkip: 2px;` (`boxes/polls.style:30`)
pub const HISTORY_POLL_ANSWERS_SKIP: i32 = 2;
/// `historyPollTotalVotesSkip: 5px;` (`boxes/polls.style:34`)
pub const HISTORY_POLL_TOTAL_VOTES_SKIP: i32 = 5;
/// `historyPollBottomButtonSkip: 15px;` (`boxes/polls.style:76`)
pub const HISTORY_POLL_BOTTOM_BUTTON_SKIP: i32 = 15;

// ---------------------------------------------------------------------------
// History element / list behaviour constants
// ---------------------------------------------------------------------------

/// Consecutive messages from the same sender attach within 15 minutes (900 seconds).
/// `kAttachMessageToPreviousSecondsDelta = 900` (`history/view/history_view_element.cpp:105`)
pub const ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA: i32 = 900;

/// Maximum items in a media album group (`GroupedMedia::kMaxSize = 10`).
pub const MAX_GROUP_SIZE: usize = 10;

/// Maximum unexpanded lines in a reply preview (`kNonExpandedLinesLimit = 5`, `history_view_reply.cpp:46`).
pub const REPLY_NON_EXPANDED_LINES_LIMIT: i32 = 5;

/// `kMaxSizeFixed = 512` (`history_view_sticker.cpp:44`)
pub const STICKER_MAX_SIZE_FIXED: i32 = 512;
/// `kMaxEmojiSizeFixed = 256` (`history_view_sticker.cpp:45`)
pub const STICKER_MAX_EMOJI_SIZE_FIXED: i32 = 256;

/// `kPreloadedScreensCount = 4` (`history_view_list_widget.cpp:117`)
pub const PRELOADED_SCREENS_COUNT: i32 = 4;
/// `kPreloadIfLessThanScreens = 2` (`history_view_list_widget.cpp:118`)
pub const PRELOAD_IF_LESS_THAN_SCREENS: i32 = 2;
/// `kPreloadedScreensCountFull = kPreloadedScreensCount + 1 + kPreloadedScreensCount = 9` (`history_view_list_widget.cpp:119`)
pub const PRELOADED_SCREENS_COUNT_FULL: i32 = PRELOADED_SCREENS_COUNT + 1 + PRELOADED_SCREENS_COUNT;

// ---------------------------------------------------------------------------
// Dialogs list layout constants (dialogs/dialogs.style, dialogs_layout.style, window.style)
// ---------------------------------------------------------------------------

/// `columnMinimalWidthLeft: 260px;` (`window/window.style:34`)
pub const COLUMN_MINIMAL_WIDTH_LEFT: i32 = 260;
/// `dialogsRowHeight: 62px;` (`dialogs/dialogs.style:91`)
pub const DIALOGS_ROW_HEIGHT: i32 = 62;
/// `dialogsUnreadHeight: 19px;` (`dialogs/dialogs.style:79`)
pub const DIALOGS_UNREAD_HEIGHT: i32 = 19;
/// `dialogsUnreadPadding: 5px;` (`dialogs/dialogs.style:80`)
pub const DIALOGS_UNREAD_PADDING: i32 = 5;
/// `dialogsDateSkip: 5px;` (`dialogs/dialogs.style:89`)
pub const DIALOGS_DATE_SKIP: i32 = 5;
/// `dialogsImportantBarHeight: 37px;` (`dialogs/dialogs.style:216`)
pub const DIALOGS_IMPORTANT_BAR_HEIGHT: i32 = 37;
/// `dialogsTextWidthMin: 150px;` (`dialogs/dialogs.style:221`)
pub const DIALOGS_TEXT_WIDTH_MIN: i32 = 150;
/// `dialogsChatTypeSkip: 3px;` (`dialogs/dialogs.style:388`)
pub const DIALOGS_CHAT_TYPE_SKIP: i32 = 3;
/// `dialogsSendStateSkip: 20px;` (`dialogs/dialogs.style:405`)
pub const DIALOGS_SEND_STATE_SKIP: i32 = 20;
/// `dialogsMuteIconSkip: 4px;` (`dialogs/ui/dialogs_layout.style:112`)
pub const DIALOGS_MUTE_ICON_SKIP: i32 = 4;
/// `dialogsTopBarLeftPadding: 18px;` (`dialogs/dialogs.style:858`)
pub const DIALOGS_TOP_BAR_LEFT_PADDING: i32 = 18;
/// `dialogsOnlineBadgeStroke: 2px;` (`dialogs/dialogs.style:185`)
pub const DIALOGS_ONLINE_BADGE_STROKE: i32 = 2;
/// `dialogsOnlineBadgeSize: 10px;` (`dialogs/dialogs.style:186`)
pub const DIALOGS_ONLINE_BADGE_SIZE: i32 = 10;
/// `dialogsOnlineBadgeSkip: point(0px, 2px);` (`dialogs/dialogs.style:187`)
pub const DIALOGS_ONLINE_BADGE_SKIP: Point = Point::new(0, 2);
/// `dialogsCallBadgeSize: 16px;` (`dialogs/dialogs.style:190`)
pub const DIALOGS_CALL_BADGE_SIZE: i32 = 16;
/// `dialogsCallBadgeSkip: point(-3px, -3px);` (`dialogs/dialogs.style:191`)
pub const DIALOGS_CALL_BADGE_SKIP: Point = Point::new(-3, -3);
/// `dialogsTTLBadgeSize: 20px;` (`dialogs/dialogs.style:202`)
pub const DIALOGS_TTL_BADGE_SIZE: i32 = 20;
/// `dialogsTTLBadgeSkip: point(1px, 1px);` (`dialogs/dialogs.style:206`)
pub const DIALOGS_TTL_BADGE_SKIP: Point = Point::new(1, 1);
/// `dialogsPinnedIcon` / mention / reaction / poll icon width (18px icons in `dialogs.style:592-611`).
pub const DIALOGS_ICON_BADGE_SIZE: i32 = 18;
/// `dialogsPinnedIcon` width (18px).
pub const DIALOGS_PINNED_ICON_SIZE: i32 = 18;
/// `dialogsMuteIcon` width (16px).
pub const DIALOGS_MUTE_ICON_SIZE: i32 = 16;
/// `dialogsChatIcon` / `dialogsChannelIcon` / `dialogsBotIcon` width (16px).
pub const DIALOGS_CHAT_TYPE_ICON_WIDTH: i32 = 16;

/// Dialogs date "recent" threshold: 20 hours in seconds (`kRecentlyInSeconds = 20 * 3600`,
/// `ui/text/format_values.cpp:726`).
pub const DIALOGS_DATE_RECENTLY_SECONDS: i64 = 20 * 3600;

/// Single font's vertical metrics needed for layout without a GUI backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontMetric {
    /// Total line height in logical pixels (`font->height`).
    pub height: i32,
    /// Ascent above baseline (`font->ascent`).
    pub ascent: i32,
    /// Descent below baseline (`font->descent`).
    pub descent: i32,
    /// Width of a space character (`font->spacew`).
    pub space_width: i32,
}

/// Complete set of font metrics used across history and dialogs layout.
///
/// Defaults match tdesktop's standard 13px UI font metrics at 1x scale so unit tests
/// and headless layout computations are deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontMetrics {
    /// `st::normalFont` / `st::msgFont` / `st::dialogsTextFont` (13px regular).
    pub normal: FontMetric,
    /// `st::semiboldFont` / `st::msgNameFont` / `st::msgServiceFont` (13px semibold).
    pub semibold: FontMetric,
    /// `st::msgDateFont` / `st::dialogsDateFont` (13px regular).
    pub date: FontMetric,
    /// `st::dialogsUnreadFont` (12px bold).
    pub dialogs_unread: FontMetric,
    /// `st::webPageTitleFont` (13px semibold).
    pub web_page_title: FontMetric,
    /// `st::webPageDescriptionFont` (13px regular).
    pub web_page_description: FontMetric,
    /// `st::historyUnreadBarFont` (13px semibold).
    pub unread_bar: FontMetric,
}

impl Default for FontMetrics {
    fn default() -> Self {
        let regular_13 = FontMetric {
            height: 18,
            ascent: 14,
            descent: 4,
            space_width: 4,
        };
        let semibold_13 = FontMetric {
            height: 18,
            ascent: 14,
            descent: 4,
            space_width: 4,
        };
        let bold_12 = FontMetric {
            height: 16,
            ascent: 13,
            descent: 3,
            space_width: 3,
        };
        Self {
            normal: regular_13,
            semibold: semibold_13,
            date: regular_13,
            dialogs_unread: bold_12,
            web_page_title: semibold_13,
            web_page_description: regular_13,
            unread_bar: semibold_13,
        }
    }
}

impl FontMetrics {
    /// `HistoryView::UnitedLineHeight()` (`history_view_media_common.cpp:263`).
    #[must_use]
    pub const fn united_line_height(&self) -> i32 {
        if self.semibold.height > self.normal.height {
            self.semibold.height
        } else {
            self.normal.height
        }
    }

    /// `Ui::HistoryServiceMsgRadius()` (`ui/chat/chat_style.cpp:94`).
    #[must_use]
    pub const fn history_service_msg_radius(&self) -> i32 {
        let min_message_height =
            MSG_SERVICE_PADDING.top + self.semibold.height + MSG_SERVICE_PADDING.bottom;
        min_message_height / 2
    }

    /// `Ui::HistoryServiceMsgInvertedRadius()` (`ui/chat/chat_style.cpp:104`).
    #[must_use]
    pub const fn history_service_msg_inverted_radius(&self) -> i32 {
        self.semibold.height - self.history_service_msg_radius()
    }

    /// `Ui::HistoryServiceMsgInvertedShrink()` (`ui/chat/chat_style.cpp:112`).
    #[must_use]
    pub const fn history_service_msg_inverted_shrink(&self) -> i32 {
        (self.history_service_msg_inverted_radius() * 2) / 3
    }
}

impl FileLayoutStyle {
    /// Reads a [`tg_style::structs::HistoryFileLayout`] scaled to `scale`.
    #[must_use]
    pub fn from_style(
        style: &tg_style::structs::HistoryFileLayout,
        scale: tg_style::Scale,
    ) -> Self {
        let m = style.padding.scaled_at(scale);
        Self {
            padding: Margins::new(m.left, m.top, m.right, m.bottom),
            name_top: style.name_top.scaled_at(scale),
            status_top: style.status_top.scaled_at(scale),
            link_top: style.link_top.scaled_at(scale),
            thumb_size: style.thumb_size.scaled_at(scale),
            thumb_skip: style.thumb_skip.scaled_at(scale),
        }
    }
}

fn scale_point(p: tg_style::Point, scale: tg_style::Scale) -> Point {
    let s = p.scaled_at(scale);
    Point::new(s.x, s.y)
}

fn scale_size(s: tg_style::Size, scale: tg_style::Scale) -> Size {
    let sc = s.scaled_at(scale);
    Size::new(sc.width, sc.height)
}

fn scale_margins(m: tg_style::Margins, scale: tg_style::Scale) -> Margins {
    let sc = m.scaled_at(scale);
    Margins::new(sc.left, sc.top, sc.right, sc.bottom)
}

/// Complete set of DPI-scaled layout constants read from `tg-style` (`tg_style::st::*`).
///
/// Use [`LayoutConsts::from_style`] with an explicit [`tg_style::Scale`], or
/// [`LayoutConsts::from_current_style`] for the active global scale. [`Default`] returns
/// the unscaled 100% constants defined in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutConsts {
    // Basic / global style constants
    /// `st::fsize`
    pub fsize: i32,
    /// `st::emoji_size`
    pub emoji_size: i32,
    /// `st::line_width`
    pub line_width: i32,
    /// `st::round_radius_large`
    pub round_radius_large: i32,
    /// `st::round_radius_small`
    pub round_radius_small: i32,
    /// `st::active_fade_in_duration` (ms)
    pub active_fade_in_duration_ms: i64,
    /// `st::active_fade_out_duration` (ms)
    pub active_fade_out_duration_ms: i64,
    /// `st::fade_wrap_duration` (ms)
    pub fade_wrap_duration_ms: i64,
    /// `st::location_size`
    pub location_size: Size,

    // Message bubble & chat geometry
    /// `st::msg_max_width`
    pub msg_max_width: i32,
    /// `st::msg_service_photo_width`
    pub msg_service_photo_width: i32,
    /// `st::msg_min_width`
    pub msg_min_width: i32,
    /// `st::msg_photo_size`
    pub msg_photo_size: i32,
    /// `st::msg_photo_skip`
    pub msg_photo_skip: i32,
    /// `st::msg_padding`
    pub msg_padding: Margins,
    /// `st::msg_tag_badge_padding`
    pub msg_tag_badge_padding: Margins,
    /// `st::msg_margin`
    pub msg_margin: Margins,
    /// `st::msg_margin_top_attached`
    pub msg_margin_top_attached: i32,
    /// `st::msg_shadow`
    pub msg_shadow: i32,

    /// `st::history_reply_top`
    pub history_reply_top: i32,
    /// `st::history_reply_bottom`
    pub history_reply_bottom: i32,
    /// `st::history_reply_preview`
    pub history_reply_preview: i32,
    /// `st::history_reply_preview_margin`
    pub history_reply_preview_margin: Margins,
    /// `st::history_reply_padding`
    pub history_reply_padding: Margins,
    /// `st::msg_reply_padding`
    pub msg_reply_padding: Margins,
    /// `st::msg_reply_bar_skip`
    pub msg_reply_bar_skip: i32,
    /// `st::msg_service_padding`
    pub msg_service_padding: Margins,
    /// `st::msg_service_margin`
    pub msg_service_margin: Margins,

    /// `st::msg_date_space`
    pub msg_date_space: i32,
    /// `st::msg_date_delta`
    pub msg_date_delta: Point,
    /// `st::msg_date_img_delta`
    pub msg_date_img_delta: i32,
    /// `st::msg_date_img_padding`
    pub msg_date_img_padding: Point,

    /// `st::history_page_preview.padding`
    pub history_page_preview_padding: Margins,

    /// `st::media_caption_skip`
    pub media_caption_skip: i32,
    /// `st::media_in_bubble_skip`
    pub media_in_bubble_skip: i32,
    /// `st::media_unread_size`
    pub media_unread_size: i32,
    /// `st::media_unread_skip`
    pub media_unread_skip: i32,

    /// `st::min_photo_size`
    pub min_photo_size: i32,
    /// `st::max_media_size`
    pub max_media_size: i32,
    /// `st::max_sticker_size`
    pub max_sticker_size: i32,
    /// `st::max_animated_emoji_size`
    pub max_animated_emoji_size: i32,
    /// `st::max_gif_size`
    pub max_gif_size: i32,
    /// `st::max_video_message_size`
    pub max_video_message_size: i32,
    /// `st::max_signature_size`
    pub max_signature_size: i32,
    /// `st::history_minimal_width`
    pub history_minimal_width: i32,
    /// `st::history_padding_bottom`
    pub history_padding_bottom: i32,

    /// `st::history_send_state_space`
    pub history_send_state_space: i32,
    /// `st::history_send_state_position`
    pub history_send_state_position: Point,
    /// `st::history_shortcut_state_space`
    pub history_shortcut_state_space: i32,
    /// `st::history_views_space`
    pub history_views_space: i32,
    /// `st::history_views_width`
    pub history_views_width: i32,
    /// `st::history_pin_width`
    pub history_pin_width: i32,
    /// `st::history_silent_width`
    pub history_silent_width: i32,
    /// `st::history_ephemeral_state_width`
    pub history_ephemeral_state_width: i32,

    /// `st::topic_button_skip`
    pub topic_button_skip: i32,
    /// `st::topic_button_padding`
    pub topic_button_padding: Margins,
    /// `st::topic_button_arrow_skip`
    pub topic_button_arrow_skip: i32,

    /// `st::msg_bot_kb_button.margin`
    pub msg_bot_kb_button_margin: i32,
    /// `st::msg_bot_kb_button.padding`
    pub msg_bot_kb_button_padding: i32,
    /// `st::msg_bot_kb_button.height`
    pub msg_bot_kb_button_height: i32,

    /// `st::bubble_radius_small`
    pub bubble_radius_small: i32,
    /// `st::bubble_radius_large`
    pub bubble_radius_large: i32,

    /// `st::history_photo_left`
    pub history_photo_left: i32,
    /// `st::history_photo_bubble_min_width`
    pub history_photo_bubble_min_width: i32,
    /// `st::history_psa_icon_skip1`
    pub history_psa_icon_skip1: i32,

    /// `st::history_unread_bar_height`
    pub history_unread_bar_height: i32,
    /// `st::history_unread_bar_margin`
    pub history_unread_bar_margin: i32,

    /// `st::history_call_width`
    pub history_call_width: i32,
    /// `st::history_call_height`
    pub history_call_height: i32,
    /// `st::history_call_icon_position`
    pub history_call_icon_position: Point,
    /// `st::history_call_left`
    pub history_call_left: i32,
    /// `st::history_call_top`
    pub history_call_top: i32,
    /// `st::history_call_status_top`
    pub history_call_status_top: i32,
    /// `st::history_call_status_skip`
    pub history_call_status_skip: i32,
    /// `st::history_call_icon_skip`
    pub history_call_icon_skip: i32,

    /// `st::msg_file_layout`
    pub msg_file_layout: FileLayoutStyle,
    /// `st::msg_file_thumb_layout`
    pub msg_file_thumb_layout: FileLayoutStyle,
    /// `st::msg_file_layout_grouped`
    pub msg_file_layout_grouped: FileLayoutStyle,
    /// `st::msg_file_thumb_layout_grouped`
    pub msg_file_thumb_layout_grouped: FileLayoutStyle,
    /// `st::msg_file_min_width`
    pub msg_file_min_width: i32,
    /// `st::msg_file_top_minus`
    pub msg_file_top_minus: i32,

    /// `st::msg_waveform_bar`
    pub msg_waveform_bar: i32,
    /// `st::msg_waveform_skip`
    pub msg_waveform_skip: i32,
    /// `st::msg_waveform_min`
    pub msg_waveform_min: i32,
    /// `st::msg_waveform_max`
    pub msg_waveform_max: i32,

    /// `st::history_transcribe_skip`
    pub history_transcribe_skip: i32,
    /// `st::history_transcribe_size`
    pub history_transcribe_size: Size,

    /// `st::history_fast_share_size`
    pub history_fast_share_size: i32,
    /// `st::history_fast_share_left`
    pub history_fast_share_left: i32,
    /// `st::history_fast_share_bottom`
    pub history_fast_share_bottom: i32,

    /// `st::history_group_width_max`
    pub history_group_width_max: i32,
    /// `st::history_group_width_min`
    pub history_group_width_min: i32,
    /// `st::history_group_skip`
    pub history_group_skip: i32,

    /// `st::web_page_photo_delta`
    pub web_page_photo_delta: i32,
    /// `st::history_page_button_height`
    pub history_page_button_height: i32,
    /// `st::history_page_button_padding`
    pub history_page_button_padding: Margins,
    /// `st::history_comments_button_height`
    pub history_comments_button_height: i32,
    /// `st::history_comments_skip_left`
    pub history_comments_skip_left: i32,
    /// `st::history_comments_skip_text`
    pub history_comments_skip_text: i32,
    /// `st::history_comments_skip_right`
    pub history_comments_skip_right: i32,
    /// `st::history_comments_userpics.size`
    pub history_comments_userpic_size: i32,
    /// `st::history_comments_userpics.shift`
    pub history_comments_userpic_shift: i32,

    /// `st::reaction_info_size`
    pub reaction_info_size: i32,
    /// `st::reaction_info_skip`
    pub reaction_info_skip: i32,
    /// `st::reaction_info_between`
    pub reaction_info_between: i32,
    /// `st::effect_info_image`
    pub effect_info_image: i32,

    // Polls & contacts
    /// `st::contacts_photo_size`
    pub contacts_photo_size: i32,
    /// `st::history_poll_question_top`
    pub history_poll_question_top: i32,
    /// `st::history_poll_subtitle_skip`
    pub history_poll_subtitle_skip: i32,
    /// `st::history_poll_answer_padding`
    pub history_poll_answer_padding: Margins,
    /// `st::history_poll_answer_padding_no_media`
    pub history_poll_answer_padding_no_media: Margins,
    /// `st::history_poll_answers_skip`
    pub history_poll_answers_skip: i32,
    /// `st::history_poll_total_votes_skip`
    pub history_poll_total_votes_skip: i32,
    /// `st::history_poll_bottom_button_skip`
    pub history_poll_bottom_button_skip: i32,

    // Dialogs list layout constants
    /// `st::column_minimal_width_left`
    pub column_minimal_width_left: i32,
    /// `st::dialogs_row_height`
    pub dialogs_row_height: i32,
    /// `st::dialogs_unread_height`
    pub dialogs_unread_height: i32,
    /// `st::dialogs_unread_padding`
    pub dialogs_unread_padding: i32,
    /// `st::dialogs_date_skip`
    pub dialogs_date_skip: i32,
    /// `st::dialogs_important_bar_height`
    pub dialogs_important_bar_height: i32,
    /// `st::dialogs_text_width_min`
    pub dialogs_text_width_min: i32,
    /// `st::dialogs_chat_type_skip`
    pub dialogs_chat_type_skip: i32,
    /// `st::dialogs_send_state_skip`
    pub dialogs_send_state_skip: i32,
    /// `st::dialogs_mute_icon_skip`
    pub dialogs_mute_icon_skip: i32,
    /// `st::dialogs_top_bar_left_padding`
    pub dialogs_top_bar_left_padding: i32,
    /// `st::dialogs_online_badge_stroke`
    pub dialogs_online_badge_stroke: i32,
    /// `st::dialogs_online_badge_size`
    pub dialogs_online_badge_size: i32,
    /// `st::dialogs_online_badge_skip`
    pub dialogs_online_badge_skip: Point,
    /// `st::dialogs_call_badge_size`
    pub dialogs_call_badge_size: i32,
    /// `st::dialogs_call_badge_skip`
    pub dialogs_call_badge_skip: Point,
    /// `st::dialogs_ttl_badge_size`
    pub dialogs_ttl_badge_size: i32,
    /// `st::dialogs_ttl_badge_skip`
    pub dialogs_ttl_badge_skip: Point,
}

impl LayoutConsts {
    /// Returns the unscaled 100% defaults matching the `pub const` values in this module.
    #[must_use]
    pub const fn unscaled_defaults() -> Self {
        Self {
            fsize: FSIZE,
            emoji_size: EMOJI_SIZE,
            line_width: LINE_WIDTH,
            round_radius_large: ROUND_RADIUS_LARGE,
            round_radius_small: ROUND_RADIUS_SMALL,
            active_fade_in_duration_ms: ACTIVE_FADE_IN_DURATION_MS,
            active_fade_out_duration_ms: ACTIVE_FADE_OUT_DURATION_MS,
            fade_wrap_duration_ms: FADE_WRAP_DURATION_MS,
            location_size: LOCATION_SIZE,

            msg_max_width: MSG_MAX_WIDTH,
            msg_service_photo_width: MSG_SERVICE_PHOTO_WIDTH,
            msg_min_width: MSG_MIN_WIDTH,
            msg_photo_size: MSG_PHOTO_SIZE,
            msg_photo_skip: MSG_PHOTO_SKIP,
            msg_padding: MSG_PADDING,
            msg_tag_badge_padding: MSG_TAG_BADGE_PADDING,
            msg_margin: MSG_MARGIN,
            msg_margin_top_attached: MSG_MARGIN_TOP_ATTACHED,
            msg_shadow: MSG_SHADOW,

            history_reply_top: HISTORY_REPLY_TOP,
            history_reply_bottom: HISTORY_REPLY_BOTTOM,
            history_reply_preview: HISTORY_REPLY_PREVIEW,
            history_reply_preview_margin: HISTORY_REPLY_PREVIEW_MARGIN,
            history_reply_padding: HISTORY_REPLY_PADDING,
            msg_reply_padding: MSG_REPLY_PADDING,
            msg_reply_bar_skip: MSG_REPLY_BAR_SKIP,
            msg_service_padding: MSG_SERVICE_PADDING,
            msg_service_margin: MSG_SERVICE_MARGIN,

            msg_date_space: MSG_DATE_SPACE,
            msg_date_delta: MSG_DATE_DELTA,
            msg_date_img_delta: MSG_DATE_IMG_DELTA,
            msg_date_img_padding: MSG_DATE_IMG_PADDING,

            history_page_preview_padding: HISTORY_PAGE_PREVIEW_PADDING,

            media_caption_skip: MEDIA_CAPTION_SKIP,
            media_in_bubble_skip: MEDIA_IN_BUBBLE_SKIP,
            media_unread_size: MEDIA_UNREAD_SIZE,
            media_unread_skip: MEDIA_UNREAD_SKIP,

            min_photo_size: MIN_PHOTO_SIZE,
            max_media_size: MAX_MEDIA_SIZE,
            max_sticker_size: MAX_STICKER_SIZE,
            max_animated_emoji_size: MAX_ANIMATED_EMOJI_SIZE,
            max_gif_size: MAX_GIF_SIZE,
            max_video_message_size: MAX_VIDEO_MESSAGE_SIZE,
            max_signature_size: MAX_SIGNATURE_SIZE,
            history_minimal_width: HISTORY_MINIMAL_WIDTH,
            history_padding_bottom: HISTORY_PADDING_BOTTOM,

            history_send_state_space: HISTORY_SEND_STATE_SPACE,
            history_send_state_position: HISTORY_SEND_STATE_POSITION,
            history_shortcut_state_space: HISTORY_SHORTCUT_STATE_SPACE,
            history_views_space: HISTORY_VIEWS_SPACE,
            history_views_width: HISTORY_VIEWS_WIDTH,
            history_pin_width: HISTORY_PIN_WIDTH,
            history_silent_width: HISTORY_SILENT_WIDTH,
            history_ephemeral_state_width: HISTORY_EPHEMERAL_STATE_WIDTH,

            topic_button_skip: TOPIC_BUTTON_SKIP,
            topic_button_padding: TOPIC_BUTTON_PADDING,
            topic_button_arrow_skip: TOPIC_BUTTON_ARROW_SKIP,

            msg_bot_kb_button_margin: MSG_BOT_KB_BUTTON_MARGIN,
            msg_bot_kb_button_padding: MSG_BOT_KB_BUTTON_PADDING,
            msg_bot_kb_button_height: MSG_BOT_KB_BUTTON_HEIGHT,

            bubble_radius_small: BUBBLE_RADIUS_SMALL,
            bubble_radius_large: BUBBLE_RADIUS_LARGE,

            history_photo_left: HISTORY_PHOTO_LEFT,
            history_photo_bubble_min_width: HISTORY_PHOTO_BUBBLE_MIN_WIDTH,
            history_psa_icon_skip1: HISTORY_PSA_ICON_SKIP1,

            history_unread_bar_height: HISTORY_UNREAD_BAR_HEIGHT,
            history_unread_bar_margin: HISTORY_UNREAD_BAR_MARGIN,

            history_call_width: HISTORY_CALL_WIDTH,
            history_call_height: HISTORY_CALL_HEIGHT,
            history_call_icon_position: HISTORY_CALL_ICON_POSITION,
            history_call_left: HISTORY_CALL_LEFT,
            history_call_top: HISTORY_CALL_TOP,
            history_call_status_top: HISTORY_CALL_STATUS_TOP,
            history_call_status_skip: HISTORY_CALL_STATUS_SKIP,
            history_call_icon_skip: HISTORY_CALL_ICON_SKIP,

            msg_file_layout: MSG_FILE_LAYOUT,
            msg_file_thumb_layout: MSG_FILE_THUMB_LAYOUT,
            msg_file_layout_grouped: MSG_FILE_LAYOUT_GROUPED,
            msg_file_thumb_layout_grouped: MSG_FILE_THUMB_LAYOUT_GROUPED,
            msg_file_min_width: MSG_FILE_MIN_WIDTH,
            msg_file_top_minus: MSG_FILE_TOP_MINUS,

            msg_waveform_bar: MSG_WAVEFORM_BAR,
            msg_waveform_skip: MSG_WAVEFORM_SKIP,
            msg_waveform_min: MSG_WAVEFORM_MIN,
            msg_waveform_max: MSG_WAVEFORM_MAX,

            history_transcribe_skip: HISTORY_TRANSCRIBE_SKIP,
            history_transcribe_size: HISTORY_TRANSCRIBE_SIZE,

            history_fast_share_size: HISTORY_FAST_SHARE_SIZE,
            history_fast_share_left: HISTORY_FAST_SHARE_LEFT,
            history_fast_share_bottom: HISTORY_FAST_SHARE_BOTTOM,

            history_group_width_max: HISTORY_GROUP_WIDTH_MAX,
            history_group_width_min: HISTORY_GROUP_WIDTH_MIN,
            history_group_skip: HISTORY_GROUP_SKIP,

            web_page_photo_delta: WEB_PAGE_PHOTO_DELTA,
            history_page_button_height: HISTORY_PAGE_BUTTON_HEIGHT,
            history_page_button_padding: HISTORY_PAGE_BUTTON_PADDING,
            history_comments_button_height: HISTORY_COMMENTS_BUTTON_HEIGHT,
            history_comments_skip_left: HISTORY_COMMENTS_SKIP_LEFT,
            history_comments_skip_text: HISTORY_COMMENTS_SKIP_TEXT,
            history_comments_skip_right: HISTORY_COMMENTS_SKIP_RIGHT,
            history_comments_userpic_size: HISTORY_COMMENTS_USERPIC_SIZE,
            history_comments_userpic_shift: HISTORY_COMMENTS_USERPIC_SHIFT,

            reaction_info_size: REACTION_INFO_SIZE,
            reaction_info_skip: REACTION_INFO_SKIP,
            reaction_info_between: REACTION_INFO_BETWEEN,
            effect_info_image: EFFECT_INFO_IMAGE,

            contacts_photo_size: CONTACTS_PHOTO_SIZE,
            history_poll_question_top: HISTORY_POLL_QUESTION_TOP,
            history_poll_subtitle_skip: HISTORY_POLL_SUBTITLE_SKIP,
            history_poll_answer_padding: HISTORY_POLL_ANSWER_PADDING,
            history_poll_answer_padding_no_media: HISTORY_POLL_ANSWER_PADDING_NO_MEDIA,
            history_poll_answers_skip: HISTORY_POLL_ANSWERS_SKIP,
            history_poll_total_votes_skip: HISTORY_POLL_TOTAL_VOTES_SKIP,
            history_poll_bottom_button_skip: HISTORY_POLL_BOTTOM_BUTTON_SKIP,

            column_minimal_width_left: COLUMN_MINIMAL_WIDTH_LEFT,
            dialogs_row_height: DIALOGS_ROW_HEIGHT,
            dialogs_unread_height: DIALOGS_UNREAD_HEIGHT,
            dialogs_unread_padding: DIALOGS_UNREAD_PADDING,
            dialogs_date_skip: DIALOGS_DATE_SKIP,
            dialogs_important_bar_height: DIALOGS_IMPORTANT_BAR_HEIGHT,
            dialogs_text_width_min: DIALOGS_TEXT_WIDTH_MIN,
            dialogs_chat_type_skip: DIALOGS_CHAT_TYPE_SKIP,
            dialogs_send_state_skip: DIALOGS_SEND_STATE_SKIP,
            dialogs_mute_icon_skip: DIALOGS_MUTE_ICON_SKIP,
            dialogs_top_bar_left_padding: DIALOGS_TOP_BAR_LEFT_PADDING,
            dialogs_online_badge_stroke: DIALOGS_ONLINE_BADGE_STROKE,
            dialogs_online_badge_size: DIALOGS_ONLINE_BADGE_SIZE,
            dialogs_online_badge_skip: DIALOGS_ONLINE_BADGE_SKIP,
            dialogs_call_badge_size: DIALOGS_CALL_BADGE_SIZE,
            dialogs_call_badge_skip: DIALOGS_CALL_BADGE_SKIP,
            dialogs_ttl_badge_size: DIALOGS_TTL_BADGE_SIZE,
            dialogs_ttl_badge_skip: DIALOGS_TTL_BADGE_SKIP,
        }
    }

    /// Reads all layout constants from `tg_style::st::*` scaled at `scale`.
    #[must_use]
    pub fn from_style(scale: tg_style::Scale) -> Self {
        use tg_style::st;
        Self {
            fsize: st::fsize.scaled_at(scale),
            emoji_size: st::emoji_size.scaled_at(scale),
            line_width: st::line_width.scaled_at(scale),
            round_radius_large: st::round_radius_large.scaled_at(scale),
            round_radius_small: st::round_radius_small.scaled_at(scale),
            active_fade_in_duration_ms: i64::from(st::active_fade_in_duration),
            active_fade_out_duration_ms: i64::from(st::active_fade_out_duration),
            fade_wrap_duration_ms: i64::from(st::fade_wrap_duration),
            location_size: scale_size(st::location_size, scale),

            msg_max_width: st::msg_max_width.scaled_at(scale),
            msg_service_photo_width: st::msg_service_photo_width.scaled_at(scale),
            msg_min_width: st::msg_min_width.scaled_at(scale),
            msg_photo_size: st::msg_photo_size.scaled_at(scale),
            msg_photo_skip: st::msg_photo_skip.scaled_at(scale),
            msg_padding: scale_margins(st::msg_padding, scale),
            msg_tag_badge_padding: scale_margins(st::msg_tag_badge_padding, scale),
            msg_margin: scale_margins(st::msg_margin, scale),
            msg_margin_top_attached: st::msg_margin_top_attached.scaled_at(scale),
            msg_shadow: st::msg_shadow.scaled_at(scale),

            history_reply_top: st::history_reply_top.scaled_at(scale),
            history_reply_bottom: st::history_reply_bottom.scaled_at(scale),
            history_reply_preview: st::history_reply_preview.scaled_at(scale),
            history_reply_preview_margin: scale_margins(st::history_reply_preview_margin, scale),
            history_reply_padding: scale_margins(st::history_reply_padding, scale),
            msg_reply_padding: scale_margins(st::msg_reply_padding, scale),
            msg_reply_bar_skip: st::msg_reply_bar_skip.scaled_at(scale),
            msg_service_padding: scale_margins(st::msg_service_padding, scale),
            msg_service_margin: scale_margins(st::msg_service_margin, scale),

            msg_date_space: st::msg_date_space.scaled_at(scale),
            msg_date_delta: scale_point(st::msg_date_delta, scale),
            msg_date_img_delta: st::msg_date_img_delta.scaled_at(scale),
            msg_date_img_padding: scale_point(st::msg_date_img_padding, scale),

            history_page_preview_padding: scale_margins(st::history_page_preview.padding, scale),

            media_caption_skip: st::media_caption_skip.scaled_at(scale),
            media_in_bubble_skip: st::media_in_bubble_skip.scaled_at(scale),
            media_unread_size: st::media_unread_size.scaled_at(scale),
            media_unread_skip: st::media_unread_skip.scaled_at(scale),

            min_photo_size: st::min_photo_size.scaled_at(scale),
            max_media_size: st::max_media_size.scaled_at(scale),
            max_sticker_size: st::max_sticker_size.scaled_at(scale),
            max_animated_emoji_size: st::max_animated_emoji_size.scaled_at(scale),
            max_gif_size: st::max_gif_size.scaled_at(scale),
            max_video_message_size: st::max_video_message_size.scaled_at(scale),
            max_signature_size: st::max_signature_size.scaled_at(scale),
            history_minimal_width: st::history_minimal_width.scaled_at(scale),
            history_padding_bottom: st::history_padding_bottom.scaled_at(scale),

            history_send_state_space: st::history_send_state_space.scaled_at(scale),
            history_send_state_position: scale_point(st::history_send_state_position, scale),
            history_shortcut_state_space: st::history_shortcut_state_space.scaled_at(scale),
            history_views_space: st::history_views_space.scaled_at(scale),
            history_views_width: st::history_views_width.scaled_at(scale),
            history_pin_width: st::history_pin_width.scaled_at(scale),
            history_silent_width: st::history_silent_width.scaled_at(scale),
            history_ephemeral_state_width: st::history_ephemeral_state_width.scaled_at(scale),

            topic_button_skip: st::topic_button_skip.scaled_at(scale),
            topic_button_padding: scale_margins(st::topic_button_padding, scale),
            topic_button_arrow_skip: st::topic_button_arrow_skip.scaled_at(scale),

            msg_bot_kb_button_margin: st::msg_bot_kb_button.margin.scaled_at(scale),
            msg_bot_kb_button_padding: st::msg_bot_kb_button.padding.scaled_at(scale),
            msg_bot_kb_button_height: st::msg_bot_kb_button.height.scaled_at(scale),

            bubble_radius_small: st::bubble_radius_small.scaled_at(scale),
            bubble_radius_large: st::bubble_radius_large.scaled_at(scale),

            history_photo_left: st::history_photo_left.scaled_at(scale),
            history_photo_bubble_min_width: st::history_photo_bubble_min_width.scaled_at(scale),
            history_psa_icon_skip1: st::history_psa_icon_skip1.scaled_at(scale),

            history_unread_bar_height: st::history_unread_bar_height.scaled_at(scale),
            history_unread_bar_margin: st::history_unread_bar_margin.scaled_at(scale),

            history_call_width: st::history_call_width.scaled_at(scale),
            history_call_height: st::history_call_height.scaled_at(scale),
            history_call_icon_position: scale_point(st::history_call_icon_position, scale),
            history_call_left: st::history_call_left.scaled_at(scale),
            history_call_top: st::history_call_top.scaled_at(scale),
            history_call_status_top: st::history_call_status_top.scaled_at(scale),
            history_call_status_skip: st::history_call_status_skip.scaled_at(scale),
            history_call_icon_skip: st::history_call_icon_skip.scaled_at(scale),

            msg_file_layout: FileLayoutStyle::from_style(&st::msg_file_layout, scale),
            msg_file_thumb_layout: FileLayoutStyle::from_style(&st::msg_file_thumb_layout, scale),
            msg_file_layout_grouped: FileLayoutStyle::from_style(
                &st::msg_file_layout_grouped,
                scale,
            ),
            msg_file_thumb_layout_grouped: FileLayoutStyle::from_style(
                &st::msg_file_thumb_layout_grouped,
                scale,
            ),
            msg_file_min_width: st::msg_file_min_width.scaled_at(scale),
            msg_file_top_minus: st::msg_file_top_minus.scaled_at(scale),

            msg_waveform_bar: st::msg_waveform_bar.scaled_at(scale),
            msg_waveform_skip: st::msg_waveform_skip.scaled_at(scale),
            msg_waveform_min: st::msg_waveform_min.scaled_at(scale),
            msg_waveform_max: st::msg_waveform_max.scaled_at(scale),

            history_transcribe_skip: st::history_transcribe_skip.scaled_at(scale),
            history_transcribe_size: scale_size(st::history_transcribe_size, scale),

            history_fast_share_size: st::history_fast_share_size.scaled_at(scale),
            history_fast_share_left: st::history_fast_share_left.scaled_at(scale),
            history_fast_share_bottom: st::history_fast_share_bottom.scaled_at(scale),

            history_group_width_max: st::history_group_width_max.scaled_at(scale),
            history_group_width_min: st::history_group_width_min.scaled_at(scale),
            history_group_skip: st::history_group_skip.scaled_at(scale),

            web_page_photo_delta: st::web_page_photo_delta.scaled_at(scale),
            history_page_button_height: st::history_page_button_height.scaled_at(scale),
            history_page_button_padding: scale_margins(st::history_page_button_padding, scale),
            history_comments_button_height: st::history_comments_button_height.scaled_at(scale),
            history_comments_skip_left: st::history_comments_skip_left.scaled_at(scale),
            history_comments_skip_text: st::history_comments_skip_text.scaled_at(scale),
            history_comments_skip_right: st::history_comments_skip_right.scaled_at(scale),
            history_comments_userpic_size: st::history_comments_userpics.size.scaled_at(scale),
            history_comments_userpic_shift: st::history_comments_userpics.shift.scaled_at(scale),

            reaction_info_size: st::reaction_info_size.scaled_at(scale),
            reaction_info_skip: st::reaction_info_skip.scaled_at(scale),
            reaction_info_between: st::reaction_info_between.scaled_at(scale),
            effect_info_image: st::effect_info_image.scaled_at(scale),

            contacts_photo_size: st::contacts_photo_size.scaled_at(scale),
            history_poll_question_top: st::history_poll_question_top.scaled_at(scale),
            history_poll_subtitle_skip: st::history_poll_subtitle_skip.scaled_at(scale),
            history_poll_answer_padding: scale_margins(st::history_poll_answer_padding, scale),
            history_poll_answer_padding_no_media: scale_margins(
                st::history_poll_answer_padding_no_media,
                scale,
            ),
            history_poll_answers_skip: st::history_poll_answers_skip.scaled_at(scale),
            history_poll_total_votes_skip: st::history_poll_total_votes_skip.scaled_at(scale),
            history_poll_bottom_button_skip: st::history_poll_bottom_button_skip.scaled_at(scale),

            column_minimal_width_left: st::column_minimal_width_left.scaled_at(scale),
            dialogs_row_height: st::dialogs_row_height.scaled_at(scale),
            dialogs_unread_height: st::dialogs_unread_height.scaled_at(scale),
            dialogs_unread_padding: st::dialogs_unread_padding.scaled_at(scale),
            dialogs_date_skip: st::dialogs_date_skip.scaled_at(scale),
            dialogs_important_bar_height: st::dialogs_important_bar_height.scaled_at(scale),
            dialogs_text_width_min: st::dialogs_text_width_min.scaled_at(scale),
            dialogs_chat_type_skip: st::dialogs_chat_type_skip.scaled_at(scale),
            dialogs_send_state_skip: st::dialogs_send_state_skip.scaled_at(scale),
            dialogs_mute_icon_skip: st::dialogs_mute_icon_skip.scaled_at(scale),
            dialogs_top_bar_left_padding: st::dialogs_top_bar_left_padding.scaled_at(scale),
            dialogs_online_badge_stroke: st::dialogs_online_badge_stroke.scaled_at(scale),
            dialogs_online_badge_size: st::dialogs_online_badge_size.scaled_at(scale),
            dialogs_online_badge_skip: scale_point(st::dialogs_online_badge_skip, scale),
            dialogs_call_badge_size: st::dialogs_call_badge_size.scaled_at(scale),
            dialogs_call_badge_skip: scale_point(st::dialogs_call_badge_skip, scale),
            dialogs_ttl_badge_size: st::dialogs_ttl_badge_size.scaled_at(scale),
            dialogs_ttl_badge_skip: scale_point(st::dialogs_ttl_badge_skip, scale),
        }
    }

    /// Reads all layout constants from `tg_style::st::*` using the current global DPI scale.
    #[must_use]
    pub fn from_current_style() -> Self {
        Self::from_style(tg_style::scale())
    }
}

impl Default for LayoutConsts {
    fn default() -> Self {
        Self::unscaled_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::DialogRowStyle;
    use tg_style::{Scale, st};

    #[test]
    fn defaults_equal_tg_style_100_percent() {
        assert_eq!(
            LayoutConsts::default(),
            LayoutConsts::from_style(Scale::S100)
        );

        assert_eq!(
            DialogRowStyle::from_style(&st::default_dialog_row, Scale::S100),
            DialogRowStyle::DEFAULT
        );
        assert_eq!(
            DialogRowStyle::from_style(&st::tagged_dialog_row, Scale::S100),
            DialogRowStyle::TAGGED
        );
        assert_eq!(
            DialogRowStyle::from_style(&st::forum_dialog_row, Scale::S100),
            DialogRowStyle::FORUM
        );
        assert_eq!(
            DialogRowStyle::from_style(&st::tagged_forum_dialog_row, Scale::S100),
            DialogRowStyle::TAGGED_FORUM
        );
        assert_eq!(
            DialogRowStyle::from_style(&st::forum_topic_row, Scale::S100),
            DialogRowStyle::FORUM_TOPIC
        );
    }

    #[test]
    fn from_style_applies_dpi_scaling() {
        let s200 = LayoutConsts::from_style(Scale::S200);
        assert_eq!(s200.msg_max_width, MSG_MAX_WIDTH * 2);
        assert_eq!(s200.dialogs_row_height, DIALOGS_ROW_HEIGHT * 2);
        assert_eq!(
            s200.msg_padding,
            Margins::new(
                MSG_PADDING.left * 2,
                MSG_PADDING.top * 2,
                MSG_PADDING.right * 2,
                MSG_PADDING.bottom * 2,
            )
        );
        assert_eq!(
            s200.location_size,
            Size::new(LOCATION_SIZE.width * 2, LOCATION_SIZE.height * 2)
        );
        // Durations are unaffected by DPI scaling.
        assert_eq!(s200.active_fade_in_duration_ms, ACTIVE_FADE_IN_DURATION_MS);
        assert_eq!(
            s200.active_fade_out_duration_ms,
            ACTIVE_FADE_OUT_DURATION_MS
        );
        assert_eq!(s200.fade_wrap_duration_ms, FADE_WRAP_DURATION_MS);
    }
}
