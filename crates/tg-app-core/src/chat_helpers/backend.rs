//! Backend data trait for `chat_helpers` UI widgets (emoji, stickers, GIFs, autocomplete).
//!
//! Port origin: decouples `Telegram/SourceFiles/chat_helpers/*` widgets from `Main::Session`,
//! `Data::Stickers`, and `ApiWrap` so `MockChatHelpersBackend` and future `tg-api` backends
//! plug into the exact same UI components.

use std::collections::HashMap;

use tg_chat::autocomplete::RecentHashtags;
use tg_chat::emoji::{EmojiKeywordResult, EmojiVariants, SkinTone};
use tg_chat::stickers::{StickerDocument, StickerPanelSection, StickerTabIcon};
use tg_chat::{BotCommand, DocumentId, StickerSetId, UserId, UserInfo};

use crate::stand_in::types::{MsgId, PeerId};

/// Default page limit for `messages.searchStickers` (`kCloudSearchPageLimit = 50`,
/// `chat_helpers/emoji_list_widget.cpp:72` & `stickers_list_widget.cpp:765`).
pub const CLOUD_SEARCH_PAGE_LIMIT: i32 = 50;

/// Default page limit for `messages.getOldFeaturedStickers` (`kOfficialLoadLimit = 40`,
/// `chat_helpers/stickers_list_widget.cpp:84`).
pub const OLD_FEATURED_LOAD_LIMIT: i32 = 40;

/// Query parameters for `messages.searchStickers`
/// (`chat_helpers/stickers_list_widget.cpp:754-766`, `emoji_list_widget.cpp:941-955`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchStickersRequest {
    /// When `true`, sets `flags.0` (`emojis`) to search custom emoji instead of stickers.
    pub emojis: bool,
    /// Free-text search query.
    pub query: String,
    /// Optional emoticon filter (e.g. `"😀"` in `EmojiListWidget::requestSearchCloud`).
    pub emoticon: String,
    /// ISO language codes for server keyword matching (e.g. `["en"]`).
    pub lang_codes: Vec<String>,
    /// Pagination offset (`0` for the first page).
    pub offset: i32,
    /// Page size limit (`<= 0` defaults to [`CLOUD_SEARCH_PAGE_LIMIT`]).
    pub limit: i32,
}

/// Paginated result returned by `messages.searchStickers`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchStickersPage {
    /// Hydrated sticker or custom-emoji document IDs for this page.
    pub stickers: Vec<DocumentId>,
    /// Next pagination offset reported by the server, or `None` when exhausted.
    pub next_offset: Option<i32>,
    /// Server-returned 64-bit hash cached for subsequent `NotModified` validation.
    pub hash: i64,
    /// `true` if the server replied `messages.foundStickersNotModified`.
    pub not_modified: bool,
}

/// Result returned by `messages.searchEmojiStickerSets` (`emoji_list_widget.cpp:924-939`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchStickerSetsResult {
    /// Matching hydrated custom-emoji set IDs in server order.
    pub set_ids: Vec<StickerSetId>,
    /// Server-returned 64-bit hash cached for subsequent `NotModified` validation.
    pub hash: i64,
    /// `true` if the server replied `messages.foundStickerSetsNotModified`.
    pub not_modified: bool,
}

/// Paginated result returned by `messages.getOldFeaturedStickers`
/// (`stickers_list_widget.cpp:515-547`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OldFeaturedStickersPage {
    /// Hydrated featured sticker set IDs in this page.
    pub set_ids: Vec<StickerSetId>,
    /// Unread featured sticker set IDs reported by the server in this page.
    pub unread_ids: Vec<StickerSetId>,
    /// Total count reported by `messages.featuredStickers.count` / `featuredStickersNotModified.count`.
    pub total_count: i32,
    /// Next pagination offset (`offset + set_ids.len()`).
    pub next_offset: i32,
    /// Whether the server flagged this featured page as `premium`.
    pub premium: bool,
    /// Server-returned 64-bit hash cached for subsequent `NotModified` validation.
    pub hash: i64,
    /// `true` if the server replied `messages.featuredStickersNotModified`.
    pub not_modified: bool,
}

