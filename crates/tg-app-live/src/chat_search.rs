//! In-chat message search for the history search bar (`HistoryView::ComposeSearch` ->
//! `Api::MessagesSearch`, `api/api_messages_search.cpp`): `messages.search` pages newest first,
//! continued with `offset_id` = the oldest id of the previous page.

use tg_api::LoadDirection;
use tg_app_core::history::backend::ChatSearchPage;
use tg_app_core::stand_in::types::{MsgId, PeerId};

use crate::LiveSession;
use crate::search_and_info::{InChatSearchParams, SharedMediaFilter};

/// Loads one page of `query` matches inside `peer_id` below `offset_id` (`MsgId(0)` = newest).
pub fn chat_search_page(
    live: &LiveSession,
    peer_id: PeerId,
    query: &str,
    from: Option<PeerId>,
    offset_id: MsgId,
    limit: i32,
) -> Result<ChatSearchPage, String> {
    let mut params = InChatSearchParams::new(peer_id, query, SharedMediaFilter::Empty);
    params.from_peer = from;
    params.around_id = offset_id;
    params.direction = LoadDirection::Before;
    params.limit = limit.clamp(1, 100);
    let page = live.search_in_chat(&params)?;
    let mut ids: Vec<MsgId> = page.messages.iter().map(|m| m.id).collect();
    ids.sort_by_key(|id| std::cmp::Reverse(id.0));
    ids.dedup();
    let next_offset = if page.has_more_older && !ids.is_empty() {
        page.oldest_id
    } else {
        None
    };
    Ok(ChatSearchPage {
        total: page.total_count.max(ids.len()),
        ids,
        next_offset,
    })
}
