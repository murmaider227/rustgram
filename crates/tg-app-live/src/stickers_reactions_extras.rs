//! WP-67 Stickers Search, Default Emoji Catalogs & Reactions Moderation/Privacy Protocol
//! completion for [`LiveSession`].
//!
//! Covers the 17 remaining Layer-233 sticker/emoji/reaction RPCs (13 `messages.*` + 4 `account.*`):
//! - `messages.searchStickers` (`chat_helpers/stickers_list_widget.cpp:754-776`,
//!   `chat_helpers/emoji_list_widget.cpp:941-1023`)
//! - `messages.searchEmojiStickerSets` (`chat_helpers/emoji_list_widget.cpp:924-939`)
//! - `messages.getOldFeaturedStickers` (`chat_helpers/stickers_list_widget.cpp:515-547`)
//! - `messages.clearRecentStickers` (`apiwrap.cpp:1747-1778`)
//! - `messages.getAttachedStickers` (`api/api_attached_stickers.cpp:25-90`)
//! - `messages.getEmojiKeywordsLanguages` (`chat_helpers/emoji_keywords.cpp:692-717`)
//! - `messages.getMessagesReactions` (`data/data_message_reactions.cpp:1696-1729`)
//! - `messages.getDefaultTagReactions` (`data/data_message_reactions.cpp:1079-1097`)
//! - `messages.getPaidReactionPrivacy` (`api/api_global_privacy.cpp:177-194`)
//! - `messages.togglePaidReactionPrivacy` (`data/data_message_reactions.cpp:1813-1843`)
//! - `messages.reportReaction` (`api/api_report.cpp:199-212`)
//! - `messages.deleteParticipantReaction` (`apiwrap.cpp:1584-1602`)
//! - `messages.deleteParticipantReactions` (`apiwrap.cpp:1566-1582`)
//! - `account.getDefaultProfilePhotoEmojis`, `account.getDefaultGroupPhotoEmojis`,
//!   `account.getDefaultBackgroundEmojis`, `account.getChannelRestrictedStatusEmojis`
//!   (`api/api_peer_photo.cpp:854-899`)
//!
//! Reuses `tg_api::{hash, cached_lists, stickers}` for rolling hashes and refresh intervals,
//! and routes every `Updates`-returning RPC through `SessionRuntime::apply_updates_blocking`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tg_api::RpcClient;
use tg_api::cached_lists::{
    EMOJI_KEYWORDS_REFRESH_MS, EMOJI_STATUSES_REFRESH_MS, REACTIONS_FULL_LIST_REFRESH_MS,
};
use tg_api::convert::{input_peer_for, reaction_from_tl};
use tg_api::hash::{count_document_vector_hash, hash_finalize, hash_init, hash_update_u64};
use tg_api::stickers::StickersSyncManager;
use tg_chat::{DocumentId as ChatDocId, StickerSetId};
use tg_data::{
    FullMsgId as DataFullMsgId, MessageMedia, PeerId as DataPeerId, ReactionId, Session,
    UserId as DataUserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{from_data_peer, to_data_msg, to_data_peer};
use crate::media::block_on_runtime;
use crate::session::LiveSession;
use tg_app_core::chat_helpers::backend::{
    AttachedStickerSetSummary, AttachedStickersMedia, CLOUD_SEARCH_PAGE_LIMIT, DefaultEmojiCatalog,
    DefaultEmojiCatalogKind, DefaultTagReaction, DefaultTagReactionsResult,
    OLD_FEATURED_LOAD_LIMIT, OldFeaturedStickersPage, PaidReactionPrivacy, SearchStickerSetsResult,
    SearchStickersPage, SearchStickersRequest,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Per-LiveSession Cache State
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchStickersCacheKey {
    emojis: bool,
    query: String,
    emoticon: String,
    lang_codes: Vec<String>,
    offset: i32,
    limit: i32,
}

#[derive(Debug, Clone)]
struct SearchStickersCacheEntry {
    stickers: Vec<ChatDocId>,
    next_offset: Option<i32>,
    hash: i64,
    received_ms: u64,
}

#[derive(Debug, Clone)]
struct SearchEmojiSetsCacheEntry {
    set_ids: Vec<StickerSetId>,
    hash: i64,
    received_ms: u64,
}

#[derive(Debug, Clone)]
struct OldFeaturedCacheEntry {
    set_ids: Vec<StickerSetId>,
    unread_ids: Vec<StickerSetId>,
    total_count: i32,
    next_offset: i32,
    premium: bool,
    hash: i64,
    received_ms: u64,
}

#[derive(Debug, Clone)]
struct EmojiKeywordsLanguagesCache {
    requested_codes: Vec<String>,
    remote_codes: Vec<String>,
    received_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct DefaultEmojiCatalogCacheEntry {
    document_ids: Vec<ChatDocId>,
    hash: i64,
    received_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct DefaultTagReactionsCache {
    reactions: Vec<DefaultTagReaction>,
    hash: i64,
    received_ms: u64,
}

/// Minimum polling interval for `messages.getMessagesReactions` on visible group/channel
/// messages (`kPollEach = 20 * 1000` in `data/data_message_reactions.cpp:49`).
pub const REACTIONS_POLL_INTERVAL_MS: u64 = 20_000;

#[derive(Debug, Default)]
struct StickersReactionsExtrasState {
    updates_listener_started: bool,
    search_stickers: HashMap<SearchStickersCacheKey, SearchStickersCacheEntry>,
    search_emoji_sets: HashMap<(String, bool), SearchEmojiSetsCacheEntry>,
    old_featured: HashMap<(i32, i32), OldFeaturedCacheEntry>,
    emoji_keywords_languages: Option<EmojiKeywordsLanguagesCache>,
    default_emoji_catalogs: HashMap<DefaultEmojiCatalogKind, DefaultEmojiCatalogCacheEntry>,
    default_tag_reactions: DefaultTagReactionsCache,
    paid_reaction_privacy_loaded: bool,
    paid_reaction_privacy: PaidReactionPrivacy,
    message_paid_reaction_privacy: HashMap<(PeerId, MsgId), PaidReactionPrivacy>,
    reactions_last_polled_ms: HashMap<(PeerId, MsgId), u64>,
}

struct RegistryEntry {
    owner: Weak<LiveBridgeState>,
    state: Arc<Mutex<StickersReactionsExtrasState>>,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REG: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_for_live(live: &LiveSession) -> Arc<Mutex<StickersReactionsExtrasState>> {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let state = {
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        reg.retain(|_, entry| entry.owner.strong_count() > 0);
        if let Some(existing) = reg.get(&key)
            && existing.owner.ptr_eq(&Arc::downgrade(live.bridge_state()))
        {
            Arc::clone(&existing.state)
        } else {
            let created = Arc::new(Mutex::new(StickersReactionsExtrasState::default()));
            reg.insert(
                key,
                RegistryEntry {
                    owner: Arc::downgrade(live.bridge_state()),
                    state: Arc::clone(&created),
                },
            );
            created
        }
    };
    ensure_paid_privacy_updates_listener(live, &state);
    state
}

fn ensure_paid_privacy_updates_listener(
    live: &LiveSession,
    state: &Arc<Mutex<StickersReactionsExtrasState>>,
) {
    {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.updates_listener_started {
            return;
        }
        guard.updates_listener_started = true;
    }

    let Some(mut rx) = live.runtime().rpc().subscribe_updates() else {
        return;
    };
    let state_weak = Arc::downgrade(state);
    let session_weak = Arc::downgrade(live.runtime().session());
    let bridge_weak = Arc::downgrade(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    let (Some(state_arc), Some(session_arc)) =
                        (state_weak.upgrade(), session_weak.upgrade())
                    else {
                        break;
                    };
                    let self_peer = {
                        let s = session_arc.read().await;
                        DataPeerId::from_user(s.self_user_id())
                    };
                    if let Some(privacy) = extract_paid_reaction_privacy(self_peer, &updates) {
                        let mut guard = state_arc.lock().unwrap_or_else(|e| e.into_inner());
                        guard.paid_reaction_privacy = privacy;
                        guard.paid_reaction_privacy_loaded = true;
                        drop(guard);
                        if let Some(bridge) = bridge_weak.upgrade() {
                            bridge.notify_ui();
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// =============================================================================
// Shared Conversion & Hash Helpers (reusing `tg_api::hash`)
// =============================================================================

fn now_ms_u64(live: &LiveSession) -> u64 {
    u64::try_from(live.ch_now_ms()).unwrap_or(0)
}

fn read_session<R: Send>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R: Send>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn resolve_input_peer(live: &LiveSession, peer_id: PeerId) -> Result<tl_enums::InputPeer, String> {
    let data_peer = to_data_peer(peer_id);
    let resolved = read_session(live, |session| {
        if data_peer != session.self_peer_id()
            && data_peer.to_chat().is_none()
            && session.peer(data_peer).is_none()
        {
            return tl_enums::InputPeer::Empty;
        }
        input_peer_for(session, data_peer)
    });
    if matches!(resolved, tl_enums::InputPeer::Empty) {
        return Err(format!("Peer {peer_id:?} is not loaded in session."));
    }
    Ok(resolved)
}

fn msg_id_to_i32(msg_id: MsgId) -> i32 {
    msg_id.0
}

/// Computes a featured-stickers page hash using `tg_api::hash` (matching
/// `Api::CountFeaturedStickersHash` in `api/api_hash.cpp:123-127`).
fn count_featured_page_hash(set_ids: &[StickerSetId], unread_ids: &[StickerSetId]) -> i64 {
    let mut acc = hash_init();
    for &set_id in set_ids {
        hash_update_u64(&mut acc, set_id.0);
        if unread_ids.contains(&set_id) {
            hash_update_u64(&mut acc, 1);
        }
    }
    hash_finalize(acc) as i64
}

/// Parses TL `PaidReactionPrivacy` into [`PaidReactionPrivacy`], matching
/// `Api::ParsePaidReactionShownPeer` (`api/api_global_privacy.cpp:18-42`).
#[must_use]
pub fn parse_paid_reaction_privacy(
    self_peer: DataPeerId,
    value: &tl_enums::PaidReactionPrivacy,
) -> PaidReactionPrivacy {
    match value {
        tl_enums::PaidReactionPrivacy::Default => PaidReactionPrivacy::Default,
        tl_enums::PaidReactionPrivacy::Anonymous => PaidReactionPrivacy::Anonymous,
        tl_enums::PaidReactionPrivacy::Peer(p) => match &p.peer {
            tl_enums::InputPeer::Self_ => PaidReactionPrivacy::Default,
            tl_enums::InputPeer::User(u) => {
                let target = DataPeerId::from_user(DataUserId(u.user_id as u64));
                if target == self_peer {
                    PaidReactionPrivacy::Default
                } else {
                    PaidReactionPrivacy::Peer(from_data_peer(target))
                }
            }
            tl_enums::InputPeer::Chat(c) => {
                PaidReactionPrivacy::Peer(PeerId::from_chat(c.chat_id as u64))
            }
            tl_enums::InputPeer::Channel(c) => {
                PaidReactionPrivacy::Peer(PeerId::from_channel(c.channel_id as u64))
            }
            tl_enums::InputPeer::UserFromMessage(u) => {
                let target = DataPeerId::from_user(DataUserId(u.user_id as u64));
                if target == self_peer {
                    PaidReactionPrivacy::Default
                } else {
                    PaidReactionPrivacy::Peer(from_data_peer(target))
                }
            }
            tl_enums::InputPeer::ChannelFromMessage(c) => {
                PaidReactionPrivacy::Peer(PeerId::from_channel(c.channel_id as u64))
            }
            tl_enums::InputPeer::Empty => PaidReactionPrivacy::Anonymous,
        },
    }
}

/// Converts [`PaidReactionPrivacy`] to TL `PaidReactionPrivacy`, matching
/// `PaidReactionShownPeerToTL` (`data/data_message_reactions.cpp:165-176`).
fn paid_reaction_privacy_to_tl(
    live: &LiveSession,
    privacy: PaidReactionPrivacy,
) -> Result<tl_enums::PaidReactionPrivacy, String> {
    match privacy {
        PaidReactionPrivacy::Default => Ok(tl_enums::PaidReactionPrivacy::Default),
        PaidReactionPrivacy::Anonymous => Ok(tl_enums::PaidReactionPrivacy::Anonymous),
        PaidReactionPrivacy::Peer(shown_peer) => {
            let self_peer = read_session(live, |s| DataPeerId::from_user(s.self_user_id()));
            if to_data_peer(shown_peer) == self_peer {
                Ok(tl_enums::PaidReactionPrivacy::Default)
            } else {
                let input = resolve_input_peer(live, shown_peer)?;
                Ok(tl_enums::PaidReactionPrivacy::Peer(
                    tl_types::PaidReactionPrivacyPeer { peer: input },
                ))
            }
        }
    }
}

fn extract_paid_reaction_privacy(
    self_peer: DataPeerId,
    updates: &tl_enums::Updates,
) -> Option<PaidReactionPrivacy> {
    let mut found = None;
    match updates {
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                if let tl_enums::Update::PaidReactionPrivacy(p) = upd {
                    found = Some(parse_paid_reaction_privacy(self_peer, &p.r#private));
                }
            }
        }
        tl_enums::Updates::Combined(u) => {
            for upd in &u.updates {
                if let tl_enums::Update::PaidReactionPrivacy(p) = upd {
                    found = Some(parse_paid_reaction_privacy(self_peer, &p.r#private));
                }
            }
        }
        tl_enums::Updates::UpdateShort(s) => {
            if let tl_enums::Update::PaidReactionPrivacy(p) = &s.update {
                found = Some(parse_paid_reaction_privacy(self_peer, &p.r#private));
            }
        }
        _ => {}
    }
    found
}

fn reaction_id_to_default_tag(id: ReactionId) -> DefaultTagReaction {
    match id {
        ReactionId::Emoji(e) => DefaultTagReaction::Emoji(e),
        ReactionId::Custom(doc) => DefaultTagReaction::CustomEmoji(ChatDocId(doc.0)),
        ReactionId::Paid => DefaultTagReaction::Paid,
    }
}

fn summary_from_covered(
    covered: &tl_enums::StickerSetCovered,
    hydrated_set_id: StickerSetId,
) -> AttachedStickerSetSummary {
    let (raw_set, cover_doc_ids) = match covered {
        tl_enums::StickerSetCovered::StickerSetCovered(c) => {
            let cover = match &c.cover {
                tl_enums::Document::Document(d) => vec![ChatDocId(d.id as u64)],
                tl_enums::Document::Empty(_) => Vec::new(),
            };
            (&c.set, cover)
        }
        tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => {
            let covers = c
                .covers
                .iter()
                .filter_map(|doc| match doc {
                    tl_enums::Document::Document(d) => Some(ChatDocId(d.id as u64)),
                    tl_enums::Document::Empty(_) => None,
                })
                .collect();
            (&c.set, covers)
        }
        tl_enums::StickerSetCovered::StickerSetFullCovered(c) => {
            let docs = c
                .documents
                .iter()
                .filter_map(|doc| match doc {
                    tl_enums::Document::Document(d) => Some(ChatDocId(d.id as u64)),
                    tl_enums::Document::Empty(_) => None,
                })
                .collect();
            (&c.set, docs)
        }
        tl_enums::StickerSetCovered::StickerSetNoCovered(c) => (&c.set, Vec::new()),
    };
    let tl_enums::StickerSet::StickerSet(s) = raw_set;
    AttachedStickerSetSummary {
        set_id: hydrated_set_id,
        access_hash: s.access_hash as u64,
        title: s.title.clone(),
        short_name: s.short_name.clone(),
        count: s.count,
        is_emoji: s.emojis,
        is_masks: s.masks,
        cover_doc_ids,
    }
}

fn resolve_input_stickered_media(
    live: &LiveSession,
    media: &AttachedStickersMedia,
) -> Result<tl_enums::InputStickeredMedia, String> {
    match media {
        AttachedStickersMedia::Photo {
            id,
            access_hash,
            file_reference,
        } => {
            if *id == 0 || *access_hash == 0 {
                return Err(
                    "Attached stickers photo reference is missing a valid server ID or access_hash."
                        .to_owned(),
                );
            }
            Ok(tl_enums::InputStickeredMedia::Photo(
                tl_types::InputStickeredMediaPhoto {
                    id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                        id: *id,
                        access_hash: *access_hash,
                        file_reference: file_reference.clone(),
                    }),
                },
            ))
        }
        AttachedStickersMedia::Document {
            id,
            access_hash,
            file_reference,
        } => {
            if *id == 0 || *access_hash == 0 {
                return Err(
                    "Attached stickers document reference is missing a valid server ID or access_hash."
                        .to_owned(),
                );
            }
            Ok(tl_enums::InputStickeredMedia::Document(
                tl_types::InputStickeredMediaDocument {
                    id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                        id: *id,
                        access_hash: *access_hash,
                        file_reference: file_reference.clone(),
                    }),
                },
            ))
        }
        AttachedStickersMedia::Message { peer_id, msg_id } => {
            let full_id = DataFullMsgId::new(to_data_peer(*peer_id), to_data_msg(*msg_id));
            read_session(live, |session| {
                let msg = session.message(full_id).ok_or_else(|| {
                    format!("Message {msg_id:?} in {peer_id:?} is not loaded in session.")
                })?;
                match msg.media() {
                    Some(MessageMedia::Photo(photo)) => Ok(tl_enums::InputStickeredMedia::Photo(
                        tl_types::InputStickeredMediaPhoto {
                            id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                                id: photo.id.0 as i64,
                                access_hash: photo.access_hash as i64,
                                file_reference: photo.file_reference.clone(),
                            }),
                        },
                    )),
                    Some(MessageMedia::Document(doc)) => {
                        Ok(tl_enums::InputStickeredMedia::Document(
                            tl_types::InputStickeredMediaDocument {
                                id: tl_enums::InputDocument::InputDocument(
                                    tl_types::InputDocument {
                                        id: doc.id.0 as i64,
                                        access_hash: doc.access_hash as i64,
                                        file_reference: doc.file_reference.clone(),
                                    },
                                ),
                            },
                        ))
                    }
                    _ => Err(format!(
                        "Message {msg_id:?} does not contain a photo or document with attached stickers."
                    )),
                }
            })
        }
    }
}

// =============================================================================
// 17 WP-67 Live RPC Methods on `LiveSession`
// =============================================================================

impl LiveSession {
    /// `messages.searchStickers` — remote sticker / custom-emoji search with hash caching,
    /// `NotModified` handling, and document pool hydration
    /// (`chat_helpers/stickers_list_widget.cpp:754-776`, `emoji_list_widget.cpp:941-1023`).
    pub fn search_stickers_remote_live(
        &self,
        req: &SearchStickersRequest,
    ) -> Result<SearchStickersPage, String> {
        let q = req.query.trim();
        let emoticon = req.emoticon.trim();
        if q.is_empty() && emoticon.is_empty() {
            return Ok(SearchStickersPage::default());
        }
        let offset = req.offset.max(0);
        let limit = if req.limit > 0 {
            req.limit
        } else {
            CLOUD_SEARCH_PAGE_LIMIT
        };
        let cache_key = SearchStickersCacheKey {
            emojis: req.emojis,
            query: q.to_lowercase(),
            emoticon: emoticon.to_owned(),
            lang_codes: req.lang_codes.clone(),
            offset,
            limit,
        };
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let (should_fetch, prev_hash, cached) = {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = g.search_stickers.get(&cache_key) {
                let stale = StickersSyncManager::update_needed(entry.received_ms, now_ms);
                (stale, entry.hash, Some(entry.clone()))
            } else {
                (true, 0, None)
            }
        };
        if !should_fetch && let Some(entry) = cached {
            return Ok(SearchStickersPage {
                stickers: entry.stickers,
                next_offset: entry.next_offset,
                hash: entry.hash,
                not_modified: false,
            });
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::SearchStickers {
            emojis: req.emojis,
            q: q.to_owned(),
            emoticon: emoticon.to_owned(),
            lang_code: req.lang_codes.clone(),
            offset,
            limit,
            hash: prev_hash,
        });
        match resp {
            Ok(tl_enums::messages::FoundStickers::NotModified(nm)) => {
                let next_offset = nm.next_offset.filter(|&n| n > offset);
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                let entry = g.search_stickers.entry(cache_key).or_insert_with(|| {
                    SearchStickersCacheEntry {
                        stickers: Vec::new(),
                        next_offset,
                        hash: prev_hash,
                        received_ms: now_ms,
                    }
                });
                entry.received_ms = now_ms;
                entry.next_offset = next_offset;
                Ok(SearchStickersPage {
                    stickers: entry.stickers.clone(),
                    next_offset: entry.next_offset,
                    hash: entry.hash,
                    not_modified: true,
                })
            }
            Ok(tl_enums::messages::FoundStickers::FoundStickers(found)) => {
                let stickers = self.ch_ingest_documents(&found.stickers, None);
                let next_offset = found.next_offset.filter(|&n| n > offset);
                let effective_hash = if found.hash != 0 || stickers.is_empty() {
                    found.hash
                } else {
                    let raw_ids: Vec<u64> = stickers.iter().map(|id| id.0).collect();
                    count_document_vector_hash(&raw_ids) as i64
                };
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.search_stickers.insert(
                    cache_key,
                    SearchStickersCacheEntry {
                        stickers: stickers.clone(),
                        next_offset,
                        hash: effective_hash,
                        received_ms: now_ms,
                    },
                );
                Ok(SearchStickersPage {
                    stickers,
                    next_offset,
                    hash: effective_hash,
                    not_modified: false,
                })
            }
            Err(err) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = g.search_stickers.get_mut(&cache_key) {
                    entry.hash = 0;
                }
                debug!(error = %err, "messages.searchStickers failed");
                Err(err)
            }
        }
    }

    /// `messages.searchEmojiStickerSets` — remote custom-emoji pack search with hash caching,
    /// `NotModified` handling, and document pool hydration
    /// (`chat_helpers/emoji_list_widget.cpp:924-939`).
    pub fn search_emoji_sticker_sets_live(
        &self,
        query: &str,
        exclude_featured: bool,
    ) -> Result<SearchStickerSetsResult, String> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(SearchStickerSetsResult::default());
        }
        let cache_key = (q.to_lowercase(), exclude_featured);
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let (should_fetch, prev_hash, cached) = {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = g.search_emoji_sets.get(&cache_key) {
                let stale = StickersSyncManager::update_needed(entry.received_ms, now_ms);
                (stale, entry.hash, Some(entry.clone()))
            } else {
                (true, 0, None)
            }
        };
        if !should_fetch && let Some(entry) = cached {
            return Ok(SearchStickerSetsResult {
                set_ids: entry.set_ids,
                hash: entry.hash,
                not_modified: false,
            });
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::SearchEmojiStickerSets {
            exclude_featured,
            q: q.to_owned(),
            hash: prev_hash,
        });
        match resp {
            Ok(tl_enums::messages::FoundStickerSets::NotModified) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                let entry = g.search_emoji_sets.entry(cache_key).or_insert_with(|| {
                    SearchEmojiSetsCacheEntry {
                        set_ids: Vec::new(),
                        hash: prev_hash,
                        received_ms: now_ms,
                    }
                });
                entry.received_ms = now_ms;
                Ok(SearchStickerSetsResult {
                    set_ids: entry.set_ids.clone(),
                    hash: entry.hash,
                    not_modified: true,
                })
            }
            Ok(tl_enums::messages::FoundStickerSets::FoundStickerSets(found)) => {
                let set_ids = self.ch_ingest_covered_sets(&found.sets, true);
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.search_emoji_sets.insert(
                    cache_key,
                    SearchEmojiSetsCacheEntry {
                        set_ids: set_ids.clone(),
                        hash: found.hash,
                        received_ms: now_ms,
                    },
                );
                Ok(SearchStickerSetsResult {
                    set_ids,
                    hash: found.hash,
                    not_modified: false,
                })
            }
            Err(err) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = g.search_emoji_sets.get_mut(&cache_key) {
                    entry.hash = 0;
                }
                debug!(error = %err, "messages.searchEmojiStickerSets failed");
                Err(err)
            }
        }
    }

    /// `messages.getOldFeaturedStickers` — paginated archive of older featured sticker sets
    /// with hash caching, `NotModified` handling, and document pool hydration
    /// (`chat_helpers/stickers_list_widget.cpp:515-547`).
    pub fn load_old_featured_stickers_live(
        &self,
        offset: i32,
        limit: i32,
    ) -> Result<OldFeaturedStickersPage, String> {
        let offset = offset.max(0);
        let limit = if limit > 0 {
            limit
        } else {
            OLD_FEATURED_LOAD_LIMIT
        };
        let cache_key = (offset, limit);
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let (should_fetch, prev_hash, cached) = {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = g.old_featured.get(&cache_key) {
                let stale = StickersSyncManager::update_needed(entry.received_ms, now_ms);
                (stale, entry.hash, Some(entry.clone()))
            } else {
                (true, 0, None)
            }
        };
        if !should_fetch && let Some(entry) = cached {
            return Ok(OldFeaturedStickersPage {
                set_ids: entry.set_ids,
                unread_ids: entry.unread_ids,
                total_count: entry.total_count,
                next_offset: entry.next_offset,
                premium: entry.premium,
                hash: entry.hash,
                not_modified: false,
            });
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetOldFeaturedStickers {
            offset,
            limit,
            hash: prev_hash,
        });
        match resp {
            Ok(tl_enums::messages::FeaturedStickers::NotModified(nm)) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                let entry =
                    g.old_featured
                        .entry(cache_key)
                        .or_insert_with(|| OldFeaturedCacheEntry {
                            set_ids: Vec::new(),
                            unread_ids: Vec::new(),
                            total_count: nm.count,
                            next_offset: offset,
                            premium: false,
                            hash: prev_hash,
                            received_ms: now_ms,
                        });
                entry.received_ms = now_ms;
                entry.total_count = nm.count;
                Ok(OldFeaturedStickersPage {
                    set_ids: entry.set_ids.clone(),
                    unread_ids: entry.unread_ids.clone(),
                    total_count: entry.total_count,
                    next_offset: entry.next_offset,
                    premium: entry.premium,
                    hash: entry.hash,
                    not_modified: true,
                })
            }
            Ok(tl_enums::messages::FeaturedStickers::FeaturedStickers(featured)) => {
                let set_ids = self.ch_ingest_old_featured_sets(&featured);
                let unread_ids: Vec<StickerSetId> = featured
                    .unread
                    .iter()
                    .map(|&id| StickerSetId(id as u64))
                    .collect();
                let next_offset = offset.saturating_add(i32::try_from(set_ids.len()).unwrap_or(0));
                let effective_hash = if featured.hash != 0 || set_ids.is_empty() {
                    featured.hash
                } else {
                    count_featured_page_hash(&set_ids, &unread_ids)
                };
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.old_featured.insert(
                    cache_key,
                    OldFeaturedCacheEntry {
                        set_ids: set_ids.clone(),
                        unread_ids: unread_ids.clone(),
                        total_count: featured.count,
                        next_offset,
                        premium: featured.premium,
                        hash: effective_hash,
                        received_ms: now_ms,
                    },
                );
                Ok(OldFeaturedStickersPage {
                    set_ids,
                    unread_ids,
                    total_count: featured.count,
                    next_offset,
                    premium: featured.premium,
                    hash: effective_hash,
                    not_modified: false,
                })
            }
            Err(err) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = g.old_featured.get_mut(&cache_key) {
                    entry.hash = 0;
                }
                debug!(error = %err, "messages.getOldFeaturedStickers failed");
                Err(err)
            }
        }
    }

    /// `messages.clearRecentStickers` — clears cloud recent stickers (`attached = false`) or
    /// recent attached masks (`attached = true`) and updates the local `StickersStore`
    /// (`apiwrap.cpp:1747-1778`).
    pub fn clear_recent_stickers_live(&mut self, attached: bool) -> Result<bool, String> {
        let cleared = self.ch_invoke_rpc(tl_fns::messages::ClearRecentStickers { attached })?;
        if cleared {
            self.ch_clear_recent_stickers_local(attached);
            self.bridge_state().notify_ui();
        }
        Ok(cleared)
    }

    /// `messages.getAttachedStickers` — resolves sticker/mask/emoji sets attached to a photo
    /// or document and hydrates returned sets/covers into `StickersStore` + `doc_pool`
    /// (`api/api_attached_stickers.cpp:25-90`).
    pub fn get_attached_stickers_live(
        &self,
        media: &AttachedStickersMedia,
    ) -> Result<Vec<AttachedStickerSetSummary>, String> {
        let input_media = resolve_input_stickered_media(self, media)?;
        let covered =
            self.ch_invoke_rpc(tl_fns::messages::GetAttachedStickers { media: input_media })?;
        let set_ids = self.ch_ingest_covered_sets(&covered, false);
        Ok(covered
            .iter()
            .zip(set_ids)
            .map(|(item, set_id)| summary_from_covered(item, set_id))
            .collect())
    }

    /// `messages.getEmojiKeywordsLanguages` — resolves server-supported emoji keyword
    /// language codes (`chat_helpers/emoji_keywords.cpp:692-717`) with 1-hour TTL caching
    /// (`tg_api::cached_lists::EMOJI_KEYWORDS_REFRESH_MS`).
    pub fn emoji_keywords_languages_live(
        &self,
        lang_codes: &[String],
        force: bool,
    ) -> Result<Vec<String>, String> {
        let normalized: Vec<String> = lang_codes
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        if !force {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cache) = &g.emoji_keywords_languages
                && cache.requested_codes == normalized
                && cache.received_ms > 0
                && now_ms.saturating_sub(cache.received_ms) < EMOJI_KEYWORDS_REFRESH_MS
            {
                return Ok(cache.remote_codes.clone());
            }
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetEmojiKeywordsLanguages {
            lang_codes: normalized.clone(),
        })?;
        let remote_codes: Vec<String> = resp
            .into_iter()
            .map(|tl_enums::EmojiLanguage::EmojiLanguage(lang)| lang.lang_code)
            .collect();
        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        g.emoji_keywords_languages = Some(EmojiKeywordsLanguagesCache {
            requested_codes: normalized,
            remote_codes: remote_codes.clone(),
            received_ms: now_ms,
        });
        Ok(remote_codes)
    }

    /// Best-effort emoji-keyword language refresh triggered by live emoji-keyword lookup
    /// (`EmojiKeywords::refresh` -> `refreshRemoteList`, `chat_helpers/emoji_keywords.cpp:552-560, 692-717`).
    ///
    /// Respects [`EMOJI_KEYWORDS_REFRESH_MS`] (1 hour) per active language list; on RPC failure
    /// caches the failed timestamp for that list so repeated keystrokes do not flood RPCs.
    pub(crate) fn ensure_emoji_keywords_languages_refreshed(&self) {
        let active_lang = {
            let code = self.langpack().active_language_code();
            if code.trim().is_empty() {
                "en".to_owned()
            } else {
                code
            }
        };
        let mut lang_codes = vec![active_lang];
        if !lang_codes.iter().any(|c| c.eq_ignore_ascii_case("en")) {
            lang_codes.push("en".to_owned());
        }
        if let Err(err) = self.emoji_keywords_languages_live(&lang_codes, false) {
            debug!(error = %err, "auto messages.getEmojiKeywordsLanguages refresh failed");
            let state = state_for_live(self);
            let now_ms = now_ms_u64(self);
            let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
            if g.emoji_keywords_languages
                .as_ref()
                .is_none_or(|c| c.requested_codes != lang_codes)
            {
                g.emoji_keywords_languages = Some(EmojiKeywordsLanguagesCache {
                    requested_codes: lang_codes,
                    remote_codes: Vec::new(),
                    received_ms: now_ms,
                });
            } else if let Some(cache) = g.emoji_keywords_languages.as_mut() {
                cache.received_ms = now_ms;
            }
        }
    }

    /// `account.getDefaultProfilePhotoEmojis`, `account.getDefaultGroupPhotoEmojis`,
    /// `account.getDefaultBackgroundEmojis`, and `account.getChannelRestrictedStatusEmojis` —
    /// hash-cached default custom-emoji catalogs (`api/api_peer_photo.cpp:854-899`).
    pub fn default_emoji_catalog_live(
        &self,
        kind: DefaultEmojiCatalogKind,
        force: bool,
    ) -> Result<DefaultEmojiCatalog, String> {
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let (should_fetch, prev_hash, cached) = {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = g.default_emoji_catalogs.get(&kind) {
                let fresh = entry.received_ms > 0
                    && now_ms < entry.received_ms.saturating_add(EMOJI_STATUSES_REFRESH_MS);
                (force || !fresh, entry.hash, Some(entry.clone()))
            } else {
                (true, 0, None)
            }
        };
        if !should_fetch && let Some(entry) = cached {
            return Ok(DefaultEmojiCatalog {
                document_ids: entry.document_ids,
                hash: entry.hash,
                not_modified: false,
            });
        }

        let resp = match kind {
            DefaultEmojiCatalogKind::ProfilePhoto => self
                .ch_invoke_rpc(tl_fns::account::GetDefaultProfilePhotoEmojis { hash: prev_hash }),
            DefaultEmojiCatalogKind::GroupPhoto => {
                self.ch_invoke_rpc(tl_fns::account::GetDefaultGroupPhotoEmojis { hash: prev_hash })
            }
            DefaultEmojiCatalogKind::Background => {
                self.ch_invoke_rpc(tl_fns::account::GetDefaultBackgroundEmojis { hash: prev_hash })
            }
            DefaultEmojiCatalogKind::ChannelRestrictedStatus => {
                self.ch_invoke_rpc(tl_fns::account::GetChannelRestrictedStatusEmojis {
                    hash: prev_hash,
                })
            }
        };

        match resp {
            Ok(tl_enums::EmojiList::NotModified) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                let entry = g.default_emoji_catalogs.entry(kind).or_default();
                entry.received_ms = now_ms;
                Ok(DefaultEmojiCatalog {
                    document_ids: entry.document_ids.clone(),
                    hash: entry.hash,
                    not_modified: true,
                })
            }
            Ok(tl_enums::EmojiList::EmojiList(list)) => {
                let raw_ids: Vec<u64> = list.document_id.iter().map(|&id| id as u64).collect();
                let document_ids: Vec<ChatDocId> = raw_ids.iter().copied().map(ChatDocId).collect();
                let effective_hash = if list.hash != 0 || raw_ids.is_empty() {
                    list.hash
                } else {
                    count_document_vector_hash(&raw_ids) as i64
                };
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.default_emoji_catalogs.insert(
                    kind,
                    DefaultEmojiCatalogCacheEntry {
                        document_ids: document_ids.clone(),
                        hash: effective_hash,
                        received_ms: now_ms,
                    },
                );
                Ok(DefaultEmojiCatalog {
                    document_ids,
                    hash: effective_hash,
                    not_modified: false,
                })
            }
            Err(err) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = g.default_emoji_catalogs.get_mut(&kind) {
                    entry.hash = 0;
                }
                debug!(?kind, error = %err, "account.getDefault*Emojis failed");
                Err(err)
            }
        }
    }

    /// `messages.getMessagesReactions` — refreshes reactions for `msg_ids` in `peer_id`
    /// and routes the returned `Updates` through `SessionRuntime::apply_updates_blocking`
    /// (`data/data_message_reactions.cpp:1696-1729`).
    pub fn refresh_messages_reactions_live(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<(), String> {
        if msg_ids.is_empty() {
            return Ok(());
        }
        let peer = resolve_input_peer(self, peer_id)?;
        let ids: Vec<i32> = msg_ids.iter().copied().map(msg_id_to_i32).collect();
        let updates =
            self.ch_invoke_rpc(tl_fns::messages::GetMessagesReactions { peer, id: ids })?;
        self.runtime()
            .apply_updates_blocking(updates)
            .map_err(|e| format!("failed to apply messages.getMessagesReactions updates: {e}"))?;
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        for &mid in msg_ids {
            g.reactions_last_polled_ms.insert((peer_id, mid), now_ms);
        }
        drop(g);
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Polls visible messages in a group/channel that already carry reactions via
    /// `messages.getMessagesReactions`, respecting tdesktop's 20-second `kPollEach` interval
    /// and skipping 1:1 user peers (`Data::Reactions::poll` & `pollCollected`,
    /// `data/data_message_reactions.cpp:1512-1535, 1696-1729`).
    ///
    /// Returns the subset of `msg_ids` that were actually polled on this call.
    pub fn poll_visible_messages_reactions_live(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        let due = self.due_reaction_polls(peer_id, msg_ids, false);
        self.poll_reactions_of_live(peer_id, &due)
    }

    /// The ids of `msg_ids` whose reactions were not polled during the last `kPollEach` (none
    /// in a 1:1 chat). With `mark`, they count as polled now: the history calls this for its
    /// painted messages every frame (`HistoryInner::paintEvent` -> `Reactions::poll`), so a
    /// visible message costs one map lookup until it is due again.
    pub(crate) fn due_reaction_polls(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        mark: bool,
    ) -> Vec<MsgId> {
        if to_data_peer(peer_id).is_user() || msg_ids.is_empty() {
            return Vec::new();
        }
        let now_ms = now_ms_u64(self);
        let state = state_for_live(self);
        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        let mut due = Vec::with_capacity(msg_ids.len());
        for &mid in msg_ids {
            if mid.0 <= 0 || due.contains(&mid) {
                continue;
            }
            let is_due = g
                .reactions_last_polled_ms
                .get(&(peer_id, mid))
                .is_none_or(|&last| now_ms.saturating_sub(last) >= REACTIONS_POLL_INTERVAL_MS);
            if is_due {
                due.push(mid);
            }
        }
        if mark {
            for &mid in &due {
                g.reactions_last_polled_ms.insert((peer_id, mid), now_ms);
            }
        }
        due
    }

    /// Sends `messages.getMessagesReactions` for the messages of `candidates` that ever carried
    /// a reactions object (`HistoryItem::lastReactionsRefreshTime() != 0`: an empty list counts,
    /// a channel post with no reactions yet gets them this way). Returns the polled ids.
    pub(crate) fn poll_reactions_of_live(
        &self,
        peer_id: PeerId,
        candidates: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        let data_peer = to_data_peer(peer_id);
        if data_peer.is_user() || candidates.is_empty() {
            return Ok(Vec::new());
        }
        let with_reactions: Vec<MsgId> = read_session(self, |session| {
            candidates
                .iter()
                .copied()
                .filter(|&mid| {
                    let full_id = DataFullMsgId::new(data_peer, to_data_msg(mid));
                    session
                        .message(full_id)
                        .is_some_and(|m| m.reactions.is_some())
                })
                .collect()
        });
        if with_reactions.is_empty() {
            return Ok(Vec::new());
        }
        // Record poll attempt timestamp before invoking so transient errors also respect
        // `kPollEach` instead of re-polling on every frame.
        {
            let now_ms = now_ms_u64(self);
            let state = state_for_live(self);
            let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
            for &mid in &with_reactions {
                g.reactions_last_polled_ms.insert((peer_id, mid), now_ms);
            }
        }
        self.refresh_messages_reactions_live(peer_id, &with_reactions)?;
        Ok(with_reactions)
    }

    /// `messages.getDefaultTagReactions` — hash-cached default Saved Messages tag reactions
    /// (`data/data_message_reactions.cpp:1079-1097`), resetting `hash = 0` on RPC error.
    pub fn default_tag_reactions_live(
        &self,
        force: bool,
    ) -> Result<DefaultTagReactionsResult, String> {
        let state = state_for_live(self);
        let now_ms = now_ms_u64(self);
        let (should_fetch, prev_hash, cached) = {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            let entry = &g.default_tag_reactions;
            let fresh = entry.received_ms > 0
                && now_ms
                    < entry
                        .received_ms
                        .saturating_add(REACTIONS_FULL_LIST_REFRESH_MS);
            (force || !fresh, entry.hash, entry.clone())
        };
        if !should_fetch {
            return Ok(DefaultTagReactionsResult {
                reactions: cached.reactions,
                hash: cached.hash,
                not_modified: false,
            });
        }

        let resp = self.ch_invoke_rpc(tl_fns::messages::GetDefaultTagReactions { hash: prev_hash });
        match resp {
            Ok(tl_enums::messages::Reactions::NotModified) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.default_tag_reactions.received_ms = now_ms;
                Ok(DefaultTagReactionsResult {
                    reactions: g.default_tag_reactions.reactions.clone(),
                    hash: g.default_tag_reactions.hash,
                    not_modified: true,
                })
            }
            Ok(tl_enums::messages::Reactions::Reactions(data)) => {
                let reactions: Vec<DefaultTagReaction> = data
                    .reactions
                    .iter()
                    .filter_map(reaction_from_tl)
                    .map(reaction_id_to_default_tag)
                    .collect();
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.default_tag_reactions = DefaultTagReactionsCache {
                    reactions: reactions.clone(),
                    hash: data.hash,
                    received_ms: now_ms,
                };
                Ok(DefaultTagReactionsResult {
                    reactions,
                    hash: data.hash,
                    not_modified: false,
                })
            }
            Err(err) => {
                let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
                g.default_tag_reactions.hash = 0;
                debug!(error = %err, "messages.getDefaultTagReactions failed");
                Err(err)
            }
        }
    }

    /// `messages.getPaidReactionPrivacy` — fetches account-wide default paid-reaction
    /// privacy (`api/api_global_privacy.cpp:177-194`), routing returned `Updates` through
    /// `SessionRuntime::apply_updates_blocking` and caching `Update::PaidReactionPrivacy`.
    pub fn paid_reaction_privacy_live(&self, force: bool) -> Result<PaidReactionPrivacy, String> {
        let state = state_for_live(self);
        if !force {
            let g = state.lock().unwrap_or_else(|e| e.into_inner());
            if g.paid_reaction_privacy_loaded {
                return Ok(g.paid_reaction_privacy);
            }
        }

        let updates = self.ch_invoke_rpc(tl_fns::messages::GetPaidReactionPrivacy {})?;
        let self_peer = read_session(self, |s| DataPeerId::from_user(s.self_user_id()));
        let parsed = extract_paid_reaction_privacy(self_peer, &updates);
        self.runtime()
            .apply_updates_blocking(updates)
            .map_err(|e| format!("failed to apply messages.getPaidReactionPrivacy updates: {e}"))?;

        let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(privacy) = parsed {
            g.paid_reaction_privacy = privacy;
        }
        g.paid_reaction_privacy_loaded = true;
        let current = g.paid_reaction_privacy;
        drop(g);
        self.bridge_state().notify_ui();
        Ok(current)
    }

    /// Returns the last toggled paid-reaction privacy for `(peer_id, msg_id)`, if any.
    #[must_use]
    pub fn cached_message_paid_reaction_privacy(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<PaidReactionPrivacy> {
        let state = state_for_live(self);
        let g = state.lock().unwrap_or_else(|e| e.into_inner());
        g.message_paid_reaction_privacy
            .get(&(peer_id, msg_id))
            .copied()
    }

    /// `messages.togglePaidReactionPrivacy` — changes paid-reaction leaderboard privacy
    /// on a specific message (`data/data_message_reactions.cpp:1813-1843`).
    pub fn toggle_paid_reaction_privacy_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        privacy: PaidReactionPrivacy,
    ) -> Result<bool, String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let msg_id_i32 = msg_id_to_i32(msg_id);
        let tl_privacy = paid_reaction_privacy_to_tl(self, privacy)?;
        let ok = self.ch_invoke_rpc(tl_fns::messages::TogglePaidReactionPrivacy {
            peer,
            msg_id: msg_id_i32,
            r#private: tl_privacy,
        })?;
        if ok {
            let state = state_for_live(self);
            let mut g = state.lock().unwrap_or_else(|e| e.into_inner());
            g.message_paid_reaction_privacy
                .insert((peer_id, msg_id), privacy);
            drop(g);
            self.bridge_state().notify_ui();
        }
        Ok(ok)
    }

    /// `messages.reportReaction` — reports a participant's reaction on a message
    /// (`api/api_report.cpp:199-212`).
    pub fn report_reaction_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        reaction_peer_id: PeerId,
    ) -> Result<bool, String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let id = msg_id_to_i32(msg_id);
        let reaction_peer = resolve_input_peer(self, reaction_peer_id)?;
        self.ch_invoke_rpc(tl_fns::messages::ReportReaction {
            peer,
            id,
            reaction_peer,
        })
    }

    /// `messages.deleteParticipantReaction` — removes a participant's reaction from one
    /// message (`apiwrap.cpp:1584-1602`) and routes returned `Updates` through
    /// `SessionRuntime::apply_updates_blocking`.
    pub fn delete_participant_reaction_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        participant_id: PeerId,
    ) -> Result<(), String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let msg_id_i32 = msg_id_to_i32(msg_id);
        let participant = resolve_input_peer(self, participant_id)?;
        let updates = self.ch_invoke_rpc(tl_fns::messages::DeleteParticipantReaction {
            peer,
            msg_id: msg_id_i32,
            participant,
        })?;
        self.runtime()
            .apply_updates_blocking(updates)
            .map_err(|e| {
                format!("failed to apply messages.deleteParticipantReaction updates: {e}")
            })?;
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// `messages.deleteParticipantReactions` — removes all reactions by `participant_id`
    /// across `peer_id` (`apiwrap.cpp:1566-1582`) and optimistically strips `participant_id`'s
    /// reactions across cached messages in `peer_id`, matching
    /// `Session::removeReactionsFromParticipant` (`data/data_session.cpp:3518-3544`).
    pub fn delete_participant_reactions_live(
        &mut self,
        peer_id: PeerId,
        participant_id: PeerId,
    ) -> Result<bool, String> {
        let peer = resolve_input_peer(self, peer_id)?;
        let participant = resolve_input_peer(self, participant_id)?;
        let ok =
            self.ch_invoke_rpc(tl_fns::messages::DeleteParticipantReactions { peer, participant })?;
        if ok {
            let data_peer = to_data_peer(peer_id);
            let data_participant = to_data_peer(participant_id);
            write_session(self, |s| {
                s.remove_reactions_from_participant(data_peer, None, data_participant, None);
            });
            self.bridge_state().notify_ui();
        }
        Ok(ok)
    }
}
