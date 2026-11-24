//! Protocol-level synchronization for hash-cached server lists:
//! reactions, emoji statuses, emoji groups, emoji keywords, app config,
//! peer colors, wallpapers, cloud themes, attach menu bots, premium promo,
//! saved ringtones, and quick replies.
//!
//! Port origin (tdesktop commit `c2d0ad9`, Layer 233):
//! - `data/data_message_reactions.cpp` (`requestTop`, `requestRecent`, `requestDefault`,
//!   `requestGeneric`, `refreshRecentDelayed`)
//! - `data/data_emoji_statuses.cpp` (`requestDefault`, `requestChannelDefault`,
//!   `requestRecent`, `requestCollectibles`, `requestGroups`, `refreshRecentDelayed`)
//! - `chat_helpers/emoji_keywords.cpp` (`LangPack::refresh`, `LangPack::applyDifference`,
//!   `ApplyDifference`)
//! - `main/main_app_config.cpp` (`AppConfig::refresh`, `kRefreshTimeout`)
//! - `api/api_peer_colors.cpp` (`PeerColors::request`, `PeerColors::requestProfile`,
//!   `kRequestEach`)
//! - `data/data_session.cpp` (`updateWallpapers`, `wallpapersHash`) &
//!   `boxes/background_box.cpp` (`requestPapers`)
//! - `data/data_cloud_themes.cpp` (`CloudThemes::refresh`, `refreshChatThemes`)
//! - `inline_bots/bot_attach_web_view.cpp` (`AttachWebView::requestBots`)
//! - `api/api_premium.cpp` (`Premium::reloadPromo`)
//! - `api/api_ringtones.cpp` (`Ringtones::requestList`, `Ringtones::applyUpdate`)
//! - `data/business/data_shortcut_messages.cpp` (`ShortcutMessages::preloadShortcuts`,
//!   `TooEarlyForRequest`, `kRequestTimeLimit`)

use std::collections::BTreeMap;

use tg_data::{ReactionId, Session};
use tg_tl::{enums as tl_enums, functions as tl_fns};
use tracing::warn;

use crate::convert::{apply_tl_users_and_chats, reaction_from_tl};
use crate::error::ApiError;
use crate::rpc::RpcClient;
use crate::stickers::{StickerSetSpec, StickersSyncManager};
use crate::updates::UpdateEvent;

// ============================================================================
// Refresh timing & limit constants (matching tdesktop verbatim)
// ============================================================================

/// Reactions full list refresh interval (1 hour; `data_message_reactions.cpp:48`).
pub const REACTIONS_FULL_LIST_REFRESH_MS: u64 = 3_600_000;
/// Delayed refetch throttle for `updateRecentReactions` (10s; `data_message_reactions.cpp:51`).
pub const RECENT_REACTIONS_DEBOUNCE_MS: u64 = 10_000;
/// Limit for `messages.getRecentReactions` (`data_message_reactions.cpp:52`).
pub const RECENT_REACTIONS_LIMIT: i32 = 40;
/// Delay before refreshing top reactions (60s; `data_message_reactions.cpp:54`).
pub const TOP_REACTIONS_DELAY_MS: u64 = 60_000;
/// Limit for `messages.getTopReactions` (`data_message_reactions.cpp:55`).
pub const TOP_REACTIONS_LIMIT: i32 = 14;
/// Delayed refetch throttle for `updateSavedReactionTags` (1s; `data_message_reactions.cpp:53`).
pub const MY_TAGS_DEBOUNCE_MS: u64 = 1_000;

/// Emoji statuses default list refresh interval (1 hour; `data_emoji_statuses.cpp:27`).
pub const EMOJI_STATUSES_REFRESH_MS: u64 = 3_600_000;
/// Delayed refetch throttle for `updateRecentEmojiStatuses` (10s; `data_emoji_statuses.cpp:28`).
pub const RECENT_EMOJI_STATUSES_DEBOUNCE_MS: u64 = 10_000;

/// Emoji keywords refresh interval (1 hour; `chat_helpers/emoji_keywords.cpp:28`).
pub const EMOJI_KEYWORDS_REFRESH_MS: u64 = 3_600_000;

/// `help.getAppConfig` refresh interval (1 hour; `main/main_app_config.cpp:23`).
pub const APP_CONFIG_REFRESH_MS: u64 = 3_600_000;

/// `help.getPeerColors` / `help.getPeerProfileColors` interval (1 hour; `api/api_peer_colors.cpp:19`).
pub const PEER_COLORS_REFRESH_MS: u64 = 3_600_000;

/// `messages.getQuickReplies` throttle interval (60s; `data/business/data_shortcut_messages.cpp:26`).
pub const QUICK_REPLIES_REFRESH_LIMIT_MS: u64 = 60_000;

/// Desktop theme format string (`Data::CloudThemes::Format()`, `data_cloud_themes.cpp`).
pub const CLOUD_THEME_FORMAT: &str = "tdesktop";

// ============================================================================
// 1. Reactions (`data/data_message_reactions.cpp`)
// ============================================================================

/// Cached available reaction entry from `messages.getAvailableReactions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAvailableReaction {
    /// Reaction emoji string.
    pub reaction: String,
    /// Human-readable title.
    pub title: String,
    /// Whether the reaction is inactive (hidden from quick picker).
    pub inactive: bool,
    /// Whether this reaction requires Telegram Premium.
    pub premium: bool,
}

/// Sync state for top, recent, and available reactions (`Data::Reactions`).
#[derive(Debug, Clone, Default)]
pub struct ReactionsSync {
    /// Server-returned 64-bit hash for `messages.getTopReactions`.
    pub top_hash: i64,
    /// Cached top reactions.
    pub top: Vec<ReactionId>,
    /// Last successful refresh timestamp (ms) for top reactions.
    pub last_top_ms: u64,

    /// Server-returned 64-bit hash for `messages.getRecentReactions`.
    pub recent_hash: i64,
    /// Cached recent reactions.
    pub recent: Vec<ReactionId>,
    /// Scheduled delayed refresh deadline (ms) set by `updateRecentReactions`.
    pub recent_scheduled_at_ms: Option<u64>,

    /// Server-returned 32-bit hash for `messages.getAvailableReactions`.
    pub available_hash: i32,
    /// All returned available reactions (including inactive).
    pub available: Vec<CachedAvailableReaction>,
    /// Active available reactions (`!inactive`).
    pub active: Vec<CachedAvailableReaction>,
    /// Last refresh timestamp (ms) for available reactions.
    pub last_available_ms: u64,

    /// Generic reaction animation document IDs (`InputStickerSetEmojiGenericAnimations`).
    pub generic_animation_doc_ids: Vec<u64>,

    /// Scheduled delayed refresh deadline (ms) for `updateSavedReactionTags`.
    pub my_tags_scheduled_at_ms: Option<u64>,
}