/// Media selector for `messages.getAttachedStickers` (`api/api_attached_stickers.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachedStickersMedia {
    /// Explicit photo reference (`inputStickeredMediaPhoto`).
    Photo {
        /// Photo ID.
        id: i64,
        /// Server access hash.
        access_hash: i64,
        /// File reference bytes.
        file_reference: Vec<u8>,
    },
    /// Explicit document reference (`inputStickeredMediaDocument`).
    Document {
        /// Document ID.
        id: i64,
        /// Server access hash.
        access_hash: i64,
        /// File reference bytes.
        file_reference: Vec<u8>,
    },
    /// Look up photo or document metadata from a cached message in `Session`.
    Message {
        /// Owning chat peer ID.
        peer_id: PeerId,
        /// Message ID carrying the photo or document.
        msg_id: MsgId,
    },
}

/// Summary of a sticker/mask/emoji pack returned by `messages.getAttachedStickers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedStickerSetSummary {
    /// Sticker set ID.
    pub set_id: StickerSetId,
    /// Server access hash.
    pub access_hash: u64,
    /// Display title.
    pub title: String,
    /// Short name / slug.
    pub short_name: String,
    /// Total number of stickers in the set.
    pub count: i32,
    /// `true` if this is a custom emoji pack.
    pub is_emoji: bool,
    /// `true` if this is a mask pack.
    pub is_masks: bool,
    /// Hydrated cover / preview sticker document IDs.
    pub cover_doc_ids: Vec<DocumentId>,
}

/// Catalog selector for `Api::PeerPhoto::EmojiListType` (`api/api_peer_photo.cpp:854-899`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultEmojiCatalogKind {
    /// `account.getDefaultProfilePhotoEmojis#e2750328`.
    ProfilePhoto,
    /// `account.getDefaultGroupPhotoEmojis#915860ae`.
    GroupPhoto,
    /// `account.getDefaultBackgroundEmojis#a60ab9ce`.
    Background,
    /// `account.getChannelRestrictedStatusEmojis#35a9e0d5`.
    ChannelRestrictedStatus,
}

/// Cached result of an `account.getDefault*Emojis` / `getChannelRestrictedStatusEmojis` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DefaultEmojiCatalog {
    /// Custom emoji document IDs returned in `emojiList.document_id`.
    pub document_ids: Vec<DocumentId>,
    /// Server-returned 64-bit hash cached for subsequent `NotModified` validation.
    pub hash: i64,
    /// `true` if the server replied `emojiListNotModified`.
    pub not_modified: bool,
}

/// Reaction identifier returned by `messages.getDefaultTagReactions`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DefaultTagReaction {
    /// Standard Unicode emoji reaction (`reactionEmoji`).
    Emoji(String),
    /// Custom emoji document reaction (`reactionCustomEmoji`).
    CustomEmoji(DocumentId),
    /// Paid Stars reaction (`reactionPaid`).
    Paid,
}

/// Cached result of `messages.getDefaultTagReactions` (`data/data_message_reactions.cpp:1079-1097`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DefaultTagReactionsResult {
    /// Ordered default tag reactions for Saved Messages.
    pub reactions: Vec<DefaultTagReaction>,
    /// Server-returned 64-bit hash cached for subsequent `NotModified` validation.
    pub hash: i64,
    /// `true` if the server replied `messages.reactionsNotModified`.
    pub not_modified: bool,
}

/// Paid reaction leaderboard privacy setting (`PaidReactionPrivacy` in `schema/api.tl:2060-2062`,
/// `Api::ParsePaidReactionShownPeer` in `api/api_global_privacy.cpp:18-42`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaidReactionPrivacy {
    /// `paidReactionPrivacyDefault#206ad49e` (show current user).
    #[default]
    Default,
    /// `paidReactionPrivacyAnonymous#1f0c1ad9` (hide sender identity).
    Anonymous,
    /// `paidReactionPrivacyPeer#dc6cfcf0` (show a specific peer, e.g. owned channel).
    Peer(PeerId),
}

