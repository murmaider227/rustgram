//! Search protocol layer: in-chat `messages.search` (including migrated supergroups via
//! `MessagesSearchMerged` and shared-media bidirectional paging), global `messages.searchGlobal`,
//! `messages.getSearchCounters`, peer directory `contacts.search`, and hashtag/public-posts
//! `channels.searchPosts` + `channels.checkSearchPostsFlood`.
//!
//! Port origin: `api/api_messages_search.cpp`, `api/api_messages_search_merged.cpp`,
//! `api/api_peer_search.cpp`, `api/api_single_message_search.cpp`,
//! `data/data_search_controller.cpp`, `dialogs/dialogs_search_posts.cpp`,
//! `dialogs/dialogs_widget.cpp`, `dialogs/ui/chat_search_in.cpp`.

use std::collections::HashMap;

use tg_data::{
    FullMsgId, MsgId, MsgRange, Peer, PeerId, ReactionId, SERVER_MAX_MSG_ID, Session, TimeId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_message, apply_tl_users_and_chats, input_peer_for, peer_id_from_tl, reaction_to_tl,
    tl_message_meta,
};
use crate::error::ApiError;
use crate::forum::apply_tl_forum_topic;
use crate::history::MAX_OFFSET_ID;
use crate::rpc::{InvocationError, RpcClient};
use crate::types::LoadDirection;
use crate::updates::UpdatesHandler;

/// In-chat and global search page size (`kSearchPerPage` in `api_messages_search.cpp:23`,
/// `dialogs_widget.cpp:116`, `dialogs_search_posts.cpp:21`).
pub const SEARCH_PER_PAGE: i32 = 50;

/// Shared media slice limit for subsequent pages (`kSharedMediaLimit` in `data_search_controller.cpp:22`).
pub const SHARED_MEDIA_LIMIT: i32 = 100;

/// Shared media slice limit when `message_id == 0` (count-only initial query,
/// `kFirstSharedMediaLimit` in `data_search_controller.cpp:23`).
pub const FIRST_SHARED_MEDIA_LIMIT: i32 = 0;

/// Default peer search limit (`SearchPeopleLimit` in `config.h:23`).
pub const SEARCH_PEOPLE_LIMIT: i32 = 5;

/// Typed peer search limit (`kTypedLimit` for channels/bots in `api_peer_search.cpp:20`).
pub const PEER_SEARCH_TYPED_LIMIT: i32 = 50;

/// Default search input debounce delay (`kDefaultSearchTimeoutMs` in `data_search_controller.cpp:25`).
pub const DEFAULT_SEARCH_TIMEOUT_MS: u64 = 200;

/// Delay before querying `channels.checkSearchPostsFlood` (`kQueryDelay` in `dialogs_search_posts.cpp:20`).
pub const POSTS_SEARCH_QUERY_DELAY_MS: u64 = 500;

// ============================================================================
// Filters, Hashtag/Cashtag Classification, and Peer Search Query Conversion
// ============================================================================

/// Shared media / message search filter (`Storage::SharedMediaType` in `data_search_controller.cpp:29-60`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SharedMediaFilter {
    /// `inputMessagesFilterEmpty`.
    #[default]
    Empty,
    /// `inputMessagesFilterPhotos`.
    Photo,
    /// `inputMessagesFilterVideo`.
    Video,
    /// `inputMessagesFilterPhotoVideo`.
    PhotoVideo,
    /// `inputMessagesFilterMusic`.
    MusicFile,
    /// `inputMessagesFilterDocument`.
    File,
    /// `inputMessagesFilterVoice`.
    VoiceFile,
    /// `inputMessagesFilterRoundVoice`.
    RoundVoiceFile,
    /// `inputMessagesFilterRoundVideo`.
    RoundFile,
    /// `inputMessagesFilterGif`.
    Gif,
    /// `inputMessagesFilterUrl`.
    Link,
    /// `inputMessagesFilterChatPhotos`.
    ChatPhoto,
    /// `inputMessagesFilterPinned`.
    Pinned,
    /// `inputMessagesFilterPoll`.
    Poll,
}

impl SharedMediaFilter {
    /// Converts to the exact TL `MessagesFilter` (`Api::PrepareSearchFilter` in
    /// `data_search_controller.cpp:29-60`).
    #[must_use]
    pub const fn to_tl(self) -> tl_enums::MessagesFilter {
        match self {
            Self::Empty => tl_enums::MessagesFilter::InputMessagesFilterEmpty,
            Self::Photo => tl_enums::MessagesFilter::InputMessagesFilterPhotos,
            Self::Video => tl_enums::MessagesFilter::InputMessagesFilterVideo,
            Self::PhotoVideo => tl_enums::MessagesFilter::InputMessagesFilterPhotoVideo,
            Self::MusicFile => tl_enums::MessagesFilter::InputMessagesFilterMusic,
            Self::File => tl_enums::MessagesFilter::InputMessagesFilterDocument,
            Self::VoiceFile => tl_enums::MessagesFilter::InputMessagesFilterVoice,
            Self::RoundVoiceFile => tl_enums::MessagesFilter::InputMessagesFilterRoundVoice,
            Self::RoundFile => tl_enums::MessagesFilter::InputMessagesFilterRoundVideo,
            Self::Gif => tl_enums::MessagesFilter::InputMessagesFilterGif,
            Self::Link => tl_enums::MessagesFilter::InputMessagesFilterUrl,
            Self::ChatPhoto => tl_enums::MessagesFilter::InputMessagesFilterChatPhotos,
            Self::Pinned => tl_enums::MessagesFilter::InputMessagesFilterPinned,
            Self::Poll => tl_enums::MessagesFilter::InputMessagesFilterPoll,
        }
    }
}

/// Classification of a hashtag or cashtag search query (`Dialogs::HashOrCashtag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashOrCashtag {
    /// Regular query.
    None,
    /// `#hashtag` (starts with `#` and contains no whitespace).
    Hashtag,
    /// `$CASHTAG` (starts with `$` followed exclusively by ASCII uppercase `A..=Z`).
    Cashtag,
}