impl ReactionsSync {
    /// Schedules a debounced refresh of recent reactions (`Data::Reactions::refreshRecentDelayed`,
    /// `data_message_reactions.cpp:430-440`). Repeated updates while pending are coalesced.
    pub fn on_update_recent_reactions(&mut self, now_ms: u64) {
        if self.recent_scheduled_at_ms.is_none() {
            self.recent_scheduled_at_ms = Some(now_ms.saturating_add(RECENT_REACTIONS_DEBOUNCE_MS));
        }
    }

    /// Schedules a debounced refresh of saved reaction tags (`refreshMyTagsDelayed`,
    /// `data_message_reactions.cpp:450-461`).
    pub fn on_update_saved_reaction_tags(&mut self, now_ms: u64) {
        if self.my_tags_scheduled_at_ms.is_none() {
            self.my_tags_scheduled_at_ms = Some(now_ms.saturating_add(MY_TAGS_DEBOUNCE_MS));
        }
    }

    /// Fetches `messages.getTopReactions(limit = 14, hash)` (`data_message_reactions.cpp:972-991`).
    /// On RPC error, resets `top_hash = 0` (`data_message_reactions.cpp:989`).
    pub async fn refresh_top<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_top_ms > 0
            && now_ms < self.last_top_ms.saturating_add(TOP_REACTIONS_DELAY_MS)
        {
            return Ok(false);
        }
        let req = tl_fns::messages::GetTopReactions {
            limit: TOP_REACTIONS_LIMIT,
            hash: self.top_hash,
        };
        match rpc.invoke(&req).await {
            Ok(tl_enums::messages::Reactions::NotModified) => {
                self.last_top_ms = now_ms;
                Ok(true)
            }
            Ok(tl_enums::messages::Reactions::Reactions(data)) => {
                self.top_hash = data.hash;
                self.top = data.reactions.iter().filter_map(reaction_from_tl).collect();
                self.last_top_ms = now_ms;
                Ok(true)
            }
            Err(err) => {
                self.top_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `messages.getRecentReactions(limit = 40, hash)` (`data_message_reactions.cpp:993-1012`).
    /// On RPC error, resets `recent_hash = 0` (`data_message_reactions.cpp:1010`).
    pub async fn refresh_recent<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        self.recent_scheduled_at_ms = None;
        let req = tl_fns::messages::GetRecentReactions {
            limit: RECENT_REACTIONS_LIMIT,
            hash: self.recent_hash,
        };
        match rpc.invoke(&req).await {
            Ok(tl_enums::messages::Reactions::NotModified) => Ok(()),
            Ok(tl_enums::messages::Reactions::Reactions(data)) => {
                self.recent_hash = data.hash;
                self.recent = data.reactions.iter().filter_map(reaction_from_tl).collect();
                Ok(())
            }
            Err(err) => {
                self.recent_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `messages.getAvailableReactions(hash: i32)` (`data_message_reactions.cpp:1014-1031`).
    /// On RPC error, resets `available_hash = 0` (`data_message_reactions.cpp:1029`).
    pub async fn refresh_available<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_available_ms > 0
            && now_ms
                < self
                    .last_available_ms
                    .saturating_add(REACTIONS_FULL_LIST_REFRESH_MS)
        {
            return Ok(false);
        }
        let req = tl_fns::messages::GetAvailableReactions {
            hash: self.available_hash,
        };
        match rpc.invoke(&req).await {
            Ok(tl_enums::messages::AvailableReactions::NotModified) => {
                self.last_available_ms = now_ms;
                Ok(true)
            }
            Ok(tl_enums::messages::AvailableReactions::AvailableReactions(data)) => {
                self.available_hash = data.hash;
                self.available.clear();
                self.active.clear();
                for tl_enums::AvailableReaction::AvailableReaction(r) in data.reactions {
                    let entry = CachedAvailableReaction {
                        reaction: r.reaction,
                        title: r.title,
                        inactive: r.inactive,
                        premium: r.premium,
                    };
                    if !entry.inactive {
                        self.active.push(entry.clone());
                    }
                    self.available.push(entry);
                }
                self.last_available_ms = now_ms;
                Ok(true)
            }
            Err(err) => {
                self.available_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches generic reaction animations via `messages.getStickerSet(EmojiGenericAnimations, 0)`
    /// (`data_message_reactions.cpp:1033-1051`).
    pub async fn request_generic_animations<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<(), ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetStickerSet {
                stickerset: StickerSetSpec::EmojiGenericAnimations.to_tl(),
                hash: 0,
            })
            .await
            .map_err(Into::into)?;
        if let tl_enums::messages::StickerSet::StickerSet(data) = resp {
            self.generic_animation_doc_ids = data
                .documents
                .iter()
                .filter_map(|doc| match doc {
                    tl_enums::Document::Document(d) => Some(d.id as u64),
                    tl_enums::Document::Empty(_) => None,
                })
                .collect();
        }
        Ok(())
    }

    /// Executes any due delayed/periodic reaction refreshes at `now_ms`.
    pub async fn poll_due<C: RpcClient>(&mut self, rpc: &C, now_ms: u64) -> Result<(), ApiError> {
        if self
            .recent_scheduled_at_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.refresh_recent(rpc).await?;
        }
        if self.last_available_ms > 0
            && now_ms
                >= self
                    .last_available_ms
                    .saturating_add(REACTIONS_FULL_LIST_REFRESH_MS)
        {
            let _ = self.refresh_available(rpc, now_ms, false).await?;
        }
        Ok(())
    }
}

// ============================================================================
// 2. Emoji Statuses & Emoji Groups (`data/data_emoji_statuses.cpp`)
// ============================================================================

/// Cached emoji status entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedEmojiStatusEntry {
    /// Custom emoji document ID.
    pub document_id: u64,
    /// Optional collectible ID (for `emojiStatusCollectible`).
    pub collectible_id: Option<u64>,
    /// Optional expiration timestamp.
    pub until: i32,
}

/// Cached emoji category group from `messages.getEmoji*Groups`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedEmojiGroup {
    /// Icon custom emoji document ID.
    pub icon_emoji_id: i64,
    /// Group title (empty for `emojiGroupPremium`).
    pub title: String,
    /// Associated emoticons (empty for `emojiGroupPremium`).
    pub emoticons: Vec<String>,
    /// Whether this is a `emojiGroupPremium` sentinel group.
    pub is_premium: bool,
    /// Whether this is an `emojiGroupGreeting` group.
    pub is_greeting: bool,
}

/// Hash + cached list for one `messages.getEmoji*Groups` endpoint.
#[derive(Debug, Clone, Default)]
pub struct EmojiGroupsCache {
    /// Server-returned 32-bit hash.
    pub hash: i32,
    /// Cached groups.
    pub groups: Vec<CachedEmojiGroup>,
}

/// Sync state for default/channel/recent/collectible emoji statuses and all 4 emoji group lists.
#[derive(Debug, Clone, Default)]
pub struct EmojiStatusesSync {
    /// Server-returned 64-bit hash for `account.getDefaultEmojiStatuses`.
    pub default_hash: i64,
    /// Cached default emoji statuses.
    pub default_statuses: Vec<CachedEmojiStatusEntry>,
    /// Last refresh timestamp (ms) for default statuses.
    pub last_default_ms: u64,