/// Decoded or raw media payload for a sticker preview.
#[derive(Debug, Clone)]
pub enum StickerMedia {
    /// Static unpremultiplied RGBA8 image buffer.
    StaticRgba {
        /// Image width in pixels.
        width: u32,
        /// Image height in pixels.
        height: u32,
        /// Unpremultiplied RGBA8 pixel data (`width * height * 4` bytes).
        rgba: Vec<u8>,
    },
    /// Uncompressed Lottie JSON animation rendered live by `tg-lottie`.
    LottieJson(String),
    /// Gzip-compressed Telegram `.tgs` animated sticker rendered live by `tg-lottie`.
    LottieTgs(Vec<u8>),
}

/// Metadata for a saved or inline-search GIF item in the GIFs tab.
#[derive(Debug, Clone, PartialEq)]
pub struct GifItem {
    /// Document identifier.
    pub id: DocumentId,
    /// Short descriptive title shown in hover overlay.
    pub title: String,
    /// Intrinsic width in pixels (used for mosaic aspect ratio).
    pub width: u32,
    /// Intrinsic height in pixels (used for mosaic aspect ratio).
    pub height: u32,
    /// Searchable tags associated with this GIF.
    pub tags: Vec<String>,
    /// Whether the GIF is currently in the user's saved GIFs list.
    pub saved: bool,
    /// Number of animation frames available via `gif_frame_rgba`.
    pub frame_count: usize,
    /// Playback frame rate in frames per second.
    pub fps: f32,
}

/// Category quick-filter button in the GIFs tab footer (`GifSection` in `gifs_list_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifCategory {
    /// Stable category identifier (e.g. `"recent"`, `"trending"`, `"joy"`).
    pub id: String,
    /// Emoji glyph shown on the category button (empty for the built-in Recent clock icon).
    pub emoji: String,
    /// Tooltip / accessible label.
    pub label: String,
    /// Inline-bot search query triggered when this category is selected (empty = saved GIFs).
    pub query: String,
}

/// Hook for inline-bot GIF searches (e.g. `@gif <query>`), matching `GifsListWidget::sendInlineRequest`.
pub trait InlineGifQueryHook {
    /// Executes an inline-bot GIF query and returns matching [`GifItem`] results.
    fn query_inline_gifs(&self, bot_username: &str, query: &str) -> Vec<GifItem>;
}

/// Data and mutation interface consumed by [`crate::chat_helpers::TabbedPanel`],
/// [`crate::chat_helpers::TabbedSelector`], and [`crate::chat_helpers::FieldAutocompleteView`].
pub trait ChatHelpersBackend {
    // --- Emoji ---

    /// Returns recently used emoji strings in descending usage order.
    fn recent_emoji(&mut self) -> Vec<String>;

    /// Records usage of `emoji` in the recent emoji list.
    fn record_emoji_used(&mut self, emoji: &str);

    /// Hides `emoji` from the recent emoji list.
    fn hide_recent_emoji(&mut self, emoji: &str);

    /// Returns the user's saved Fitzpatrick skin tone preferences.
    fn emoji_variants(&self) -> &EmojiVariants;

    /// Saves a preferred skin tone for a single emoji (keyed by its base uncolored sequence).
    fn save_emoji_variant(&mut self, base_emoji: &str, tone: SkinTone);

    /// Sets a global skin tone preference for all colorizable emoji.
    fn save_all_emoji_variants(&mut self, tone: SkinTone);

    /// Searches emoji by keyword or shortcode using `tg-chat`'s `EmojiKeywords` engine.
    fn search_emoji(&mut self, query: &str) -> Vec<EmojiKeywordResult>;

    // --- Stickers ---

    /// Whether the current user has Telegram Premium (controls Premium-locked sticker markers).
    fn is_premium(&self) -> bool;

    /// Toggles Premium status (useful for mock previews and tests).
    fn set_premium(&mut self, premium: bool);

    /// Builds ordered "My Stickers" sections (Favorites, Recent, Installed sets).
    fn sticker_sections(&self) -> Vec<StickerPanelSection>;

    /// Builds ordered "Trending / Featured" sticker sections.
    fn featured_sticker_sections(&self) -> Vec<StickerPanelSection>;

    /// Builds footer pack icon descriptors for `sections`.
    fn sticker_tab_icons(
        &self,
        sections: &[StickerPanelSection],
        icon_side: i32,
    ) -> Vec<StickerTabIcon>;