/// Exact port of `Dialogs::IsHashOrCashtagSearchQuery` (`dialogs/ui/chat_search_in.cpp:241-260`).
#[must_use]
pub fn classify_hash_or_cashtag(query: &str) -> HashOrCashtag {
    let trimmed = query.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('#') => {
            if trimmed.chars().any(char::is_whitespace) {
                HashOrCashtag::None
            } else {
                HashOrCashtag::Hashtag
            }
        }
        Some('$') => {
            if chars.all(|c| c.is_ascii_uppercase()) {
                HashOrCashtag::Cashtag
            } else {
                HashOrCashtag::None
            }
        }
        _ => HashOrCashtag::None,
    }
}

/// Exact port of `Api::ConvertPeerSearchQuery` (`api/api_single_message_search.cpp:221-235`):
/// extracts the `domain` query parameter from `tg://resolve?domain=...` (or `t.me/<user>`) URLs.
#[must_use]
pub fn convert_peer_search_query(query: &str) -> String {
    let trimmed = query.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("tg://resolve")
        && let Some(idx) = trimmed.find('?')
    {
        for pair in trimmed[idx + 1..].split('&') {
            if let Some((k, v)) = pair.split_once('=')
                && k.eq_ignore_ascii_case("domain")
            {
                return v.to_owned();
            }
        }
    }
    query.to_owned()
}

// ============================================================================
// 1. Shared Media & In-Chat Search (`messages.search`)
// ============================================================================

/// Builds a shared-media `messages.search` request matching `Api::PrepareSearchRequest`
/// (`data/data_search_controller.cpp:141-202`).
///
/// Returns `None` when:
/// - `query` is empty AND `filter == SharedMediaFilter::Empty`, or
/// - `topic_root_id != 0` AND `filter == SharedMediaFilter::ChatPhoto`.
///
/// Sets `limit = 0` (`FIRST_SHARED_MEDIA_LIMIT`) when `message_id == 0`, otherwise `100`
/// (`SHARED_MEDIA_LIMIT`), clamps `offset_id` to `0..=0x3FFFFFFF`, and sets exact `add_offset`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn prepare_shared_media_search_request(
    session: &Session,
    peer_id: PeerId,
    topic_root_id: MsgId,
    monoforum_peer_id: PeerId,
    filter: SharedMediaFilter,
    query: &str,
    message_id: MsgId,
    direction: LoadDirection,
) -> Option<tl_fns::messages::Search> {
    if query.is_empty() && filter == SharedMediaFilter::Empty {
        return None;
    }
    if !topic_root_id.is_zero() && filter == SharedMediaFilter::ChatPhoto {
        return None;
    }

    let limit = if message_id.is_zero() {
        FIRST_SHARED_MEDIA_LIMIT
    } else {
        SHARED_MEDIA_LIMIT
    };
    let raw_offset_id = match direction {
        LoadDirection::Before | LoadDirection::Around => message_id.0,
        LoadDirection::After => message_id.0.saturating_add(1),
    };
    let offset_id = raw_offset_id.clamp(0, i64::from(MAX_OFFSET_ID)) as i32;
    let add_offset = match direction {
        LoadDirection::Before => 0,
        LoadDirection::Around => -limit / 2,
        LoadDirection::After => -limit,
    };

    Some(tl_fns::messages::Search {
        peer: input_peer_for(session, peer_id),
        q: query.to_owned(),
        from_id: None,
        saved_peer_id: (!monoforum_peer_id.is_zero())
            .then(|| input_peer_for(session, monoforum_peer_id)),
        saved_reaction: None,
        top_msg_id: (!topic_root_id.is_zero()).then(|| topic_root_id.as_i32().unwrap_or(0)),
        filter: filter.to_tl(),
        min_date: 0,
        max_date: 0,
        offset_id,
        add_offset,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    })
}

/// Parsed bidirectional search slice (`Api::SearchResult` in `data_search_controller.cpp:204-292`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSliceResult {
    /// Server message IDs returned in this slice.
    pub message_ids: Vec<MsgId>,
    /// Gap-free range covered by this response.
    pub no_skip_range: MsgRange,
    /// Total count reported by server, if known.
    pub full_count: Option<i32>,
    /// Channel `pts` if the response was `messages.channelMessages`.
    pub channel_pts: Option<i32>,
}

/// Parses a `messages.Messages` response from `messages.search`, applying users, chats, topics,
/// messages, and channel PTS to `session` and computing tdesktop's exact `noSkipRange`
/// (`Api::ParseSearchResult` in `data/data_search_controller.cpp:204-292`).
pub fn parse_search_result(
    session: &mut Session,
    updates_handler: Option<&mut UpdatesHandler>,
    peer_id: PeerId,
    message_id: MsgId,
    direction: LoadDirection,
    response: &tl_enums::messages::Messages,
    now: TimeId,
) -> SearchSliceResult {
    let mut no_skip_range = MsgRange::new(message_id, message_id);

    let (messages, users, chats, topics, full_count, channel_pts) = match response {
        tl_enums::messages::Messages::Messages(d) => (
            &d.messages,
            &d.users,
            &d.chats,
            &d.topics,
            Some(d.messages.len() as i32),
            None,
        ),
        tl_enums::messages::Messages::Slice(d) => (
            &d.messages,
            &d.users,
            &d.chats,
            &d.topics,
            Some(d.count),
            None,
        ),
        tl_enums::messages::Messages::ChannelMessages(d) => (
            &d.messages,
            &d.users,
            &d.chats,
            &d.topics,
            Some(d.count),
            Some(d.pts),
        ),
        tl_enums::messages::Messages::NotModified(_) => {
            return SearchSliceResult {
                message_ids: Vec::new(),
                no_skip_range,
                full_count: None,
                channel_pts: None,
            };
        }
    };

    apply_tl_users_and_chats(session, users, chats);
    if let Some(pts) = channel_pts
        && let Some(ch_id) = peer_id.to_channel()
        && let Some(handler) = updates_handler
    {
        handler.init_channel_pts(ch_id, pts);
    }
    for topic in topics {
        let _ = apply_tl_forum_topic(session, peer_id, topic, now);
    }

    let mut message_ids = Vec::with_capacity(messages.len());
    for msg in messages {
        if let Some(full_id) = apply_tl_message(session, msg) {
            message_ids.push(full_id.msg);
            if full_id.msg < no_skip_range.from {
                no_skip_range.from = full_id.msg;
            }
            if full_id.msg > no_skip_range.till {
                no_skip_range.till = full_id.msg;
            }
        }
    }

    if !message_id.is_zero() && message_ids.is_empty() {
        no_skip_range = match direction {
            LoadDirection::Before => MsgRange::new(MsgId::ZERO, no_skip_range.till),
            LoadDirection::Around => MsgRange::new(MsgId::ZERO, SERVER_MAX_MSG_ID),
            LoadDirection::After => MsgRange::new(no_skip_range.from, SERVER_MAX_MSG_ID),
        };
    }

    SearchSliceResult {
        message_ids,
        no_skip_range,
        full_count,
        channel_pts,
    }
}