    /// Server-returned 64-bit hash for `account.getChannelDefaultEmojiStatuses`.
    pub channel_default_hash: i64,
    /// Cached channel default emoji statuses.
    pub channel_default_statuses: Vec<CachedEmojiStatusEntry>,
    /// Last refresh timestamp (ms) for channel default statuses.
    pub last_channel_default_ms: u64,

    /// Server-returned 64-bit hash for `account.getRecentEmojiStatuses`.
    pub recent_hash: i64,
    /// Cached recent emoji statuses.
    pub recent_statuses: Vec<CachedEmojiStatusEntry>,
    /// Scheduled delayed refresh deadline (ms) set by `updateRecentEmojiStatuses`.
    pub recent_scheduled_at_ms: Option<u64>,

    /// Server-returned 64-bit hash for `account.getCollectibleEmojiStatuses`.
    pub collectibles_hash: i64,
    /// Cached collectible emoji statuses.
    pub collectible_statuses: Vec<CachedEmojiStatusEntry>,

    /// `messages.getEmojiGroups` cache.
    pub emoji_groups: EmojiGroupsCache,
    /// `messages.getEmojiStatusGroups` cache.
    pub status_groups: EmojiGroupsCache,
    /// `messages.getEmojiStickerGroups` cache.
    pub sticker_groups: EmojiGroupsCache,
    /// `messages.getEmojiProfilePhotoGroups` cache.
    pub profile_photo_groups: EmojiGroupsCache,
}

impl EmojiStatusesSync {
    /// Schedules a debounced refresh of recent emoji statuses
    /// (`Data::EmojiStatuses::refreshRecentDelayed`, `data_emoji_statuses.cpp:92-102`).
    pub fn on_update_recent_emoji_statuses(&mut self, now_ms: u64) {
        if self.recent_scheduled_at_ms.is_none() {
            self.recent_scheduled_at_ms =
                Some(now_ms.saturating_add(RECENT_EMOJI_STATUSES_DEBOUNCE_MS));
        }
    }

    /// Fetches `account.getDefaultEmojiStatuses(hash)` (`data_emoji_statuses.cpp:350-367`).
    pub async fn refresh_default<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_default_ms > 0
            && now_ms
                < self
                    .last_default_ms
                    .saturating_add(EMOJI_STATUSES_REFRESH_MS)
        {
            return Ok(false);
        }
        match rpc
            .invoke(&tl_fns::account::GetDefaultEmojiStatuses {
                hash: self.default_hash,
            })
            .await
        {
            Ok(tl_enums::account::EmojiStatuses::NotModified) => {
                self.last_default_ms = now_ms;
                Ok(true)
            }
            Ok(tl_enums::account::EmojiStatuses::EmojiStatuses(data)) => {
                self.default_hash = data.hash;
                self.default_statuses = parse_emoji_statuses(&data.statuses);
                self.last_default_ms = now_ms;
                Ok(true)
            }
            Err(err) => {
                self.default_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `account.getChannelDefaultEmojiStatuses(hash)` (`schema/api.tl:2538`,
    /// `data_emoji_statuses.cpp:390-407`).
    pub async fn refresh_channel_default<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_channel_default_ms > 0
            && now_ms
                < self
                    .last_channel_default_ms
                    .saturating_add(EMOJI_STATUSES_REFRESH_MS)
        {
            return Ok(false);
        }
        match rpc
            .invoke(&tl_fns::account::GetChannelDefaultEmojiStatuses {
                hash: self.channel_default_hash,
            })
            .await
        {
            Ok(tl_enums::account::EmojiStatuses::NotModified) => {
                self.last_channel_default_ms = now_ms;
                Ok(true)
            }
            Ok(tl_enums::account::EmojiStatuses::EmojiStatuses(data)) => {
                self.channel_default_hash = data.hash;
                self.channel_default_statuses = parse_emoji_statuses(&data.statuses);
                self.last_channel_default_ms = now_ms;
                Ok(true)
            }
            Err(err) => {
                self.channel_default_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `account.getRecentEmojiStatuses(hash)` (`data_emoji_statuses.cpp:330-348`).
    pub async fn refresh_recent<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        self.recent_scheduled_at_ms = None;
        match rpc
            .invoke(&tl_fns::account::GetRecentEmojiStatuses {
                hash: self.recent_hash,
            })
            .await
        {
            Ok(tl_enums::account::EmojiStatuses::NotModified) => Ok(()),
            Ok(tl_enums::account::EmojiStatuses::EmojiStatuses(data)) => {
                self.recent_hash = data.hash;
                self.recent_statuses = parse_emoji_statuses(&data.statuses);
                Ok(())
            }
            Err(err) => {
                self.recent_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `account.getCollectibleEmojiStatuses(hash)` (`data_emoji_statuses.cpp:429-446`).
    pub async fn refresh_collectibles<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        match rpc
            .invoke(&tl_fns::account::GetCollectibleEmojiStatuses {
                hash: self.collectibles_hash,
            })
            .await
        {
            Ok(tl_enums::account::EmojiStatuses::NotModified) => Ok(()),
            Ok(tl_enums::account::EmojiStatuses::EmojiStatuses(data)) => {
                self.collectibles_hash = data.hash;
                self.collectible_statuses = parse_emoji_statuses(&data.statuses);
                Ok(())
            }
            Err(err) => {
                self.collectibles_hash = 0;
                Err(ApiError::Invocation(err.into()))
            }
        }
    }

    /// Fetches `messages.getEmojiGroups(hash: i32)` (`data_emoji_statuses.cpp:202-207`).
    pub async fn refresh_emoji_groups<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetEmojiGroups {
                hash: self.emoji_groups.hash,
            })
            .await
            .map_err(Into::into)?;
        apply_emoji_groups_response(&mut self.emoji_groups, resp);
        Ok(())
    }

    /// Fetches `messages.getEmojiStatusGroups(hash: i32)` (`data_emoji_statuses.cpp:209-213`).
    pub async fn refresh_status_groups<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetEmojiStatusGroups {
                hash: self.status_groups.hash,
            })
            .await
            .map_err(Into::into)?;
        apply_emoji_groups_response(&mut self.status_groups, resp);
        Ok(())
    }

    /// Fetches `messages.getEmojiStickerGroups(hash: i32)` (`data_emoji_statuses.cpp:215-219`).
    pub async fn refresh_sticker_groups<C: RpcClient>(&mut self, rpc: &C) -> Result<(), ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetEmojiStickerGroups {
                hash: self.sticker_groups.hash,
            })
            .await
            .map_err(Into::into)?;
        apply_emoji_groups_response(&mut self.sticker_groups, resp);
        Ok(())
    }

