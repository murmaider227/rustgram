//! `tg-api`: high-level Telegram API layer (port of Telegram Desktop's `ApiWrap`,
//! `Telegram/SourceFiles/api/*`, `Telegram/SourceFiles/intro/*`, and TL <-> `tg-data`
//! conversions from `data/data_session.cpp`, `data/data_media_types.cpp`, and
//! `history/history_item.cpp`).
//!
//! Built directly on generated `tg-tl` types, `tg-data` domain models, `tg-crypto::srp`,
//! `tg-lang`, and a `tg_tl::RemoteCall`-generic [`RpcClient`] trait so wiring `tg-mtproto`
//! is a one-line implementation.

pub mod account;
pub mod cached_lists;
pub mod channel_mgmt;
pub mod channels;
pub mod contacts;
pub mod contacts_extras;
pub mod convert;
pub mod dialogs;
pub mod error;
pub mod file_origin;
pub mod forum;
pub mod hash;
pub mod history;
pub mod invoker;
pub mod lang;
pub mod login;
pub mod messaging;
pub mod rpc;
pub mod saved;
pub mod saved_side_log;
pub mod scheduled;
pub mod search;
pub mod stickers;
pub mod types;
pub mod updates;

pub use account::{
    AUTHORIZATION_AUTOCONFIRM_FALLBACK_SECS, AccountSettingsManager, AuthorizationEntry,
    AuthorizationsManager, CloudPasswordManager, CloudPasswordRandom, ContentSettingsState,
    DefaultNotifyScope, DisallowedGifts, FullNotifySettings, GlobalPrivacyState,
    MIN_USERNAME_LENGTH, MuteValue, NotifySettingsManager, NotifySound, NotifyTarget,
    ResetPasswordOutcome, SetPasswordOutcome, TDESKTOP_API_ID, TDESKTOP_SNAP_API_ID,
    TDESKTOP_TEST_API_ID, UnarchiveOnNewMessage, UnreviewedAuthorization, UsernameCheckResult,
    WebAuthorizationEntry, WebAuthorizationsManager, modpow_algo_to_tl, parse_secure_secret_algo,
    secure_secret_algo_to_tl, skip_add_notify_exception,
};
pub use cached_lists::{
    APP_CONFIG_REFRESH_MS, AppConfigSync, AppConfigValue, AttachMenuBotsSync, CLOUD_THEME_FORMAT,
    CachedAttachMenuBot, CachedAvailableReaction, CachedEmojiGroup, CachedEmojiStatusEntry,
    CachedListsSync, CachedPeerColorOption, CachedPremiumPeriodOption, CachedQuickReply,
    CachedTheme, CachedWallPaper, CloudThemesSync, EMOJI_KEYWORDS_REFRESH_MS,
    EMOJI_STATUSES_REFRESH_MS, EmojiGroupsCache, EmojiKeywordsPack, EmojiKeywordsSync,
    EmojiStatusesSync, MY_TAGS_DEBOUNCE_MS, PEER_COLORS_REFRESH_MS, PeerColorsSync,
    PremiumPromoSync, QUICK_REPLIES_REFRESH_LIMIT_MS, QuickRepliesSync,
    REACTIONS_FULL_LIST_REFRESH_MS, RECENT_EMOJI_STATUSES_DEBOUNCE_MS,
    RECENT_REACTIONS_DEBOUNCE_MS, RECENT_REACTIONS_LIMIT, ReactionsSync, SavedRingtonesSync,
    TOP_REACTIONS_DELAY_MS, TOP_REACTIONS_LIMIT, WallPapersSync,
};
pub use channel_mgmt::{
    ChannelStickerSetInput, ChannelUsernameCheckResult, ChatAdminWithInvitesInfo,
    DeleteChatOutcome, apply_deactivate_all_usernames_to_session, build_check_channel_username,
    build_convert_to_gigagroup, build_deactivate_all_usernames, build_delete_chat,
    build_get_admins_with_invites, build_get_groups_for_discussion,
    build_set_boosts_to_unblock_restrictions, build_set_emoji_stickers, build_set_stickers,
    build_toggle_autotranslation, build_toggle_forum, build_toggle_participants_hidden,
    build_toggle_pre_history_hidden, build_toggle_view_forum_as_messages,
    build_update_paid_messages_price, check_channel_username, convert_to_gigagroup,
    deactivate_all_usernames, delete_chat, get_admins_with_invites, get_groups_for_discussion,
    is_chat_not_modified, is_rpc_error_name, set_boosts_to_unblock_restrictions,
    set_emoji_stickers, set_stickers, toggle_autotranslation, toggle_forum,
    toggle_participants_hidden, toggle_pre_history_hidden, toggle_view_forum_as_messages,
    update_paid_messages_price,
};
pub use channels::{
    ADMIN_LOG_FIRST_PAGE, ADMIN_LOG_PER_PAGE, AdminLogEventsFilter, AdminLogPage, AdminLogQuery,
    ChannelParticipantInfo, ChannelParticipantKind, ChannelParticipantsFilter,
    ChannelParticipantsPage, ChannelRecommendations, ChatInviteImporterInfo,
    ChatInviteImportersPage, ChatInviteImportersQuery, EXPORTED_INVITES_FIRST_PAGE,
    EXPORTED_INVITES_PER_PAGE, EditChatInviteOptions, EditChatInviteResult,
    ExportChatInviteOptions, ExportedChatInviteInfo, ExportedChatInvitesPage,
    ExportedChatInvitesQuery, INVITE_IMPORTERS_FIRST_PAGE, INVITE_IMPORTERS_PER_PAGE,
    PARTICIPANTS_PER_PAGE, api_hash_finalize, api_hash_init, api_hash_update_i32,
    api_hash_update_i64, api_hash_update_u32, api_hash_update_u64, kicked_restrictions_for_peer,
    normalize_ban_until_date,
};
pub use contacts::{
    BLOCKED_FIRST_SLICE, BLOCKED_PER_PAGE, BlockedPeerEntry, BlockedPeersManager,
    BlockedPeersSlice, ContactsFetchResult, ContactsManager, PrivacyExceptions, PrivacyKey,
    PrivacyOption, PrivacyRule, ResolvedUsername, UserPrivacyManager, privacy_key_from_tl,
    privacy_key_to_tl, privacy_rules_all_chats_loaded, privacy_rules_from_tl, privacy_rules_to_tl,
    resolve_username,
};
pub use contacts_extras::{
    BlockFromRepliesOptions, ContactBirthday, ContactStatusUpdate, ExportedContactToken,
    ExportedNoPaidMessagesToken, NoPaidMessagesTokenImport, SponsoredPeerEntry,
    UploadContactProfilePhotoParams, accept_contact, apply_contact_birthdays,
    apply_contact_statuses, apply_deleted_contacts_local, apply_deleted_photos_local,
    apply_sponsored_peers, apply_uploaded_contact_photo_local, block_from_replies,
    clear_peer_bar_settings, delete_contacts, delete_photos, export_contact_token,
    export_no_paid_messages_token, get_birthdays, get_sponsored_peers, get_statuses,
    hide_peer_settings_bar, hide_promo_data, import_contact_token, import_no_paid_messages_token,
    report_spam, set_blocked, toggle_top_peers, update_contact_note, upload_contact_profile_photo,
};
pub use dialogs::{
    DIALOGS_FIRST_LOAD, DIALOGS_PER_PAGE, DialogsLoader, DialogsPageResult, FolderDialogsState,
    LOAD_EXCEPTIONS_AFTER, LOAD_EXCEPTIONS_PER_REQUEST,
};
pub use error::{
    ApiError, LoginErrorKind, SendMessageErrorKind, map_login_error, map_send_message_error,
};
pub use file_origin::{
    FileOrigin, UpdatedFileReferences, apply_file_reference, refresh_file_reference,
    retry_once_on_file_reference_error,
};
pub use forum::{
    ForumTopicEdit, ForumTopicsPageResult, MAX_DELETE_TOPIC_HISTORY_LOOPS, apply_tl_forum_topic,
    apply_tl_forum_topics_response, create_forum_topic, delete_topic_history, edit_forum_topic,
    load_forum_topics_by_id, load_forum_topics_page, read_topic_discussion,
    reorder_pinned_forum_topics, update_pinned_forum_topic,
};
pub use hash::{
    count_contacts_hash, count_document_vector_hash, count_hash_i32, count_hash_i64,
    count_hash_u32, count_hash_u64, hash_finalize, hash_init, hash_update_i32, hash_update_i64,
    hash_update_u32, hash_update_u64,
};
pub use history::{
    HISTORY_LIMIT, HistoryLoader, HistorySlice, JumpToDateResult, MAX_OFFSET_ID,
    parse_history_result, prepare_history_request,
};
pub use invoker::{InvokeResult, Invoker};
pub use lang::{
    RpcLangCloudClient, countries_list_from_tl, fetch_countries_list, fetch_lang_pack,
    fetch_lang_pack_difference, fetch_language, fetch_languages, fetch_strings,
    lang_pack_difference_from_tl, lang_pack_difference_struct_from_tl, lang_pack_language_from_tl,
    lang_pack_language_struct_from_tl, lang_pack_string_from_tl, sync_countries_list,
};
pub use login::{
    CallStatus, CloudPasswordState, CodeInputState, DEFAULT_CALL_TIMEOUT_SECS,
    HANDLE_SRP_ID_INVALID_TIMEOUT, LoginStateMachine, LoginStep, PasswordCheckMode, QrLoginState,
    allow_phone_attempt, digits_only, encode_base64url, format_qr_login_url,
};
pub use messaging::{
    AlbumMediaItem, CANCEL_TYPING_ACTION_TIMEOUT, DEFAULT_MESSAGE_LENGTH_LIMIT,
    DELETE_HISTORY_RETRY_LIMIT, DeleteHistoryOutcome, DraftManager, FlushedRead, ForwardItem,
    ForwardOutcome, MessageSender, OsRandomIdSource, PendingMessage, PendingMessageState,
    PreparedTextChunk, READ_REQUEST_TIMEOUT, RandomIdSource, ReadHistoryManager,
    SAVE_CLOUD_DRAFT_TIMEOUT, SEND_MY_SPEAKING_INTERVAL, SEND_MY_TYPING_INTERVAL,
    SEND_TYPINGS_TO_OFFLINE_FOR_SECS, SendMediaOutcome, SendMessageOutcome, SendMultiMediaOutcome,
    SendProgressKind, SendProgressManager, SendProgressUpdate, TypingPeerInfo,
};
pub use rpc::{DcId, InvocationError, RpcClient, RpcError};
pub use saved::{
    MAX_DELETE_SAVED_HISTORY_LOOPS, SavedDialogsPageResult, apply_tl_saved_dialogs,
    delete_saved_history, get_saved_reaction_tags, load_pinned_saved_dialogs,
    load_saved_dialogs_by_id, load_saved_dialogs_page, load_saved_history,
    prepare_saved_history_request, read_saved_history, reorder_pinned_saved_dialogs,
    toggle_saved_dialog_pin, update_saved_reaction_tag,
};
pub use scheduled::{
    QuickRepliesOutcome, QuickReplyMessagesOutcome, ScheduledHistoryOutcome,
    delete_quick_reply_messages, delete_quick_reply_shortcut, delete_scheduled_messages,
    edit_quick_reply_shortcut, get_quick_replies, get_quick_reply_messages, get_scheduled_history,
    quick_reply_from_tl, send_quick_reply_messages, send_scheduled_messages,
};
pub use search::{
    DEFAULT_SEARCH_TIMEOUT_MS, FIRST_SHARED_MEDIA_LIMIT, FoundMessages, GlobalChatTypeFilter,
    GlobalSearchController, GlobalSearchPage, GlobalSearchParams, GlobalSearchScope, HashOrCashtag,
    InChatSearchRequest, MessagesSearch, MessagesSearchMerged, PEER_SEARCH_TYPED_LIMIT,
    POSTS_SEARCH_QUERY_DELAY_MS, PeerSearchController, PeerSearchResult, PeerSearchType,
    PostsSearchController, PostsSearchFloodState, PublicPostsPage, SEARCH_PEOPLE_LIMIT,
    SEARCH_PER_PAGE, SHARED_MEDIA_LIMIT, SearchCounterResult, SearchSliceResult, SharedMediaFilter,
    classify_hash_or_cashtag, convert_peer_search_query, get_search_counters, parse_search_result,
    prepare_global_media_request, prepare_shared_media_search_request,
};
pub use stickers::{
    CLOUD_RECENT_ATTACHED_SET_ID, CLOUD_RECENT_SET_ID, COLLECTIBLE_SET_ID, CUSTOM_SET_ID,
    CachedStickerSet, DEFAULT_SET_ID, FAVED_SET_ID, FEATURED_SET_ID, InstallStickerSetOutcome,
    MAX_STICKER_SET_TITLE_LEN, NONE_SET_ID, RECENT_SET_ID, STICKERS_UPDATE_TIMEOUT_MS,
    StickerSetFlags, StickerSetSpec, StickersSyncManager, StickersType, parse_sticker_set_flags,
    tl_sticker_set_hash_to_u64,
};
pub use types::{
    ApiCredentials, ForwardOptions, LoadDirection, SendOptions, code_settings_desktop_default,
};
pub use updates::{
    CHANNEL_GET_DIFF_LIMIT, DEFAULT_OFFLINE_IDLE_TIMEOUT_MS, DEFAULT_ONLINE_UPDATE_PERIOD_MS,
    FAIL_DIFF_INITIAL_BACKOFF_SECS, FAIL_DIFF_MAX_BACKOFF_SECS, FAST_GAP_TIMEOUT_MS,
    IDLE_FINISH_CHECK_MS, NO_UPDATES_AFTER_SLEEP_TIMEOUT_MS, NO_UPDATES_TIMEOUT_MS,
    PersistedUpdatesState, PresenceUpdateResult, UpdateEvent, UpdatePayload, UpdatesHandler,
    WAIT_FOR_CHANNEL_GET_DIFF_MS, WAIT_FOR_SKIPPED_TIMEOUT_MS,
};