/// Request parameters for in-chat [`MessagesSearch`] (`Api::MessagesSearch::Request` in
/// `api/api_messages_search.h`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InChatSearchRequest {
    /// Text query.
    pub query: String,
    /// Optional sender peer (`from_id` for groups/channels, or `saved_peer_id` when searching
    /// own Saved Messages — matching `api_messages_search.cpp:114-119`).
    pub from: Option<PeerId>,
    /// Optional explicit monoforum / saved sublist peer ID (`saved_peer_id`).
    pub saved_peer_id: Option<PeerId>,
    /// Optional forum topic root ID (`top_msg_id`).
    pub top_msg_id: Option<MsgId>,
    /// Optional Saved Messages reaction tags filter (`saved_reaction`).
    pub tags: Vec<ReactionId>,
    /// Message filter (`Empty`, `Pinned`, or any [`SharedMediaFilter`]).
    pub filter: SharedMediaFilter,
    /// Optional `min_date` bound (`0` = unbounded).
    pub min_date: TimeId,
    /// Optional `max_date` bound (`0` = unbounded).
    pub max_date: TimeId,
}

impl InChatSearchRequest {
    /// Deterministic start-page cache key (`RequestToToken` in `api_messages_search.cpp:47-66`).
    #[must_use]
    pub fn cache_token(&self) -> String {
        let mut out = self.query.clone();
        if let Some(from) = self.from {
            out.push('\n');
            out.push_str(&from.raw().to_string());
        }
        if let Some(saved) = self.saved_peer_id {
            out.push_str("\nsaved");
            out.push_str(&saved.raw().to_string());
        }
        if let Some(top) = self.top_msg_id {
            out.push_str("\ntop");
            out.push_str(&top.0.to_string());
        }
        for tag in &self.tags {
            out.push('\n');
            match tag {
                ReactionId::Custom(id) => {
                    out.push_str("custom");
                    out.push_str(&id.0.to_string());
                }
                ReactionId::Emoji(e) => {
                    out.push_str("emoji");
                    out.push_str(e);
                }
                ReactionId::Paid => out.push_str("paid"),
            }
        }
        if self.filter != SharedMediaFilter::Empty {
            out.push_str(&format!("\nfilter:{:?}", self.filter));
        }
        if self.min_date != 0 || self.max_date != 0 {
            out.push_str(&format!("\ndate:{}:{}", self.min_date, self.max_date));
        }
        out
    }
}

/// Output of one [`MessagesSearch`] page (`Api::FoundMessages`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoundMessages {
    /// Total match count reported by server.
    pub total: i32,
    /// Full message IDs returned in this page.
    pub messages: Vec<FullMsgId>,
    /// Cache token identifying the query.
    pub next_token: String,
}

/// Stateful in-chat `messages.search` pager with start-page cache (`Api::MessagesSearch` in
/// `api/api_messages_search.cpp`).
#[derive(Debug)]
pub struct MessagesSearch {
    peer_id: PeerId,
    request: InChatSearchRequest,
    offset_id: MsgId,
    cache_of_start: HashMap<String, tl_enums::messages::Messages>,
}