    /// Looks up metadata for a sticker document.
    fn sticker_document(&self, id: DocumentId) -> Option<&StickerDocument>;

    /// Returns static RGBA or Lottie animation payload for rendering a sticker preview.
    fn sticker_media(&self, id: DocumentId) -> Option<StickerMedia>;

    /// Searches stickers by emoji or keyword query.
    fn search_stickers(&self, query: &str) -> Vec<DocumentId>;

    /// Records usage of a sticker (moves to front of Recent stickers).
    fn record_sticker_used(&mut self, id: DocumentId);

    /// Returns `true` if `id` is currently in Favorite stickers.
    fn is_sticker_faved(&self, id: DocumentId) -> bool;

    /// Toggles favorite status of `id`; returns the new favorite state.
    fn toggle_faved_sticker(&mut self, id: DocumentId) -> bool;

    /// Installs a featured sticker set locally; returns `true` if installed.
    fn install_sticker_set(&mut self, set_id: StickerSetId) -> bool;

    /// Uninstalls an installed sticker set locally; returns `true` if removed.
    fn remove_sticker_set(&mut self, set_id: StickerSetId) -> bool;

    /// Returns `true` if `set_id` is currently installed.
    fn is_set_installed(&self, set_id: StickerSetId) -> bool;

    // --- GIFs ---

    /// Returns the user's saved GIFs list.
    fn saved_gifs(&self) -> Vec<GifItem>;

    /// Returns footer category shortcuts for the GIFs tab.
    fn gif_categories(&self) -> Vec<GifCategory>;

    /// Searches GIFs via the inline-bot query hook (`@gif <query>`).
    fn search_gifs(&self, query: &str) -> Vec<GifItem>;

    /// Toggles whether `id` is saved in the user's Saved GIFs; returns new saved state.
    fn toggle_saved_gif(&mut self, id: DocumentId) -> bool;

    /// Renders or returns a single RGBA8 animation frame `(width, height, rgba)` for `id`.
    fn gif_frame_rgba(&self, id: DocumentId, frame_index: usize) -> Option<(u32, u32, Vec<u8>)>;

    // --- Field autocomplete ---

    /// Current account's own user ID (for `mention://` tags).
    fn self_user_id(&self) -> UserId;

    /// Chat participants and known bots available for `@mention` / `/command` autocomplete.
    fn autocomplete_users(&self) -> &[UserInfo];

    /// Most recent message authors in the active chat (user IDs, newest first).
    fn autocomplete_last_authors(&self) -> &[UserId];

    /// Per-bot slash command catalog.
    fn autocomplete_bot_commands(&self) -> &HashMap<UserId, Vec<BotCommand>>;

    /// Recent inline bot user IDs shown at the top of `@` mention autocomplete.
    fn recent_inline_bot_ids(&self) -> &[UserId];

    /// Removes a bot from the recent inline bots list.
    fn remove_recent_inline_bot(&mut self, id: UserId) -> bool;

    /// Recent hashtag history store.
    fn recent_hashtags(&self) -> &RecentHashtags;

    /// Records usage of a hashtag.
    fn record_hashtag(&mut self, tag: &str);

    /// Removes a hashtag from history.
    fn remove_hashtag(&mut self, tag: &str) -> bool;

    /// Searches emoji suggestions for `:keyword` or exact first-word autocomplete.
    fn search_emoji_suggestions(&mut self, keyword: &str, exact: bool) -> Vec<EmojiKeywordResult>;

    /// Returns sticker suggestions matching a single emoji typed in the compose field.
    fn stickers_for_emoji(&self, emoji: &str) -> Vec<DocumentId>;

    // --- Remote sticker/emoji search, catalogs, attached stickers & reactions extras (WP-67) ---

    /// Searches remote stickers or custom emoji via `messages.searchStickers` with hash caching
    /// and document pool hydration (`stickers_list_widget.cpp:754-766`, `emoji_list_widget.cpp:941-955`).
    fn search_stickers_remote(
        &self,
        _req: &SearchStickersRequest,
    ) -> Result<SearchStickersPage, String> {
        Ok(SearchStickersPage::default())
    }

