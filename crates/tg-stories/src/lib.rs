//! `tg-stories`: Pure-logic Telegram Stories domain model, store, viewer state machine,
//! TL conversions, and dialogs-bar ring model for Rustgram (Rust port of Telegram Desktop).
//!
//! Ported from:
//! - `Telegram/SourceFiles/data/data_story.{h,cpp}` (`story`)
//! - `Telegram/SourceFiles/data/data_stories_ids.{h,cpp}` (`ids`)
//! - `Telegram/SourceFiles/data/data_stories.{h,cpp}` (`store`, `tl`)
//! - `Telegram/SourceFiles/media/stories/media_stories_{controller,reply,recent_views,sibling,slider,share,stealth}.*` (`viewer`)
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_stories_{list,content}.{h,cpp}` (`dialogs_bar`)

pub mod dialogs_bar;
pub mod ids;
pub mod store;
pub mod story;
pub mod tl;
pub mod viewer;

pub use dialogs_bar::{
    COLLAPSE_AFTER_RATIO, DialogBarMode, DialogStoriesBarState, DialogStoriesContent,
    DialogStoryElement, EXPAND_AFTER_RATIO, FRICTION_RATIO, MAX_TOOLTIP_NAMES, PRELOAD_PAGES,
    RingSegment, RingSegmentKind, SHOWN_LAST_COUNT, SMALL_THUMBS_SHOWN, StoryUploadStatus,
    TooltipSummary, build_dialog_stories_content, build_last_for_peer, compute_ring_segments,
    last_for_peer_contains, peer_ring_segments,
};
pub use ids::{
    StoriesIds, StoriesIdsSlice, StoryAlbumIdsKey, insert_sorted_desc, remove_sorted_desc,
    remove_unsorted, reorder_story_ids, respecting_pinned,
};
pub use store::{
    ARCHIVE_FIRST_PER_PAGE, ARCHIVE_PER_PAGE, IGNORE_PRELOAD_AROUND_IF_LOADED,
    INCREMENT_VIEWS_DELAY_MS, MARK_AS_READ_DELAY_MS, MAX_EXPIRE_TIMER_SECS, MAX_PEER_MAX_IDS_BATCH,
    MAX_PRELOAD_SOURCES, MAX_RESOLVE_TOGETHER, MAX_SEGMENTS_COUNT, NoStory,
    PEER_MAX_IDS_POLL_DELAY_MS, POLL_VIEWS_INTERVAL_MS, POLLING_INTERVAL_CHAT_SECS,
    POLLING_INTERVAL_VIEWER_SECS, POLLING_VIEWS_PER_PAGE, PRELOAD_AROUND_COUNT, ParseSource,
    PeerSourceState, PeerStoryMeta, PollingKind, SAVED_FIRST_PER_PAGE, SAVED_PER_PAGE,
    STILL_PRELOAD_FROM_FIRST, StoriesAction, StoriesContext, StoriesEvent, StoriesSource,
    StoriesSourceInfo, StoriesStore, StorySourcesList, VIEWS_PER_PAGE, source_sort_key,
};
pub use story::{
    ChannelPost, DEFAULT_MAX_PINNED_COUNT, GeoPointAddress, PHOTO_STORY_DURATION_MS,
    RECENT_VIEWERS_MAX, RectF, STORIES_ALBUM_ID_ARCHIVE, STORIES_ALBUM_ID_SAVED, StealthMode,
    Story, StoryAlbum, StoryArea, StoryIdDates, StoryLocation, StoryMedia, StoryPeerContext,
    StoryPrivacy, StoryRepostInfo, StoryUpdateFlags, StoryVideoStream, StoryView, StoryViewKind,
    StoryViews, StoryViewsCounts, SuggestedReaction, UrlArea, WeatherArea, rotate_point,
    strip_story_links, to_tg_text,
};
pub use tl::{
    EditStoryInput, ParsedAlbums, ParsedAllStories, ParsedMediaAreas, ParsedPeerStories,
    ParsedStoriesPage, ParsedStoryItem, ParsedStoryUpdate, ParsedViewsSlice, RecentStoryInfo,
    SendStoryInput, StoryMediaAreasInput, StoryPrivacyOption, StoryPrivacyRuleInput,
    StoryRepostInput, StoryViewsListFilter, albums_from_tl, all_stories_from_tl,
    all_stories_from_tl_with_existing, build_activate_stealth_mode, build_can_send_story,
    build_create_album, build_delete_album, build_delete_stories, build_edit_story,
    build_export_story_link, build_get_album_stories, build_get_albums,
    build_get_all_read_peer_stories, build_get_all_stories, build_get_chats_to_send,
    build_get_peer_max_ids, build_get_peer_stories, build_get_pinned_stories,
    build_get_stories_archive, build_get_stories_by_id, build_get_stories_views,
    build_get_story_reactions_list, build_get_story_views_list,
    build_get_story_views_list_filtered, build_increment_story_views, build_read_stories,
    build_rename_album, build_reorder_album, build_reorder_albums, build_report_story,
    build_send_reaction, build_send_story, build_toggle_peer_stories_hidden, build_toggle_pinned,
    build_toggle_pinned_to_top, build_update_album_stories, document_from_tl, entities_from_tl,
    entity_from_tl, geo_point_from_tl, geo_point_to_tl, media_areas_to_tl, parse_media_areas,
    parse_privacy_rules, parse_privacy_rules_with, peer_id_from_tl, peer_stories_from_tl,
    peer_stories_from_tl_with_existing, photo_from_tl, reaction_from_tl, reaction_to_tl,
    recent_story_from_tl, stealth_mode_from_tl, stories_page_from_tl,
    stories_page_from_tl_with_existing, story_album_from_tl, story_area_from_tl, story_area_to_tl,
    story_item_from_tl, story_media_from_tl, story_privacy_to_tl, story_reactions_list_from_tl,
    story_update_from_tl, story_update_from_tl_with_existing, story_views_counts_from_tl,
    story_views_list_from_tl,
};
pub use viewer::{
    MARK_AS_READ_AFTER_PROGRESS, MARK_AS_READ_AFTER_SECONDS, PHOTO_PROGRESS_INTERVAL_MS,
    PRELOAD_NEXT_MEDIA_COUNT, PRELOAD_PEERS_COUNT, PRELOAD_PREVIOUS_MEDIA_COUNT,
    PRELOAD_STORIES_COUNT, PauseState, PhotoPlayback, PlaybackStatus, RecentViewsState,
    RecentViewsType, ReplyAreaKind, ReplyAreaState, ReplyContext, ReplyPlaceholder,
    ReplyWriteRestriction, SameDayRange, ShareCapability, SiblingPreview, SliderState,
    StealthActivationDecision, StoryAreaHit, StoryViewerController, TrackState,
    VIEWS_MENU_ADD_PER_PAGE, ViewerConfig, ViewerEvent, compute_reply_area_state,
    compute_same_day_range, evaluate_stealth_mode, recent_views_type_for, time_left_text,
};