    /// Fetches `messages.getEmojiProfilePhotoGroups(hash: i32)` (`data_emoji_statuses.cpp:221-226`).
    pub async fn refresh_profile_photo_groups<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<(), ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetEmojiProfilePhotoGroups {
                hash: self.profile_photo_groups.hash,
            })
            .await
            .map_err(Into::into)?;
        apply_emoji_groups_response(&mut self.profile_photo_groups, resp);
        Ok(())
    }

    /// Executes any due delayed/periodic emoji status refreshes at `now_ms`.
    pub async fn poll_due<C: RpcClient>(&mut self, rpc: &C, now_ms: u64) -> Result<(), ApiError> {
        if self
            .recent_scheduled_at_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.refresh_recent(rpc).await?;
        }
        if self.last_default_ms > 0
            && now_ms
                >= self
                    .last_default_ms
                    .saturating_add(EMOJI_STATUSES_REFRESH_MS)
        {
            let _ = self.refresh_default(rpc, now_ms, false).await?;
        }
        if self.last_channel_default_ms > 0
            && now_ms
                >= self
                    .last_channel_default_ms
                    .saturating_add(EMOJI_STATUSES_REFRESH_MS)
        {
            let _ = self.refresh_channel_default(rpc, now_ms, false).await?;
        }
        Ok(())
    }
}

fn parse_emoji_statuses(list: &[tl_enums::EmojiStatus]) -> Vec<CachedEmojiStatusEntry> {
    list.iter()
        .filter_map(|status| match status {
            tl_enums::EmojiStatus::EmojiStatus(s) => Some(CachedEmojiStatusEntry {
                document_id: s.document_id as u64,
                collectible_id: None,
                until: s.until.unwrap_or(0),
            }),
            tl_enums::EmojiStatus::Collectible(c) => Some(CachedEmojiStatusEntry {
                document_id: c.document_id as u64,
                collectible_id: Some(c.collectible_id as u64),
                until: c.until.unwrap_or(0),
            }),
            tl_enums::EmojiStatus::Empty
            | tl_enums::EmojiStatus::InputEmojiStatusCollectible(_) => None,
        })
        .collect()
}

fn apply_emoji_groups_response(
    target: &mut EmojiGroupsCache,
    resp: tl_enums::messages::EmojiGroups,
) {
    match resp {
        tl_enums::messages::EmojiGroups::NotModified => {}
        tl_enums::messages::EmojiGroups::EmojiGroups(data) => {
            target.hash = data.hash;
            target.groups = data
                .groups
                .into_iter()
                .map(|g| match g {
                    tl_enums::EmojiGroup::EmojiGroup(e) => CachedEmojiGroup {
                        icon_emoji_id: e.icon_emoji_id,
                        title: e.title,
                        emoticons: e.emoticons,
                        is_premium: false,
                        is_greeting: false,
                    },
                    tl_enums::EmojiGroup::Greeting(e) => CachedEmojiGroup {
                        icon_emoji_id: e.icon_emoji_id,
                        title: e.title,
                        emoticons: e.emoticons,
                        is_premium: false,
                        is_greeting: true,
                    },
                    tl_enums::EmojiGroup::Premium(e) => CachedEmojiGroup {
                        icon_emoji_id: e.icon_emoji_id,
                        title: e.title,
                        emoticons: Vec::new(),
                        is_premium: true,
                        is_greeting: false,
                    },
                })
                .collect();
        }
    }
}

// ============================================================================
// 3. Emoji Keywords (`chat_helpers/emoji_keywords.cpp`)
// ============================================================================

/// Per-language emoji keywords dictionary (`ChatHelpers::EmojiKeywords::LangPack`).
#[derive(Debug, Clone, Default)]
pub struct EmojiKeywordsPack {
    /// Language code (e.g. `"en"`, `"ru"`).
    pub lang_code: String,
    /// Server version (`0` means full fetch via `messages.getEmojiKeywords` is needed).
    pub version: i32,
    /// Last refresh timestamp (ms).
    pub last_refresh_ms: u64,
    /// Normalized lowercase keyword -> emoticon list.
    pub keywords: BTreeMap<String, Vec<String>>,
    /// Maximum keyword character length (`maxKeyLength`).
    pub max_key_length: usize,
}

/// Sync state for multi-language emoji keyword dictionaries (`chat_helpers/emoji_keywords.cpp`).
#[derive(Debug, Clone, Default)]
pub struct EmojiKeywordsSync {
    /// Cached keyword packs keyed by `lang_code`.
    pub packs: BTreeMap<String, EmojiKeywordsPack>,
}

impl EmojiKeywordsSync {
    /// Refreshes emoji keywords for `lang_code` (`EmojiKeywords::LangPack::refresh`,
    /// `chat_helpers/emoji_keywords.cpp:386-455`).
    ///
    /// - Throttled to 1 hour (`EMOJI_KEYWORDS_REFRESH_MS`) unless `force = true`.
    /// - Uses `messages.getEmojiKeywords(lang_code)` when `version == 0`, and
    ///   `messages.getEmojiKeywordsDifference(lang_code, from_version)` when `version > 0`.
    /// - Rejects mismatched `data.lang_code != lang_code` by resetting `version = 0`.
    /// - Preserves cache when `keywords.is_empty() && current_version >= data.version`.
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        lang_code: &str,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        let pack = self
            .packs
            .entry(lang_code.to_owned())
            .or_insert_with(|| EmojiKeywordsPack {
                lang_code: lang_code.to_owned(),
                ..Default::default()
            });
        if !force
            && pack.last_refresh_ms > 0
            && now_ms.saturating_sub(pack.last_refresh_ms) < EMOJI_KEYWORDS_REFRESH_MS
        {
            return Ok(false);
        }
        let current_version = pack.version;
        let diff_res = if current_version > 0 {
            rpc.invoke(&tl_fns::messages::GetEmojiKeywordsDifference {
                lang_code: lang_code.to_owned(),
                from_version: current_version,
            })
            .await
        } else {
            rpc.invoke(&tl_fns::messages::GetEmojiKeywords {
                lang_code: lang_code.to_owned(),
            })
            .await
        };
        let pack = self.packs.get_mut(lang_code).expect("pack inserted above");
        pack.last_refresh_ms = now_ms;
        let tl_enums::EmojiKeywordsDifference::EmojiKeywordsDifference(data) =
            diff_res.map_err(Into::into)?;