impl MessagesSearch {
    /// Creates a new `MessagesSearch` scoped to `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            request: InChatSearchRequest::default(),
            offset_id: MsgId::ZERO,
            cache_of_start: HashMap::new(),
        }
    }

    /// Returns the current pagination `offset_id`.
    #[inline]
    #[must_use]
    pub const fn offset_id(&self) -> MsgId {
        self.offset_id
    }

    /// Starts a fresh search from `offset_id = 0` (`MessagesSearch::searchMessages`).
    pub async fn search_messages<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        updates_handler: Option<&mut UpdatesHandler>,
        request: InChatSearchRequest,
        now: TimeId,
    ) -> Result<FoundMessages, ApiError> {
        self.request = request;
        self.offset_id = MsgId::ZERO;
        self.execute_page(rpc, session, updates_handler, now).await
    }

    /// Loads the next page using the previous page's last message ID as `offset_id`
    /// (`MessagesSearch::searchMore`).
    pub async fn search_more<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        updates_handler: Option<&mut UpdatesHandler>,
        now: TimeId,
    ) -> Result<FoundMessages, ApiError> {
        self.execute_page(rpc, session, updates_handler, now).await
    }

    async fn execute_page<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        updates_handler: Option<&mut UpdatesHandler>,
        now: TimeId,
    ) -> Result<FoundMessages, ApiError> {
        let token = self.request.cache_token();
        if self.offset_id.is_zero()
            && let Some(cached) = self.cache_of_start.get(&token).cloned()
        {
            return Ok(self.apply_response(session, None, &cached, &token, true, now));
        }

        // Match `api_messages_search.cpp:114-121`:
        // - Private user chats omit `from_id` (`fromPeer = peer->isUser() ? nullptr : from`).
        // - Own Saved Messages (`peer->isSelf()`) moves `from` into `saved_peer_id`.
        let is_user = session.peer(self.peer_id).and_then(Peer::as_user).is_some();
        let is_self = self.peer_id == session.self_peer_id();
        let from_peer = if is_user { None } else { self.request.from };
        let saved_peer =
            self.request
                .saved_peer_id
                .or(if is_self { self.request.from } else { None });
        let saved_reaction = if self.request.tags.is_empty() {
            None
        } else {
            Some(self.request.tags.iter().map(reaction_to_tl).collect())
        };

        let req = tl_fns::messages::Search {
            peer: input_peer_for(session, self.peer_id),
            q: self.request.query.clone(),
            from_id: from_peer.map(|pid| input_peer_for(session, pid)),
            saved_peer_id: saved_peer.map(|pid| input_peer_for(session, pid)),
            saved_reaction,
            top_msg_id: self
                .request
                .top_msg_id
                .filter(|id| !id.is_zero())
                .and_then(MsgId::as_i32),
            filter: self.request.filter.to_tl(),
            min_date: self.request.min_date,
            max_date: self.request.max_date,
            offset_id: self.offset_id.as_i32().unwrap_or(0),
            add_offset: 0,
            limit: SEARCH_PER_PAGE,
            max_id: 0,
            min_id: 0,
            hash: 0,
        };

        let resp = match rpc.invoke(&req).await {
            Ok(r) => r,
            Err(err) => {
                let inv: InvocationError = err.into();
                // Match `api_messages_search.cpp:149-151`: `SEARCH_QUERY_EMPTY` yields empty result.
                if let InvocationError::Rpc(ref rpc_err) = inv
                    && rpc_err.name == "SEARCH_QUERY_EMPTY"
                {
                    self.offset_id = MsgId::ZERO;
                    return Ok(FoundMessages {
                        total: 0,
                        messages: Vec::new(),
                        next_token: token,
                    });
                }
                return Err(ApiError::Invocation(inv));
            }
        };

        if self.offset_id.is_zero() {
            self.cache_of_start.insert(token.clone(), resp.clone());
        }
        Ok(self.apply_response(session, updates_handler, &resp, &token, false, now))
    }

    fn apply_response(
        &mut self,
        session: &mut Session,
        updates_handler: Option<&mut UpdatesHandler>,
        resp: &tl_enums::messages::Messages,
        token: &str,
        from_cache: bool,
        now: TimeId,
    ) -> FoundMessages {
        let (messages, users, chats, topics, total, channel_pts) = match resp {
            tl_enums::messages::Messages::Messages(d) => (
                &d.messages,
                &d.users,
                &d.chats,
                &d.topics,
                d.messages.len() as i32,
                None,
            ),
            tl_enums::messages::Messages::Slice(d) => {
                (&d.messages, &d.users, &d.chats, &d.topics, d.count, None)
            }
            tl_enums::messages::Messages::ChannelMessages(d) => (
                &d.messages,
                &d.users,
                &d.chats,
                &d.topics,
                d.count,
                Some(d.pts),
            ),
            tl_enums::messages::Messages::NotModified(_) => {
                self.offset_id = MsgId::ZERO;
                return FoundMessages {
                    total: 0,
                    messages: Vec::new(),
                    next_token: token.to_owned(),
                };
            }
        };

        // Match `api_messages_search.cpp:172-205`: don't re-apply users/chats/topics/pts from cache.
        if !from_cache {
            apply_tl_users_and_chats(session, users, chats);
            if let Some(pts) = channel_pts
                && let Some(ch_id) = self.peer_id.to_channel()
                && let Some(handler) = updates_handler
            {
                handler.init_channel_pts(ch_id, pts);
            }
            for topic in topics {
                let _ = apply_tl_forum_topic(session, self.peer_id, topic, now);
            }
        }

        let mut items = Vec::with_capacity(messages.len());
        for raw in messages {
            if let Some((peer, _, date)) = tl_message_meta(raw)
                && session.peer(peer).is_some()
                && date != 0
                && let Some(full_id) = apply_tl_message(session, raw)
            {
                items.push(full_id);
            }
        }
        self.offset_id = items.last().map_or(MsgId::ZERO, |id| id.msg);

        FoundMessages {
            total,
            messages: items,
            next_token: token.to_owned(),
        }
    }
}

/// Merged in-chat search across a migrated supergroup and its legacy basic chat
/// (`Api::MessagesSearchMerged` in `api/api_messages_search_merged.cpp`).
#[derive(Debug)]
pub struct MessagesSearchMerged {
    primary: MessagesSearch,
    migrated: Option<MessagesSearch>,
    concatenated: FoundMessages,
    migrated_first: Option<FoundMessages>,
    primary_exhausted: bool,
}

impl MessagesSearchMerged {
    /// Creates a new merged search for `peer_id` and optional `migrated_from` legacy chat.
    #[must_use]
    pub fn new(peer_id: PeerId, migrated_from: Option<PeerId>) -> Self {
        Self {
            primary: MessagesSearch::new(peer_id),
            migrated: migrated_from.map(MessagesSearch::new),
            concatenated: FoundMessages::default(),
            migrated_first: None,
            primary_exhausted: false,
        }
    }

    /// Returns the accumulated merged results.
    #[must_use]
    pub fn messages(&self) -> &FoundMessages {
        &self.concatenated
    }

    /// Runs the initial search against primary (and legacy chat if migrated), summing totals
    /// and appending legacy hits immediately if primary is already exhausted.
    pub async fn search<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        mut updates_handler: Option<&mut UpdatesHandler>,
        request: InChatSearchRequest,
        now: TimeId,
    ) -> Result<&FoundMessages, ApiError> {
        self.primary_exhausted = false;
        self.migrated_first = None;

        let primary_found = self
            .primary
            .search_messages(
                rpc,
                session,
                updates_handler.as_deref_mut(),
                request.clone(),
                now,
            )
            .await?;
        let migrated_found = if let Some(ref mut mig) = self.migrated {
            Some(
                mig.search_messages(rpc, session, updates_handler, request, now)
                    .await?,
            )
        } else {
            None
        };

        let mut total = primary_found.total;
        let primary_done = primary_found.messages.len() as i32 >= primary_found.total;
        self.concatenated = primary_found;
        if let Some(mf) = migrated_found {
            total += mf.total;
            self.concatenated.total = total;
            if primary_done {
                self.primary_exhausted = true;
                self.concatenated.messages.extend(mf.messages);
            } else {
                self.migrated_first = Some(mf);
            }
        } else {
            self.primary_exhausted = primary_done;
        }
        Ok(&self.concatenated)
    }

    /// Loads the next page, transparently switching from primary supergroup to legacy chat
    /// once primary reaches its `total` (`MessagesSearchMerged::searchMore`).
    pub async fn search_more<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        updates_handler: Option<&mut UpdatesHandler>,
        now: TimeId,
    ) -> Result<&FoundMessages, ApiError> {
        if self.primary_exhausted {
            if let Some(ref mut mig) = self.migrated {
                let next = mig.search_more(rpc, session, updates_handler, now).await?;
                self.concatenated.messages.extend(next.messages);
            }
            return Ok(&self.concatenated);
        }

        let next = self
            .primary
            .search_more(rpc, session, updates_handler, now)
            .await?;
        let primary_total = next.total;
        self.concatenated.messages.extend(next.messages);
        if self.concatenated.messages.len() as i32 >= primary_total {
            self.primary_exhausted = true;
            if let Some(mf) = self.migrated_first.take() {
                self.concatenated.messages.extend(mf.messages);
            }
        }
        Ok(&self.concatenated)
    }
}

