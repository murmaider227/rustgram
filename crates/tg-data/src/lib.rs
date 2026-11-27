//! `tg-data`: TL-independent domain model for Rustgram (Rust port of Telegram Desktop).
//!
//! Ports the model layer of `Telegram/SourceFiles/data/`, `dialogs/`, `history/`,
//! `storage/storage_sparse_ids_list.*`, and `api/api_updates.*` (state machine).

pub mod changes;
pub mod dialogs;
pub mod forum;
pub mod history;
pub mod id;
pub mod lastseen;
pub mod message;
pub mod peer;
pub mod rights;
pub mod saved;
pub mod scheduled;
pub mod session;
pub mod text;
pub mod updates;

pub use changes::{
    ChangeEvent, Changes, EntryUpdateFlags, HistoryUpdateFlags, MessageUpdateFlags,
    PeerUpdateFlags, TopicUpdateFlags, TypingAction,
};
pub use dialogs::{
    ARCHIVE_FIX_ON_TOP_INDEX, BadgeSettings, BadgesState, ChatFilter, ChatFilterFlags, ChatFilters,
    CountInBadge, DialogKey, Folder, IncludeInBadge, IndexedList, MainList, PinnedList, SortMode,
    TOP_PROMOTION_FIX_ON_TOP_INDEX, UnreadState, badges_for_unread, compute_dialog_sort_key,
    dialog_pos_from_date, fixed_on_top_dialog_pos, pinned_dialog_pos, unread_on_top_dialog_pos,
};
pub use forum::{
    FORUM_GENERAL_TOPIC_ID, FORUM_TOPIC_COLOR_IDS, ForumOffsets, ForumState, ForumTopicData,
    TOPICS_FIRST_LOAD, TOPICS_PER_PAGE, TOPICS_STALE_PER_REQUEST,
};
pub use history::{
    Draft, DraftKey, History, IdSlice, InboxReadOutcome, SparseIdsList, SparseIdsSnapshot,
    WebPageDraft,
};
pub use id::{
    ARCHIVE_FOLDER_ID, BareId, BotAppId, BusinessShortcutId, CANCELLED_WEB_PAGE_ID, CLIENT_MSG_IDS,
    COLOR_INDEX_COUNT, CallId, ChannelId, ChatId, CollectibleId, DocumentId, END_CLIENT_MSG_ID,
    END_STORY_MSG_ID, EffectId, FakeChatId, FilterId, FolderId, FullMsgId, FullStoryId, GameId,
    GlobalMsgId, MAX_DATE, MIN_DATE, MessageGroupId, MessagePosition, MsgId, MsgRange, PeerId,
    PhotoId, PollId, SCHEDULED_MAX_MSG_ID, SEARCH_QUERY_OFFSET_HINT, SERVER_MAX_MSG_ID,
    SERVER_MAX_STORY_ID, SHORTCUT_MAX_MSG_ID, SHOW_AND_MAYBE_START_BOT_MSG_ID,
    SHOW_AND_START_BOT_MSG_ID, SHOW_AT_THE_END_MSG_ID, SHOW_AT_UNREAD_MSG_ID,
    SHOW_FOR_CHOOSE_MESSAGES_MSG_ID, SIMPLE_COLOR_INDEX_COUNT, SPECIAL_MSG_ID_SHIFT,
    START_CLIENT_MSG_ID, START_STORY_MSG_ID, STORY_MSG_IDS, SWITCH_AT_TOP_MSG_ID, StoryId, TimeId,
    UNKNOWN_PHOTO_ID, UserId, WELCOME_MAX_MSG_ID, WallPaperId, WebPageId,
    color_index_to_palette_index, decide_color_index, fake_peer_id_for_just_name,
};
pub use lastseen::{
    LIFE_START_DATE, LastseenStatus, OnlineStatusText, SET_ONLINE_AFTER_ACTIVITY, UserStatusInput,
    online_change_timeout_ms, sort_by_online_status,
};
pub use message::{
    ButtonColor, ButtonVisual, ContactData, DiceData, DocumentData, DocumentKind, DocumentType,
    ExtendedMedia, ForwardInfo, GameData, GiveawayData, GiveawayResultsData, InlineQueryPeerTypes,
    InvoiceData, KeyboardButton, KeyboardButtonKind, LiveLocationData, MediaWebPageFlags, Message,
    MessageContent, MessageFlags, MessageMedia, MessageReactionCount, MessageReactionsSummary,
    PaidMediaData, PhoneCallDiscardReason, PhotoData, PhotoSize, PollAnswer, PollAnswerVoters,
    PollData, PollMedia, PollRequestRestriction, PollResults, RecentReactor, ReplyInfo,
    ReplyMarkup, RequestPeerQuery, RequestPeerRestriction, RequestPeerType, ServiceAction,
    StarsAmount, StickerMeta, StoryMediaData, TodoCompletionData, TodoItemData, TodoListData,
    VenueData, VideoStreamData, WebDocumentData, WebPageData, WidgetData,
};
pub use peer::{
    AllowedReactions, AllowedReactionsType, Birthday, BlockStatus, BotCommand, BotInfo, BotStatus,
    BotVerifyDetails, CallsStatus, Channel, ChannelFlags, ChannelLocation, Chat, ChatFlags,
    ChatUpdateVersionStatus, ColorCollectibleAccent, ContactStatus, EmojiStatus, GeoPoint,
    LoadedStatus, MegagroupInfo, Peer, PeerBarSettings, PeerCommon, PeerNotifySettings, ProfileTab,
    ReactionId, StarsRating, StoriesState, UnavailableReason, User, UserFlags, Username,
    UsernamesInfo,
};
pub use rights::{
    ChatAdminRights, ChatBannedRights, ChatRestrictions, ChatRestrictionsInfo,
    RESTRICT_UNTIL_FOREVER, RestrictionCheckResult, SendCheckContext, is_restricted_forever,
};
pub use saved::{
    MY_TAGS_REQUEST_TIMEOUT_MS, SAVED_LIST_FIRST_PER_PAGE, SAVED_LIST_PER_PAGE,
    SAVED_MESSAGES_PER_PAGE, SAVED_STALE_PER_REQUEST, SavedDialogsOffset, SavedMessagesState,
    SavedReactionTagData, SavedReactionTagsBucket, SavedReactionTagsState, SavedSublistData,
};
pub use scheduled::{
    DeletedScheduledInfo, QuickReplyShortcutData, SCHEDULED_REQUEST_TIME_LIMIT_MS,
    SCHEDULED_UNTIL_ONLINE_TIMESTAMP, SHORTCUT_REQUEST_TIME_LIMIT_MS, ScheduledMessagesStore,
    ShortcutIdChange, ShortcutMessagesStore, ShortcutRenameCollision, count_hash,
    has_scheduled_date, hash_finalize, hash_init, hash_update_i32, hash_update_i64,
    hash_update_u32, hash_update_u64, scheduled_local_to_remote_id, scheduled_remote_to_local_id,
    shortcut_local_to_remote_id, shortcut_remote_to_local_id,
};
pub use session::{DefaultNotifyScope, Session};
pub use text::{EntityKind, FormattedDateFlags, TextEntity, TextWithEntities};
pub use updates::{
    CHANNEL_GET_DIFF_LIMIT, ChannelUpdateEnvelope, CommonUpdateEnvelope,
    FAIL_DIFF_INITIAL_BACKOFF_SECS, FAIL_DIFF_MAX_BACKOFF_SECS, FAST_GAP_TIMEOUT_MS,
    NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS, NO_UPDATES_TIMEOUT_MS, PersistedUpdatesState,
    PtsIngestResult, PtsWaiter, UpdateAction, UpdatesStateMachine, WAIT_FOR_CHANNEL_GET_DIFF_MS,
    WAIT_FOR_SKIPPED_TIMEOUT_MS,
};