        if data.lang_code != pack.lang_code {
            warn!(
                expected = %pack.lang_code,
                got = %data.lang_code,
                "API Error: Bad lang_code for emoji keywords"
            );
            pack.version = 0;
            return Ok(true);
        }
        if data.keywords.is_empty() && pack.version >= data.version {
            return Ok(true);
        }

        pack.version = data.version;
        for kw in data.keywords {
            match kw {
                tl_enums::EmojiKeyword::EmojiKeyword(entry) => {
                    let word = entry.keyword.trim().to_lowercase();
                    if word.is_empty() {
                        continue;
                    }
                    pack.keywords.insert(word, entry.emoticons);
                }
                tl_enums::EmojiKeyword::Deleted(deleted) => {
                    let word = deleted.keyword.trim().to_lowercase();
                    if word.is_empty() {
                        continue;
                    }
                    if let Some(list) = pack.keywords.get_mut(&word) {
                        list.retain(|e| !deleted.emoticons.contains(e));
                        if list.is_empty() {
                            pack.keywords.remove(&word);
                        }
                    }
                }
            }
        }
        pack.max_key_length = pack
            .keywords
            .keys()
            .map(|k| k.chars().count())
            .max()
            .unwrap_or(0);
        Ok(true)
    }
}

// ============================================================================
// 4. AppConfig (`main/main_app_config.cpp`)
// ============================================================================

/// Simplified parsed value from TL `JSONValue` in `help.appConfig`.
#[derive(Debug, Clone, PartialEq)]
pub enum AppConfigValue {
    /// `jsonNull`.
    Null,
    /// `jsonBool`.
    Bool(bool),
    /// `jsonNumber`.
    Number(f64),
    /// `jsonString`.
    String(String),
    /// `jsonArray`.
    Array(Vec<AppConfigValue>),
    /// `jsonObject`.
    Object(BTreeMap<String, AppConfigValue>),
}

impl AppConfigValue {
    /// Converts a TL `JSONValue` recursively.
    #[must_use]
    pub fn from_tl(value: &tl_enums::JSONValue) -> Self {
        match value {
            tl_enums::JSONValue::JsonNull => Self::Null,
            tl_enums::JSONValue::JsonBool(b) => Self::Bool(b.value),
            tl_enums::JSONValue::JsonNumber(n) => Self::Number(n.value),
            tl_enums::JSONValue::JsonString(s) => Self::String(s.value.clone()),
            tl_enums::JSONValue::JsonArray(a) => {
                Self::Array(a.value.iter().map(Self::from_tl).collect())
            }
            tl_enums::JSONValue::JsonObject(o) => {
                let mut map = BTreeMap::new();
                for tl_enums::JSONObjectValue::JsonObjectValue(kv) in &o.value {
                    map.insert(kv.key.clone(), Self::from_tl(&kv.value));
                }
                Self::Object(map)
            }
        }
    }
}

/// Sync state for `help.getAppConfig` (`Main::AppConfig`, `main/main_app_config.cpp:343-394`).
#[derive(Debug, Clone, Default)]
pub struct AppConfigSync {
    /// Server-returned 32-bit hash for `help.getAppConfig`.
    pub hash: i32,
    /// Parsed top-level JSON object fields.
    pub values: BTreeMap<String, AppConfigValue>,
    /// Last refresh timestamp (ms).
    pub last_refresh_ms: u64,
    /// Whether `help.getConfig` was requested on `updateConfig`.
    pub last_mtp_config_refresh_ms: u64,
}

impl AppConfigSync {
    /// Refreshes `help.getAppConfig(hash)` if due (1-hour interval) or `force = true`.
    /// On `help.appConfigNotModified`, preserves cached `values` and `hash`.
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_refresh_ms > 0
            && now_ms < self.last_refresh_ms.saturating_add(APP_CONFIG_REFRESH_MS)
        {
            return Ok(false);
        }
        let resp = rpc
            .invoke(&tl_fns::help::GetAppConfig { hash: self.hash })
            .await
            .map_err(Into::into);
        self.last_refresh_ms = now_ms;
        match resp? {
            tl_enums::help::AppConfig::NotModified => Ok(true),
            tl_enums::help::AppConfig::AppConfig(cfg) => {
                self.hash = cfg.hash;
                if let tl_enums::JSONValue::JsonObject(obj) = cfg.config {
                    self.values.clear();
                    for tl_enums::JSONObjectValue::JsonObjectValue(kv) in obj.value {
                        self.values
                            .insert(kv.key, AppConfigValue::from_tl(&kv.value));
                    }
                }
                Ok(true)
            }
        }
    }

    /// Handles `updateConfig` (`api/api_updates.cpp:2208-2212`):
    /// refreshes both `help.getConfig()` and forced `help.getAppConfig(hash)`.
    pub async fn on_update_config<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
    ) -> Result<tl_enums::Config, ApiError> {
        let mtp_cfg = rpc
            .invoke(&tl_fns::help::GetConfig)
            .await
            .map_err(Into::into)?;
        self.last_mtp_config_refresh_ms = now_ms;
        let _ = self.refresh(rpc, now_ms, true).await?;
        Ok(mtp_cfg)
    }
}

// ============================================================================
// 5. Peer Colors (`api/api_peer_colors.cpp`)
// ============================================================================

/// Cached peer color entry from `help.getPeerColors` / `help.getPeerProfileColors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPeerColorOption {
    /// Color index ID.
    pub color_id: i32,
    /// Whether hidden from pickers.
    pub hidden: bool,
    /// Required channel boost level (if any).
    pub channel_min_level: Option<i32>,
    /// Required group boost level (if any).
    pub group_min_level: Option<i32>,
}

/// Sync state for `help.getPeerColors` and `help.getPeerProfileColors` (`Api::PeerColors`).
#[derive(Debug, Clone, Default)]
pub struct PeerColorsSync {
    /// Server-returned 32-bit hash for `help.getPeerColors`.
    pub hash: i32,
    /// Cached peer reply/accent color options.
    pub colors: Vec<CachedPeerColorOption>,
    /// Last refresh timestamp (ms) for `help.getPeerColors`.
    pub last_refresh_ms: u64,

    /// Server-returned 32-bit hash for `help.getPeerProfileColors`.
    pub profile_hash: i32,
    /// Cached peer profile color options.
    pub profile_colors: Vec<CachedPeerColorOption>,
    /// Last refresh timestamp (ms) for `help.getPeerProfileColors`.
    pub last_profile_refresh_ms: u64,
}