// ============================================================================
// 2. Global Search (`messages.searchGlobal`)
// ============================================================================

/// Chat type filter for `messages.searchGlobal` (`Dialogs::ChatTypeFilter`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum GlobalChatTypeFilter {
    /// All chat types.
    #[default]
    All,
    /// Private 1:1 user chats (`users_only = true`).
    Private,
    /// Groups and supergroups (`groups_only = true`).
    Groups,
    /// Broadcast channels (`broadcasts_only = true`).
    Channels,
}

/// Folder / community scope for `messages.searchGlobal` (`dialogs_widget.cpp:3537-3556`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum GlobalSearchScope {
    /// Search across both main list and archive (`folder_id = None`).
    #[default]
    AllChats,
    /// Main list only (`folder_id = Some(0)`).
    MainOnly,
    /// Archive folder only (`folder_id = Some(1)`).
    ArchiveOnly,
    /// Scoped to a specific community (`community = Some(...)`).
    Community(tl_enums::InputChannel),
}

/// Parameters for [`GlobalSearchController`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GlobalSearchParams {
    /// Query string.
    pub query: String,
    /// Chat type restriction (`users_only`, `groups_only`, `broadcasts_only`).
    pub chat_type: GlobalChatTypeFilter,
    /// Folder / community scope.
    pub scope: GlobalSearchScope,
    /// Message media filter.
    pub filter: SharedMediaFilter,
    /// Minimum timestamp (`0` = unbounded).
    pub min_date: TimeId,
    /// Maximum timestamp (`0` = unbounded).
    pub max_date: TimeId,
    /// Page size (defaults to `SEARCH_PER_PAGE = 50`).
    pub limit: i32,
}

/// One page returned by [`GlobalSearchController`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSearchPage {
    /// Full message IDs returned on this page.
    pub messages: Vec<FullMsgId>,
    /// Total count reported by server.
    pub total_count: i32,
    /// Whether all pages have been exhausted.
    pub finished: bool,
    /// Next pagination rate (`next_rate`).
    pub next_rate: i32,
    /// Last message peer ID (for next `offset_peer`).
    pub last_peer: Option<PeerId>,
    /// Last message ID (for next `offset_id`).
    pub last_id: MsgId,
}

/// Stateful `messages.searchGlobal` pager (`dialogs/dialogs_widget.cpp:3529-3699`).
#[derive(Debug, Default)]
pub struct GlobalSearchController {
    params: GlobalSearchParams,
    next_rate: i32,
    last_peer: Option<PeerId>,
    last_id: MsgId,
    finished: bool,
}

impl GlobalSearchController {
    /// Creates a new `GlobalSearchController`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when pagination has finished.
    #[inline]
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Starts a new global search from `(offset_rate = 0, offset_peer = Empty, offset_id = 0)`.
    pub async fn search<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        params: GlobalSearchParams,
    ) -> Result<GlobalSearchPage, ApiError> {
        self.params = params;
        self.next_rate = 0;
        self.last_peer = None;
        self.last_id = MsgId::ZERO;
        self.finished = false;
        self.fetch_page(rpc, session, true).await
    }

    /// Loads the next global search page using stored `(offset_rate, offset_peer, offset_id)`.
    pub async fn load_more<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
    ) -> Result<Option<GlobalSearchPage>, ApiError> {
        if self.finished {
            return Ok(None);
        }
        let from_start = self.last_id.is_zero() || self.last_peer.is_none();
        self.fetch_page(rpc, session, from_start).await.map(Some)
    }

    async fn fetch_page<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        from_start: bool,
    ) -> Result<GlobalSearchPage, ApiError> {
        let (folder_id, community) = match &self.params.scope {
            GlobalSearchScope::AllChats => (None, None),
            GlobalSearchScope::MainOnly => (Some(0), None),
            GlobalSearchScope::ArchiveOnly => (Some(tg_data::ARCHIVE_FOLDER_ID), None),
            GlobalSearchScope::Community(ch) => (None, Some(ch.clone())),
        };
        let limit = if self.params.limit > 0 {
            self.params.limit
        } else {
            SEARCH_PER_PAGE
        };
        let req = tl_fns::messages::SearchGlobal {
            broadcasts_only: self.params.chat_type == GlobalChatTypeFilter::Channels,
            groups_only: self.params.chat_type == GlobalChatTypeFilter::Groups,
            users_only: self.params.chat_type == GlobalChatTypeFilter::Private,
            folder_id,
            community,
            q: self.params.query.clone(),
            filter: self.params.filter.to_tl(),
            min_date: self.params.min_date,
            max_date: self.params.max_date,
            offset_rate: if from_start { 0 } else { self.next_rate },
            offset_peer: if from_start {
                tl_enums::InputPeer::Empty
            } else {
                self.last_peer.map_or(tl_enums::InputPeer::Empty, |pid| {
                    input_peer_for(session, pid)
                })
            },
            offset_id: if from_start {
                0
            } else {
                self.last_id.as_i32().unwrap_or(0)
            },
            limit,
        };

        let resp = match rpc.invoke(&req).await {
            Ok(r) => r,
            Err(err) => {
                let inv: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = inv
                    && rpc_err.name == "SEARCH_QUERY_EMPTY"
                {
                    self.finished = true;
                    return Ok(GlobalSearchPage {
                        finished: true,
                        ..GlobalSearchPage::default()
                    });
                }
                return Err(ApiError::Invocation(inv));
            }
        };

        if from_start {
            self.last_peer = None;
            self.last_id = MsgId::ZERO;
        }

        let (messages, users, chats, total_count, next_rate, complete_variant) = match resp {
            tl_enums::messages::Messages::Messages(d) => {
                let total = d.messages.len() as i32;
                (d.messages, d.users, d.chats, total, None, true)
            }
            tl_enums::messages::Messages::Slice(d) => {
                (d.messages, d.users, d.chats, d.count, d.next_rate, false)
            }
            tl_enums::messages::Messages::ChannelMessages(d) => {
                (d.messages, d.users, d.chats, d.count, None, false)
            }
            tl_enums::messages::Messages::NotModified(_) => {
                self.finished = true;
                return Ok(GlobalSearchPage {
                    finished: true,
                    ..GlobalSearchPage::default()
                });
            }
        };

        apply_tl_users_and_chats(session, &users, &chats);
        let mut found_ids = Vec::with_capacity(messages.len());
        for raw in &messages {
            if let Some((peer, mid, date)) = tl_message_meta(raw) {
                if session.peer(peer).is_some() {
                    if date != 0
                        && let Some(full_id) = apply_tl_message(session, raw)
                    {
                        found_ids.push(full_id);
                    }
                    self.last_peer = Some(peer);
                }
                self.last_id = mid;
            }
        }

        // Match `dialogs_widget.cpp:3661-3681`:
        // `messages.messages` always completes; `messages.messagesSlice` continues ONLY while
        // `next_rate` is present and differs from previous `next_rate`.
        if complete_variant {
            self.finished = true;
        } else {
            let rate_updated = next_rate.is_some_and(|r| r != self.next_rate);
            if let Some(r) = next_rate
                && rate_updated
            {
                self.next_rate = r;
            }
            if !rate_updated {
                self.finished = true;
            }
        }

        Ok(GlobalSearchPage {
            messages: found_ids,
            total_count,
            finished: self.finished,
            next_rate: self.next_rate,
            last_peer: self.last_peer,
            last_id: self.last_id,
        })
    }
}