    /// Searches remote custom-emoji sticker sets via `messages.searchEmojiStickerSets` with
    /// hash caching and document pool hydration (`emoji_list_widget.cpp:924-939`).
    fn search_emoji_sticker_sets(
        &self,
        _query: &str,
        _exclude_featured: bool,
    ) -> Result<SearchStickerSetsResult, String> {
        Ok(SearchStickerSetsResult::default())
    }

    /// Loads a paginated slice of older featured sticker packs via
    /// `messages.getOldFeaturedStickers` (`stickers_list_widget.cpp:515-547`).
    fn load_old_featured_stickers(
        &self,
        offset: i32,
        _limit: i32,
    ) -> Result<OldFeaturedStickersPage, String> {
        Ok(OldFeaturedStickersPage {
            next_offset: offset.max(0),
            ..OldFeaturedStickersPage::default()
        })
    }

    /// Clears cloud recent stickers (`attached = false`) or recent attached masks
    /// (`attached = true`) via `messages.clearRecentStickers` (`apiwrap.cpp:1747-1778`).
    fn clear_recent_stickers(&mut self, _attached: bool) -> Result<bool, String> {
        Ok(true)
    }

    /// Fetches sticker/mask/emoji sets attached to a photo or document via
    /// `messages.getAttachedStickers` (`api/api_attached_stickers.cpp`).
    fn get_attached_stickers(
        &self,
        _media: &AttachedStickersMedia,
    ) -> Result<Vec<AttachedStickerSetSummary>, String> {
        Ok(Vec::new())
    }

    /// Queries server-supported emoji keyword language codes via
    /// `messages.getEmojiKeywordsLanguages` (`chat_helpers/emoji_keywords.cpp:692-717`).
    fn emoji_keywords_languages(&self, _lang_codes: &[String]) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    /// Fetches a hash-cached default custom-emoji document catalog via
    /// `account.getDefault{ProfilePhoto,GroupPhoto,Background}Emojis` or
    /// `account.getChannelRestrictedStatusEmojis` (`api/api_peer_photo.cpp:869-899`).
    fn default_emoji_catalog(
        &self,
        _kind: DefaultEmojiCatalogKind,
        _force: bool,
    ) -> Result<DefaultEmojiCatalog, String> {
        Ok(DefaultEmojiCatalog::default())
    }

    /// Refreshes message reactions for `msg_ids` in `peer_id` via
    /// `messages.getMessagesReactions`, applying returned `Updates` through the session actor
    /// (`data/data_message_reactions.cpp:1696-1729`).
    fn refresh_messages_reactions(
        &self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
    ) -> Result<(), String> {
        Ok(())
    }

    /// Fetches default Saved Messages tag reactions via `messages.getDefaultTagReactions`
    /// with hash caching (`data/data_message_reactions.cpp:1079-1097`).
    fn default_tag_reactions(&self, _force: bool) -> Result<DefaultTagReactionsResult, String> {
        Ok(DefaultTagReactionsResult::default())
    }

    /// Loads or returns the cached default paid-reaction privacy setting via
    /// `messages.getPaidReactionPrivacy` (`api/api_global_privacy.cpp:177-194`).
    fn paid_reaction_privacy(&self, _force: bool) -> Result<PaidReactionPrivacy, String> {
        Ok(PaidReactionPrivacy::Default)
    }

    /// Updates paid-reaction privacy on a specific message via
    /// `messages.togglePaidReactionPrivacy` (`data/data_message_reactions.cpp:1813-1843`).
    fn toggle_paid_reaction_privacy(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _privacy: PaidReactionPrivacy,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Reports a participant's reaction on a message via `messages.reportReaction`
    /// (`api/api_report.cpp:199-212`).
    fn report_reaction(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _reaction_peer_id: PeerId,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Deletes a participant's reaction from a specific message via
    /// `messages.deleteParticipantReaction`, applying returned `Updates` through the session
    /// actor (`apiwrap.cpp:1584-1602`).
    fn delete_participant_reaction(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _participant_id: PeerId,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Deletes all reactions by `participant_id` across `peer_id` via
    /// `messages.deleteParticipantReactions` (`apiwrap.cpp:1566-1582`).
    fn delete_participant_reactions(
        &mut self,
        _peer_id: PeerId,
        _participant_id: PeerId,
    ) -> Result<bool, String> {
        Ok(true)
    }
}