impl PeerColorsSync {
    /// Fetches `help.getPeerColors(hash)` respecting `PEER_COLORS_REFRESH_MS` (1h).
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_refresh_ms > 0
            && now_ms < self.last_refresh_ms.saturating_add(PEER_COLORS_REFRESH_MS)
        {
            return Ok(false);
        }
        let resp = rpc
            .invoke(&tl_fns::help::GetPeerColors { hash: self.hash })
            .await
            .map_err(Into::into)?;
        self.last_refresh_ms = now_ms;
        if let tl_enums::help::PeerColors::PeerColors(data) = resp {
            self.hash = data.hash;
            self.colors = parse_peer_color_options(&data.colors);
        }
        Ok(true)
    }

    /// Fetches `help.getPeerProfileColors(hash)` respecting `PEER_COLORS_REFRESH_MS` (1h).
    pub async fn refresh_profile<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_profile_refresh_ms > 0
            && now_ms
                < self
                    .last_profile_refresh_ms
                    .saturating_add(PEER_COLORS_REFRESH_MS)
        {
            return Ok(false);
        }
        let resp = rpc
            .invoke(&tl_fns::help::GetPeerProfileColors {
                hash: self.profile_hash,
            })
            .await
            .map_err(Into::into)?;
        self.last_profile_refresh_ms = now_ms;
        if let tl_enums::help::PeerColors::PeerColors(data) = resp {
            self.profile_hash = data.hash;
            self.profile_colors = parse_peer_color_options(&data.colors);
        }
        Ok(true)
    }
}

fn parse_peer_color_options(
    list: &[tl_enums::help::PeerColorOption],
) -> Vec<CachedPeerColorOption> {
    list.iter()
        .map(
            |tl_enums::help::PeerColorOption::PeerColorOption(o)| CachedPeerColorOption {
                color_id: o.color_id,
                hidden: o.hidden,
                channel_min_level: o.channel_min_level,
                group_min_level: o.group_min_level,
            },
        )
        .collect()
}

// ============================================================================
// 6. WallPapers (`data/data_session.cpp:5932-5992`, `boxes/background_box.cpp:439`)
// ============================================================================

/// Cached wallpaper entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedWallPaper {
    /// Wallpaper ID.
    pub id: i64,
    /// Optional slug (empty for `wallPaperNoFile`).
    pub slug: String,
    /// Whether this is a dark-mode wallpaper.
    pub dark: bool,
    /// Whether this is a pattern wallpaper.
    pub pattern: bool,
}

/// Sync state for `account.getWallPapers(hash: i64)`.
#[derive(Debug, Clone, Default)]
pub struct WallPapersSync {
    /// Server-returned 64-bit hash.
    pub hash: i64,
    /// Cached wallpapers list.
    pub wallpapers: Vec<CachedWallPaper>,
}

impl WallPapersSync {
    /// Fetches `account.getWallPapers(hash)`. Preserves cache on `NotModified`.
    pub async fn refresh<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::account::GetWallPapers { hash: self.hash })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::account::WallPapers::NotModified => Ok(false),
            tl_enums::account::WallPapers::WallPapers(data) => {
                self.hash = data.hash;
                self.wallpapers = data
                    .wallpapers
                    .into_iter()
                    .map(|wp| match wp {
                        tl_enums::WallPaper::WallPaper(w) => CachedWallPaper {
                            id: w.id,
                            slug: w.slug,
                            dark: w.dark,
                            pattern: w.pattern,
                        },
                        tl_enums::WallPaper::NoFile(w) => CachedWallPaper {
                            id: w.id,
                            slug: String::new(),
                            dark: w.dark,
                            pattern: false,
                        },
                    })
                    .collect();
                Ok(true)
            }
        }
    }
}

// ============================================================================
// 7. Cloud Themes & Chat Themes (`data/data_cloud_themes.cpp:489-536`)
// ============================================================================

/// Cached cloud theme entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTheme {
    /// Theme ID.
    pub id: i64,
    /// Theme access hash.
    pub access_hash: i64,
    /// Theme slug.
    pub slug: String,
    /// Theme title.
    pub title: String,
    /// Optional emoticon (for chat themes).
    pub emoticon: Option<String>,
}

/// Sync state for `account.getThemes(format = "tdesktop", hash)` and `account.getChatThemes(hash)`.
#[derive(Debug, Clone, Default)]
pub struct CloudThemesSync {
    /// Server-returned 64-bit hash for `account.getThemes`.
    pub hash: i64,
    /// Cached cloud themes.
    pub themes: Vec<CachedTheme>,

    /// Server-returned 64-bit hash for `account.getChatThemes`.
    pub chat_themes_hash: i64,
    /// Cached chat themes.
    pub chat_themes: Vec<CachedTheme>,
}

impl CloudThemesSync {
    /// Fetches `account.getThemes(format = "tdesktop", hash)` (`data_cloud_themes.cpp:489-507`).
    pub async fn refresh<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::account::GetThemes {
                format: CLOUD_THEME_FORMAT.to_owned(),
                hash: self.hash,
            })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::account::Themes::NotModified => Ok(false),
            tl_enums::account::Themes::Themes(data) => {
                self.hash = data.hash;
                self.themes = data.themes.iter().map(cached_theme_from_tl).collect();
                Ok(true)
            }
        }
    }

    /// Fetches `account.getChatThemes(hash)` (`data_cloud_themes.cpp:518-536`).
    pub async fn refresh_chat_themes<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::account::GetChatThemes {
                hash: self.chat_themes_hash,
            })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::account::Themes::NotModified => Ok(false),
            tl_enums::account::Themes::Themes(data) => {
                self.chat_themes_hash = data.hash;
                self.chat_themes = data.themes.iter().map(cached_theme_from_tl).collect();
                Ok(true)
            }
        }
    }

    /// Applies `updateTheme` locally (`CloudThemes::applyUpdate`, `api_updates.cpp:2929-2932`).
    pub fn apply_update_theme(&mut self, theme: &tl_enums::Theme) {
        let updated = cached_theme_from_tl(theme);
        if let Some(existing) = self.themes.iter_mut().find(|t| t.id == updated.id) {
            *existing = updated;
        } else {
            self.themes.push(updated);
        }
    }
}

fn cached_theme_from_tl(theme: &tl_enums::Theme) -> CachedTheme {
    let tl_enums::Theme::Theme(t) = theme;
    CachedTheme {
        id: t.id,
        access_hash: t.access_hash,
        slug: t.slug.clone(),
        title: t.title.clone(),
        emoticon: t.emoticon.clone(),
    }
}

// ============================================================================
// 8. Attach Menu Bots (`inline_bots/bot_attach_web_view.cpp:2700-2733`)
// ============================================================================

/// Cached attachment-menu bot entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAttachMenuBot {
    /// Bot user ID.
    pub bot_id: i64,
    /// Short name.
    pub short_name: String,
    /// Whether the bot is inactive (not installed by user).
    pub inactive: bool,
    /// Whether shown in main menu.
    pub show_in_side_menu: bool,
    /// Whether shown in attachment menu.
    pub show_in_attach_menu: bool,
}