/// Builds a global shared-media `messages.searchGlobal` request matching
/// `Api::PrepareGlobalMediaRequest` (`data/data_search_controller.cpp:62-93`):
/// explicitly sets `folder_id = Some(0)` (no archive), and `limit = 0` on the first request
/// (`offset_peer == None`) else `100`.
#[must_use]
pub fn prepare_global_media_request(
    session: &Session,
    offset_rate: i32,
    offset_peer: Option<PeerId>,
    offset_id: MsgId,
    filter: SharedMediaFilter,
    query: &str,
) -> Option<tl_fns::messages::SearchGlobal> {
    if query.is_empty() && filter == SharedMediaFilter::Empty {
        return None;
    }
    let limit = if offset_peer.is_some() {
        SHARED_MEDIA_LIMIT
    } else {
        FIRST_SHARED_MEDIA_LIMIT
    };
    Some(tl_fns::messages::SearchGlobal {
        broadcasts_only: false,
        groups_only: false,
        users_only: false,
        folder_id: Some(0), // Match `data_search_controller.cpp:80`: `f_folder_id` = 0 (No archive).
        community: None,
        q: query.to_owned(),
        filter: filter.to_tl(),
        min_date: 0,
        max_date: 0,
        offset_rate,
        offset_peer: offset_peer.map_or(tl_enums::InputPeer::Empty, |pid| {
            input_peer_for(session, pid)
        }),
        offset_id: offset_id.as_i32().unwrap_or(0),
        limit,
    })
}

// ============================================================================
// 3. Search Counters (`messages.getSearchCounters`)
// ============================================================================

/// One filter count returned by `messages.getSearchCounters`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCounterResult {
    /// Filter echoed by server.
    pub filter: tl_enums::MessagesFilter,
    /// Match count for this filter.
    pub count: i32,
    /// Whether the server reported `inexact = true`.
    pub inexact: bool,
}

/// Queries per-filter match counts for `peer_id` via `messages.getSearchCounters`.
pub async fn get_search_counters<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    saved_peer_id: Option<PeerId>,
    top_msg_id: Option<MsgId>,
    filters: &[SharedMediaFilter],
) -> Result<Vec<SearchCounterResult>, ApiError> {
    let req = tl_fns::messages::GetSearchCounters {
        peer: input_peer_for(session, peer_id),
        saved_peer_id: saved_peer_id.map(|pid| input_peer_for(session, pid)),
        top_msg_id: top_msg_id
            .filter(|id| !id.is_zero())
            .and_then(MsgId::as_i32),
        filters: filters.iter().map(|f| f.to_tl()).collect(),
    };
    let resp = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(resp
        .into_iter()
        .map(
            |tl_enums::messages::SearchCounter::SearchCounter(c)| SearchCounterResult {
                filter: c.filter,
                count: c.count,
                inexact: c.inexact,
            },
        )
        .collect())
}

// ============================================================================
// 4. Contacts / Peer Directory Search (`contacts.search`)
// ============================================================================

/// Peer search mode (`Api::PeerSearch::Type` in `api/api_peer_search.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PeerSearchType {
    /// All peer types (`limit = SearchPeopleLimit = 5`).
    #[default]
    Default,
    /// Channels only (`broadcasts = true`, `limit = kTypedLimit = 50`).
    Channels,
    /// Bots only (`bots = true`, `limit = kTypedLimit = 50`).
    Bots,
}

/// Result of `contacts.search` (`Api::PeerSearchResult`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerSearchResult {
    /// Normalized query string.
    pub query: String,
    /// Matching peers from user's own chats/contacts (`my_results`).
    pub my_results: Vec<PeerId>,
    /// Matching global public peers (`results`).
    pub global_results: Vec<PeerId>,
}

/// Caching `contacts.search` controller (`Api::PeerSearch` in `api/api_peer_search.cpp`).
#[derive(Debug, Default)]
pub struct PeerSearchController {
    kind: PeerSearchType,
    cache: HashMap<String, PeerSearchResult>,
}

impl PeerSearchController {
    /// Creates a new `PeerSearchController` with the given [`PeerSearchType`].
    #[must_use]
    pub fn new(kind: PeerSearchType) -> Self {
        Self {
            kind,
            cache: HashMap::new(),
        }
    }

