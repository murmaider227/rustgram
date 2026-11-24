//! Pure global shared-media search DTOs (`info/global_media.rs` model).

use crate::info::backend::{MediaItem, SharedMediaTab};
use crate::stand_in::types::{MsgId, PeerId};

/// Default page size for global media queries (`messages.searchGlobal`).
pub const GLOBAL_MEDIA_PAGE_SIZE: usize = 40;

/// Single item displayed in [`GlobalMediaSection`], pairing a [`MediaItem`] with its source chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalMediaEntry {
    /// Owning chat/channel peer id.
    pub peer_id: PeerId,
    /// Owning chat display title (shown as source badge on the item).
    pub peer_title: String,
    /// Underlying media item (`Photo`, `Video`, `File`, `Link`, `Voice`, `Music`, `Gif`).
    pub item: MediaItem,
}

impl GlobalMediaEntry {
    /// Returns the `(peer_id, msg_id)` key uniquely identifying this global media item.
    #[must_use]
    pub const fn full_key(&self) -> (PeerId, i32) {
        (self.peer_id, self.item.id())
    }
}

/// Paginated response returned by [`crate::info::backend::InfoBackend::global_media`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalMediaPage {
    /// Matching media entries across all dialogs, newest-first.
    pub entries: Vec<GlobalMediaEntry>,
    /// Total matching count reported by the server (`messages.messagesSlice.count`).
    pub total_count: usize,
    /// Pagination cursor `(offset_rate, offset_peer, offset_id)` for the next page, if any.
    pub next_cursor: Option<(i32, PeerId, MsgId)>,
    /// `true` if more entries can be fetched.
    pub has_more: bool,
}

/// Events emitted by [`GlobalMediaSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalMediaAction {
    /// User clicked a media item to open/preview/play it.
    OpenMedia {
        /// Source chat peer id.
        peer_id: PeerId,
        /// Message id of the item.
        item_id: i32,
        /// Active tab.
        tab: SharedMediaTab,
    },
    /// User clicked the source chat pill to jump to that chat.
    OpenSourceChat {
        /// Source chat peer id.
        peer_id: PeerId,
        /// Message id inside the source chat.
        msg_id: MsgId,
    },
    /// User toggled multi-selection on an item.
    ToggleSelect {
        /// Source chat peer id.
        peer_id: PeerId,
        /// Message id.
        item_id: i32,
    },
    /// User switched the active [`SharedMediaTab`].
    TabChanged(SharedMediaTab),
    /// User clicked "Load more" to paginate further global media.
    LoadMore,
}