/// Sync state for `messages.getAttachMenuBots(hash: i64)`.
#[derive(Debug, Clone, Default)]
pub struct AttachMenuBotsSync {
    /// Server-returned 64-bit hash.
    pub hash: i64,
    /// Cached attachment menu bots.
    pub bots: Vec<CachedAttachMenuBot>,
}

impl AttachMenuBotsSync {
    /// Fetches `messages.getAttachMenuBots(hash)` (`bot_attach_web_view.cpp:2700-2733`).
    /// Applies returned `users` into `session` when provided; preserves cache on `NotModified`.
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: Option<&mut Session>,
    ) -> Result<bool, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::messages::GetAttachMenuBots { hash: self.hash })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::AttachMenuBots::NotModified => Ok(false),
            tl_enums::AttachMenuBots::AttachMenuBots(data) => {
                if let Some(s) = session {
                    apply_tl_users_and_chats(s, &data.users, &[]);
                }
                self.hash = data.hash;
                self.bots = data
                    .bots
                    .into_iter()
                    .map(
                        |tl_enums::AttachMenuBot::AttachMenuBot(b)| CachedAttachMenuBot {
                            bot_id: b.bot_id,
                            short_name: b.short_name,
                            inactive: b.inactive,
                            show_in_side_menu: b.show_in_side_menu,
                            show_in_attach_menu: b.show_in_attach_menu,
                        },
                    )
                    .collect();
                Ok(true)
            }
        }
    }
}

// ============================================================================
// 9. Premium Promo (`api/api_premium.cpp:162-209`)
// ============================================================================

/// Cached subscription period option from `help.getPremiumPromo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPremiumPeriodOption {
    /// Duration in months.
    pub months: i32,
    /// ISO currency code.
    pub currency: String,
    /// Price in smallest currency unit.
    pub amount: i64,
}

/// Sync state for `help.getPremiumPromo` (`Api::Premium`, `api/api_premium.cpp:162-209`).
#[derive(Debug, Clone, Default)]
pub struct PremiumPromoSync {
    /// Promo status text.
    pub status_text: String,
    /// Subscription period options.
    pub period_options: Vec<CachedPremiumPeriodOption>,
    /// Preview `(section, document_id)` pairs.
    pub video_sections: Vec<(String, u64)>,
}

impl PremiumPromoSync {
    /// Fetches `help.getPremiumPromo` (no hash parameter in TL) and applies `users` to `session`.
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: Option<&mut Session>,
    ) -> Result<(), ApiError> {
        let tl_enums::help::PremiumPromo::PremiumPromo(data) = rpc
            .invoke(&tl_fns::help::GetPremiumPromo)
            .await
            .map_err(Into::into)?;
        if let Some(s) = session {
            apply_tl_users_and_chats(s, &data.users, &[]);
        }
        self.status_text = data.status_text;
        self.period_options = data
            .period_options
            .into_iter()
            .map(
                |tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(o)| {
                    CachedPremiumPeriodOption {
                        months: o.months,
                        currency: o.currency,
                        amount: o.amount,
                    }
                },
            )
            .collect();
        self.video_sections = data
            .video_sections
            .into_iter()
            .zip(data.videos)
            .filter_map(|(section, doc)| match doc {
                tl_enums::Document::Document(d) => Some((section, d.id as u64)),
                tl_enums::Document::Empty(_) => None,
            })
            .collect();
        Ok(())
    }
}

// ============================================================================
// 10. Saved Ringtones (`api/api_ringtones.cpp:125-170`)
// ============================================================================

/// Sync state for `account.getSavedRingtones(hash: i64)` (`Api::Ringtones`).
#[derive(Debug, Clone, Default)]
pub struct SavedRingtonesSync {
    /// Server-returned 64-bit hash.
    pub hash: i64,
    /// Cached ringtone document IDs.
    pub ringtone_ids: Vec<u64>,
}

impl SavedRingtonesSync {
    /// Fetches `account.getSavedRingtones(hash)`. Preserves cache on `NotModified`.
    pub async fn refresh<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::account::GetSavedRingtones { hash: self.hash })
            .await
            .map_err(Into::into)?;
        match resp {
            tl_enums::account::SavedRingtones::NotModified => Ok(false),
            tl_enums::account::SavedRingtones::SavedRingtones(data) => {
                self.hash = data.hash;
                self.ringtone_ids = data
                    .ringtones
                    .iter()
                    .filter_map(|doc| match doc {
                        tl_enums::Document::Document(d) => Some(d.id as u64),
                        tl_enums::Document::Empty(_) => None,
                    })
                    .collect();
                Ok(true)
            }
        }
    }

    /// Handles `updateSavedRingtones` (`Api::Ringtones::applyUpdate`, `api_ringtones.cpp:166-170`):
    /// resets `hash = 0`, clears `ringtone_ids`, and refetches `account.getSavedRingtones(0)`.
    pub async fn on_update_saved_ringtones<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<bool, ApiError> {
        self.hash = 0;
        self.ringtone_ids.clear();
        self.refresh(rpc).await
    }
}

// ============================================================================
// 11. Quick Replies (`data/business/data_shortcut_messages.cpp:460-479`)
// ============================================================================

/// Cached quick reply shortcut metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedQuickReply {
    /// Shortcut ID.
    pub shortcut_id: i32,
    /// Shortcut trigger name.
    pub shortcut: String,
    /// Top message ID.
    pub top_message: i32,
    /// Message count in shortcut.
    pub count: i32,
}

/// Sync state for `messages.getQuickReplies(hash: i64)` (`Data::ShortcutMessages`).
#[derive(Debug, Clone, Default)]
pub struct QuickRepliesSync {
    /// Hash passed to `messages.getQuickReplies` (`0` by default, matching
    /// `data/business/data_shortcut_messages.h:142`).
    pub hash: i64,
    /// Whether initial load completed.
    pub loaded: bool,
    /// Last received timestamp (ms) for 60s throttle (`kRequestTimeLimit`).
    pub last_received_ms: u64,
    /// Shortcuts keyed by `shortcut_id`.
    pub shortcuts: BTreeMap<i32, CachedQuickReply>,
}