    /// Searches Telegram's peer directory via `contacts.search`.
    ///
    /// Matches `api/api_peer_search.cpp:33-98`:
    /// - Converts `tg://resolve?domain=...` queries via [`convert_peer_search_query`].
    /// - Returns empty immediately (0 RPCs) for empty, `#hashtag`, or `$CASHTAG` queries.
    /// - Uses `limit = 50` for `Channels` (`broadcasts = true`) / `Bots` (`bots = true`),
    ///   and `limit = 5` (`SEARCH_PEOPLE_LIMIT`) otherwise.
    /// - Caches results per normalized query.
    pub async fn search<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        raw_query: &str,
    ) -> Result<PeerSearchResult, ApiError> {
        let query = convert_peer_search_query(raw_query);
        if query.is_empty() || classify_hash_or_cashtag(&query) != HashOrCashtag::None {
            return Ok(PeerSearchResult::default());
        }
        if let Some(cached) = self.cache.get(&query) {
            return Ok(cached.clone());
        }

        let broadcasts = self.kind == PeerSearchType::Channels;
        let bots = self.kind == PeerSearchType::Bots;
        let limit = if broadcasts || bots {
            PEER_SEARCH_TYPED_LIMIT
        } else {
            SEARCH_PEOPLE_LIMIT
        };
        let tl_enums::contacts::Found::Found(found) = rpc
            .invoke(&tl_fns::contacts::Search {
                broadcasts,
                bots,
                q: query.clone(),
                limit,
            })
            .await
            .map_err(Into::into)?;

        apply_tl_users_and_chats(session, &found.users, &found.chats);
        let result = PeerSearchResult {
            query: query.clone(),
            my_results: found.my_results.iter().map(peer_id_from_tl).collect(),
            global_results: found.results.iter().map(peer_id_from_tl).collect(),
        };
        self.cache.insert(query, result.clone());
        Ok(result)
    }
}

// ============================================================================
// 5. Hashtag & Public Posts Search (`channels.checkSearchPostsFlood` + `channels.searchPosts`)
// ============================================================================

/// Parsed `SearchPostsFlood` quota state (`Dialogs::PostsSearchIntroState` in
/// `dialogs/dialogs_search_posts.cpp:328-343`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostsSearchFloodState {
    /// Whether the checked query is currently free (`query_is_free`).
    pub query_is_free: bool,
    /// Daily free searches quota (`total_daily`).
    pub total_daily: i32,
    /// Remaining free searches (`remains.max(0)`).
    pub remains: i32,
    /// Unix timestamp until next free search (`wait_till`).
    pub wait_till: Option<TimeId>,
    /// Stars required for a paid search (`stars_amount`).
    pub stars_amount: i64,
}

impl PostsSearchFloodState {
    /// Converts from TL `SearchPostsFlood`.
    #[must_use]
    pub const fn from_tl(data: &tl_types::SearchPostsFlood) -> Self {
        Self {
            query_is_free: data.query_is_free,
            total_daily: data.total_daily,
            remains: if data.remains > 0 { data.remains } else { 0 },
            wait_till: data.wait_till,
            stars_amount: data.stars_amount,
        }
    }

    /// Computes effective `allow_paid_stars` clamping (`PostsSearch::setAllowedStars` in
    /// `dialogs/dialogs_search_posts.cpp:116-132`): returns `0` if free searches remain or
    /// `wait_till <= now`, else clamps to `stars_amount`.
    #[must_use]
    pub fn clamp_allowed_stars(&self, requested_stars: i64, now: TimeId) -> i64 {
        if self.remains > 0 || self.wait_till.is_some_and(|t| t > 0 && t <= now) {
            0
        } else {
            requested_stars.min(self.stars_amount).max(0)
        }
    }
}

/// Output of one `channels.searchPosts` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicPostsPage {
    /// Found public channel post IDs.
    pub messages: Vec<FullMsgId>,
    /// Total count reported by server.
    pub total_count: i32,
    /// Updated flood quota returned inside `messages.messagesSlice.search_flood`, if present.
    pub flood: Option<PostsSearchFloodState>,
    /// Whether all pages for this query have been loaded.
    pub finished: bool,
}

/// Stateful public channel posts search controller supporting both `#hashtag` mode
/// (`dialogs_widget.cpp:3498-3527`) and text query mode with `channels.checkSearchPostsFlood`
/// + paid Stars quota (`dialogs/dialogs_search_posts.cpp`).
#[derive(Debug, Default)]
pub struct PostsSearchController {
    hashtag_mode: bool,
    term: String,
    offset_rate: i32,
    offset_peer: Option<PeerId>,
    offset_id: MsgId,
    finished: bool,
    flood_state: Option<PostsSearchFloodState>,
}

impl PostsSearchController {
    /// Creates a new `PostsSearchController`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the last known [`PostsSearchFloodState`], if any.
    #[inline]
    #[must_use]
    pub const fn flood_state(&self) -> Option<PostsSearchFloodState> {
        self.flood_state
    }

    /// Returns `true` if pagination has finished.
    #[inline]
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Checks daily free-search quota via `channels.checkSearchPostsFlood`
    /// (`PostsSearch::requestState` in `dialogs/dialogs_search_posts.cpp:349-379`).
    pub async fn check_flood<C: RpcClient>(
        &mut self,
        rpc: &C,
        query: Option<&str>,
    ) -> Result<PostsSearchFloodState, ApiError> {
        let req = tl_fns::channels::CheckSearchPostsFlood {
            query: query.map(str::to_owned),
        };
        let tl_enums::SearchPostsFlood::SearchPostsFlood(raw) =
            rpc.invoke_handle_flood(&req).await.map_err(Into::into)?;
        let state = PostsSearchFloodState::from_tl(&raw);
        self.flood_state = Some(state);
        Ok(state)
    }

    /// Searches public channel posts by `#hashtag` (`Widget::requestPublicPosts` in
    /// `dialogs/dialogs_widget.cpp:3498-3527`), stripping a leading `#` if present.
    pub async fn search_hashtag<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        raw_hashtag: &str,
    ) -> Result<PublicPostsPage, ApiError> {
        let trimmed = raw_hashtag.trim();
        let stripped = trimmed.strip_prefix('#').unwrap_or(trimmed);
        self.hashtag_mode = true;
        self.term = stripped.to_owned();
        self.offset_rate = 0;
        self.offset_peer = None;
        self.offset_id = MsgId::ZERO;
        self.finished = false;
        self.fetch_page(rpc, session, 0).await
    }

    /// Searches public channel posts by text query (`PostsSearch::applyQuery` / `requestSearch`
    /// in `dialogs/dialogs_search_posts.cpp:191-326`).
    ///
    /// When `query` is non-empty and `allowed_stars == None`:
    /// 1. Calls `channels.checkSearchPostsFlood(query)`.
    /// 2. If `query_is_free == true`, immediately executes `channels.searchPosts`.
    /// 3. If `query_is_free == false`, returns `Ok(None)` with [`Self::flood_state`] populated so
    ///    the caller can confirm paying Stars and call [`Self::search_query_with_stars`].
    pub async fn search_query<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        query: &str,
    ) -> Result<Option<PublicPostsPage>, ApiError> {
        let trimmed = query.trim();
        self.hashtag_mode = false;
        self.term = trimmed.to_owned();
        self.offset_rate = 0;
        self.offset_peer = None;
        self.offset_id = MsgId::ZERO;
        self.finished = false;

        if !trimmed.is_empty() {
            let flood = self.check_flood(rpc, Some(trimmed)).await?;
            if !flood.query_is_free {
                return Ok(None);
            }
        }
        self.fetch_page(rpc, session, 0).await.map(Some)
    }

    /// Executes a paid public posts search after clamping `requested_stars` against the cached
    /// [`PostsSearchFloodState`] (`PostsSearch::setAllowedStars` in
    /// `dialogs/dialogs_search_posts.cpp:116-132`).
    pub async fn search_query_with_stars<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        requested_stars: i64,
        now: TimeId,
    ) -> Result<PublicPostsPage, ApiError> {
        let effective_stars = self.flood_state.map_or(requested_stars.max(0), |f| {
            f.clamp_allowed_stars(requested_stars, now)
        });
        self.fetch_page(rpc, session, effective_stars).await
    }

    /// Loads the next page of public posts using stored `(offset_rate, offset_peer, offset_id)`.
    pub async fn load_more<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
    ) -> Result<Option<PublicPostsPage>, ApiError> {
        if self.finished {
            return Ok(None);
        }
        self.fetch_page(rpc, session, 0).await.map(Some)
    }

    async fn fetch_page<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        allow_paid_stars: i64,
    ) -> Result<PublicPostsPage, ApiError> {
        let initial = self.offset_id.is_zero();
        let req = tl_fns::channels::SearchPosts {
            hashtag: self.hashtag_mode.then(|| self.term.clone()),
            query: (!self.hashtag_mode).then(|| self.term.clone()),
            offset_rate: if initial { 0 } else { self.offset_rate },
            offset_peer: if initial {
                tl_enums::InputPeer::Empty
            } else {
                self.offset_peer.map_or(tl_enums::InputPeer::Empty, |pid| {
                    input_peer_for(session, pid)
                })
            },
            offset_id: if initial {
                0
            } else {
                self.offset_id.as_i32().unwrap_or(0)
            },
            limit: SEARCH_PER_PAGE,
            allow_paid_stars: (allow_paid_stars > 0).then_some(allow_paid_stars),
        };

        // Match `dialogs_search_posts.cpp:314-325`: pass `.handleFloodErrors()` and re-check
        // `channels.checkSearchPostsFlood` when an initial page hits `FLOOD_WAIT_*`.
        let resp = match rpc.invoke_handle_flood(&req).await {
            Ok(r) => r,
            Err(err) => {
                let inv: InvocationError = err.into();
                if initial
                    && !self.hashtag_mode
                    && let InvocationError::Rpc(ref rpc_err) = inv
                    && rpc_err.name.starts_with("FLOOD_WAIT_")
                {
                    let query_clone = self.term.clone();
                    let _ = self.check_flood(rpc, Some(&query_clone)).await;
                } else {
                    self.finished = true;
                }
                return Err(ApiError::Invocation(inv));
            }
        };

        let (messages, users, chats, total_count, next_rate, flood, complete_variant) = match resp {
            tl_enums::messages::Messages::Messages(d) => {
                let total = d.messages.len() as i32;
                (d.messages, d.users, d.chats, total, None, None, true)
            }
            tl_enums::messages::Messages::Slice(d) => {
                let flood = d.search_flood.as_ref().map(|sf| {
                    let tl_enums::SearchPostsFlood::SearchPostsFlood(inner) = sf;
                    PostsSearchFloodState::from_tl(inner)
                });
                (
                    d.messages,
                    d.users,
                    d.chats,
                    d.count,
                    d.next_rate,
                    flood,
                    false,
                )
            }
            tl_enums::messages::Messages::ChannelMessages(d) => {
                (d.messages, d.users, d.chats, d.count, None, None, false)
            }
            tl_enums::messages::Messages::NotModified(_) => {
                self.finished = true;
                return Ok(PublicPostsPage {
                    finished: true,
                    ..PublicPostsPage::default()
                });
            }
        };

        if let Some(f) = flood {
            self.flood_state = Some(f);
        }

        apply_tl_users_and_chats(session, &users, &chats);
        let mut found_ids = Vec::with_capacity(messages.len());
        for raw in &messages {
            if let Some((peer, mid, date)) = tl_message_meta(raw) {
                if session.peer(peer).is_some() {
                    if date != 0
                        && let Some(full_id) = apply_tl_message(session, raw)
                    {
                        found_ids.push(full_id);
                    }
                    self.offset_peer = Some(peer);
                }
                self.offset_id = mid;
            }
        }

        // Match `dialogs_search_posts.cpp:263-272` & `dialogs_widget.cpp:3671`:
        // for posts search, `messagesSlice` finishes only when `list.empty()` (not when
        // `next_rate` is absent/unchanged), while updating `offset_rate` whenever `next_rate`
        // changes.
        if let Some(nr) = next_rate
            && nr != self.offset_rate
        {
            self.offset_rate = nr;
        }
        if complete_variant || found_ids.is_empty() {
            self.finished = true;
        }

        Ok(PublicPostsPage {
            messages: found_ids,
            total_count,
            flood,
            finished: self.finished,
        })
    }
}