impl QuickRepliesSync {
    /// Fetches `messages.getQuickReplies(hash)` respecting the 60s throttle
    /// (`TooEarlyForRequest`, `data_shortcut_messages.cpp:40-42, 460-479`).
    pub async fn refresh<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: Option<&mut Session>,
        now_ms: u64,
        force: bool,
    ) -> Result<bool, ApiError> {
        if !force
            && self.last_received_ms > 0
            && now_ms
                < self
                    .last_received_ms
                    .saturating_add(QUICK_REPLIES_REFRESH_LIMIT_MS)
        {
            return Ok(false);
        }
        let resp = rpc
            .invoke(&tl_fns::messages::GetQuickReplies { hash: self.hash })
            .await
            .map_err(Into::into)?;
        self.last_received_ms = now_ms;
        match resp {
            tl_enums::messages::QuickReplies::NotModified => {
                self.loaded = true;
                Ok(true)
            }
            tl_enums::messages::QuickReplies::QuickReplies(data) => {
                if let Some(s) = session {
                    apply_tl_users_and_chats(s, &data.users, &data.chats);
                }
                self.apply_quick_replies_list(&data.quick_replies);
                self.loaded = true;
                Ok(true)
            }
        }
    }

    /// Updates local shortcuts from `QuickReply` items (`updateShortcuts`).
    pub fn apply_quick_replies_list(&mut self, list: &[tl_enums::QuickReply]) {
        self.shortcuts.clear();
        for tl_enums::QuickReply::QuickReply(qr) in list {
            self.shortcuts.insert(
                qr.shortcut_id,
                CachedQuickReply {
                    shortcut_id: qr.shortcut_id,
                    shortcut: qr.shortcut.clone(),
                    top_message: qr.top_message,
                    count: qr.count,
                },
            );
        }
    }
}

// ============================================================================
// Unified CachedListsSync coordinator (dispatches UpdateEvents + due timers)
// ============================================================================

/// Coordinates all hash-synced server lists and dispatches [`UpdateEvent`]s
/// matching `Telegram/SourceFiles/api/api_updates.cpp`.
#[derive(Debug, Clone, Default)]
pub struct CachedListsSync {
    /// Sticker sets, masks, custom emoji, featured, recent, faved, and saved GIFs.
    pub stickers: StickersSyncManager,
    /// Top, recent, and available reactions + generic animations.
    pub reactions: ReactionsSync,
    /// Default/channel/recent/collectible emoji statuses and emoji groups.
    pub emoji_statuses: EmojiStatusesSync,
    /// Multi-language emoji keywords.
    pub emoji_keywords: EmojiKeywordsSync,
    /// `help.getAppConfig` & `help.getConfig`.
    pub app_config: AppConfigSync,
    /// `help.getPeerColors` & `help.getPeerProfileColors`.
    pub peer_colors: PeerColorsSync,
    /// `account.getWallPapers`.
    pub wallpapers: WallPapersSync,
    /// `account.getThemes` & `account.getChatThemes`.
    pub themes: CloudThemesSync,
    /// `messages.getAttachMenuBots`.
    pub attach_menu_bots: AttachMenuBotsSync,
    /// `help.getPremiumPromo`.
    pub premium_promo: PremiumPromoSync,
    /// `account.getSavedRingtones`.
    pub ringtones: SavedRingtonesSync,
    /// `messages.getQuickReplies`.
    pub quick_replies: QuickRepliesSync,
}

impl CachedListsSync {
    /// Creates a fresh empty coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dispatches one [`UpdateEvent`] emitted by [`crate::UpdatesHandler`] according to
    /// tdesktop's `Api::Updates::applyUpdateNoPtsCheck` (`api/api_updates.cpp`).
    pub async fn handle_update_event<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: Option<&mut Session>,
        event: &UpdateEvent,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        match event {
            UpdateEvent::StickerSets { masks, emojis } => {
                self.stickers
                    .handle_update_sticker_sets(rpc, *masks, *emojis, now_ms)
                    .await?;
            }
            UpdateEvent::StickerSetsOrder {
                masks,
                emojis,
                order,
            } => {
                let _ = self
                    .stickers
                    .handle_update_sticker_sets_order(rpc, *masks, *emojis, order, now_ms)
                    .await?;
            }
            UpdateEvent::NewStickerSet(ss) => {
                let _ = self
                    .stickers
                    .apply_update_new_sticker_set(ss, (now_ms / 1000) as i32);
            }
            UpdateEvent::RecentStickers => {
                self.stickers
                    .handle_update_recent_stickers(rpc, now_ms)
                    .await?;
            }
            UpdateEvent::FavedStickers => {
                self.stickers
                    .handle_update_faved_stickers(rpc, now_ms)
                    .await?;
            }
            UpdateEvent::ReadFeaturedStickers => {
                self.stickers
                    .handle_update_read_featured_stickers(rpc, now_ms)
                    .await?;
            }
            UpdateEvent::SavedGifs => {
                self.stickers.handle_update_saved_gifs(rpc, now_ms).await?;
            }
            UpdateEvent::RecentReactions => {
                self.reactions.on_update_recent_reactions(now_ms);
            }
            UpdateEvent::SavedReactionTags => {
                self.reactions.on_update_saved_reaction_tags(now_ms);
            }
            UpdateEvent::RecentEmojiStatuses => {
                self.emoji_statuses.on_update_recent_emoji_statuses(now_ms);
            }
            UpdateEvent::MoveStickerSetToTop {
                masks,
                emojis,
                stickerset,
            } => {
                let _ = self
                    .stickers
                    .handle_update_move_sticker_set_to_top(
                        rpc,
                        *masks,
                        *emojis,
                        *stickerset,
                        now_ms,
                    )
                    .await?;
            }
            UpdateEvent::Config => {
                let _ = self.app_config.on_update_config(rpc, now_ms).await?;
            }
            UpdateEvent::AttachMenuBots => {
                let _ = self.attach_menu_bots.refresh(rpc, session).await?;
            }
            UpdateEvent::SavedRingtones => {
                let _ = self.ringtones.on_update_saved_ringtones(rpc).await?;
            }
            UpdateEvent::Theme(theme) => {
                self.themes.apply_update_theme(theme);
            }
            UpdateEvent::QuickReplies(list) => {
                self.quick_replies.apply_quick_replies_list(list);
            }
            _ => {}
        }
        Ok(())
    }

    /// Executes any due delayed/periodic refreshes at `now_ms`.
    pub async fn poll_due<C: RpcClient>(&mut self, rpc: &C, now_ms: u64) -> Result<(), ApiError> {
        self.reactions.poll_due(rpc, now_ms).await?;
        self.emoji_statuses.poll_due(rpc, now_ms).await?;
        if self.app_config.last_refresh_ms > 0
            && now_ms
                >= self
                    .app_config
                    .last_refresh_ms
                    .saturating_add(APP_CONFIG_REFRESH_MS)
        {
            let _ = self.app_config.refresh(rpc, now_ms, false).await?;
        }
        if self.peer_colors.last_refresh_ms > 0
            && now_ms
                >= self
                    .peer_colors
                    .last_refresh_ms
                    .saturating_add(PEER_COLORS_REFRESH_MS)
        {
            let _ = self.peer_colors.refresh(rpc, now_ms, false).await?;
        }
        if self.peer_colors.last_profile_refresh_ms > 0
            && now_ms
                >= self
                    .peer_colors
                    .last_profile_refresh_ms
                    .saturating_add(PEER_COLORS_REFRESH_MS)
        {
            let _ = self.peer_colors.refresh_profile(rpc, now_ms, false).await?;
        }
        Ok(())
    }
}
