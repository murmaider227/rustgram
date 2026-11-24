//! Port of `data/data_stories_ids.h`, `data/data_stories_ids.cpp`, and `StoriesIds` /
//! `RespectingPinned` helpers from `data/data_stories.cpp`.
//!
//! Manages ordered story ID lists (saved/archive descending lists, custom album orderings,
//! pinned-to-top precedence) and sparse slice windows around a target `StoryId`.

use tg_data::{PeerId, StoryId};

/// Ordered list of story IDs paired with its pinned-to-top subset (`Data::StoriesIds`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoriesIds {
    /// Story IDs. Descending by ID for Saved (`0`) and Archive (`-1`); server order for custom albums.
    pub list: Vec<StoryId>,
    /// Story IDs pinned to the top of the profile stories grid (in display order).
    pub pinned_to_top: Vec<StoryId>,
}

impl StoriesIds {
    /// Returns story IDs with `pinned_to_top` placed first, followed by remaining items in `list`
    /// (`Data::RespectingPinned` in `data_stories.cpp`).
    pub fn respecting_pinned(&self) -> Vec<StoryId> {
        respecting_pinned(self)
    }

    /// Inserts `id` into `list` maintaining descending order (`InsertSorted` in `data_stories.cpp`).
    ///
    /// Returns `true` if inserted, `false` if already present.
    pub fn insert_sorted_desc(&mut self, id: StoryId) -> bool {
        insert_sorted_desc(&mut self.list, id)
    }

    /// Removes `id` from `list` assuming descending order (`RemoveSorted` in `data_stories.cpp`).
    ///
    /// Returns `true` if removed.
    pub fn remove_sorted_desc(&mut self, id: StoryId) -> bool {
        remove_sorted_desc(&mut self.list, id)
    }

    /// Removes `id` from `list` by linear scan (`RemoveUnsorted` in `data_stories.cpp`).
    ///
    /// Returns `true` if removed.
    pub fn remove_unsorted(&mut self, id: StoryId) -> bool {
        remove_unsorted(&mut self.list, id)
    }

    /// Removes `id` from `pinned_to_top` if present.
    pub fn remove_pinned(&mut self, id: StoryId) -> bool {
        remove_unsorted(&mut self.pinned_to_top, id)
    }
}

/// Returns story IDs with `ids.pinned_to_top` first, followed by all entries in `ids.list`
/// not already in `ids.pinned_to_top` (`Data::RespectingPinned`).
pub fn respecting_pinned(ids: &StoriesIds) -> Vec<StoryId> {
    if ids.pinned_to_top.is_empty() {
        return ids.list.clone();
    }
    let mut result = Vec::with_capacity(ids.list.len().max(ids.pinned_to_top.len()));
    result.extend_from_slice(&ids.pinned_to_top);
    for &id in &ids.list {
        if !ids.pinned_to_top.contains(&id) {
            result.push(id);
        }
    }
    result
}

/// Inserts `id` into a descending-sorted `Vec<StoryId>` if not already present (`InsertSorted`).
pub fn insert_sorted_desc(list: &mut Vec<StoryId>, id: StoryId) -> bool {
    // Descending order: find first element <= id.
    let pos = list.partition_point(|&x| x > id);
    if list.get(pos) == Some(&id) {
        return false;
    }
    list.insert(pos, id);
    true
}

/// Removes `id` from a descending-sorted `Vec<StoryId>` (`RemoveSorted`).
pub fn remove_sorted_desc(list: &mut Vec<StoryId>, id: StoryId) -> bool {
    let pos = list.partition_point(|&x| x > id);
    if list.get(pos) == Some(&id) {
        list.remove(pos);
        true
    } else {
        false
    }
}

/// Removes the first occurrence of `id` from an unsorted `Vec<StoryId>` (`RemoveUnsorted`).
pub fn remove_unsorted(list: &mut Vec<StoryId>, id: StoryId) -> bool {
    if let Some(pos) = list.iter().position(|&x| x == id) {
        list.remove(pos);
        true
    } else {
        false
    }
}

/// Reorders element at `old_pos` to `new_pos` in-place (`base::reorder` in `albumReorderStories`).
///
/// Returns `false` if either index is out of bounds.
pub fn reorder_story_ids(list: &mut [StoryId], old_pos: usize, new_pos: usize) -> bool {
    if old_pos >= list.len() || new_pos >= list.len() {
        return false;
    }
    if old_pos < new_pos {
        list[old_pos..=new_pos].rotate_left(1);
    } else if old_pos > new_pos {
        list[new_pos..=old_pos].rotate_right(1);
    }
    true
}

/// Lookup key identifying a peer's story album (`Data::StoryAlbumIdsKey`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StoryAlbumIdsKey {
    /// Owner peer ID.
    pub peer_id: PeerId,
    /// Album ID (`STORIES_ALBUM_ID_SAVED = 0`, `STORIES_ALBUM_ID_ARCHIVE = -1`, or custom `> 0`).
    pub album_id: i32,
}

impl StoryAlbumIdsKey {
    /// Creates a new `StoryAlbumIdsKey`.
    #[inline]
    pub const fn new(peer_id: PeerId, album_id: i32) -> Self {
        Self { peer_id, album_id }
    }
}

/// Sparse window slice over an album's story IDs (`Data::StoriesIdsSlice`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoriesIdsSlice {
    /// Story IDs present in this window.
    pub ids: Vec<StoryId>,
    /// Total known story count in the album (`fullCount`).
    pub full_count: Option<usize>,
    /// Number of items skipped before `ids[0]` (`skippedBefore`).
    pub skipped_before: Option<usize>,
    /// Number of items skipped after `ids.last()` (`skippedAfter`).
    pub skipped_after: Option<usize>,
}

impl StoriesIdsSlice {
    /// Returns the number of loaded IDs in this slice.
    #[inline]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Returns `true` if `ids` is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Returns the index of `id` within `self.ids`, if present.
    pub fn index_of(&self, id: StoryId) -> Option<usize> {
        self.ids.iter().position(|&x| x == id)
    }

    /// Builds a slice centered around `around_id` with up to `limit` items before and after,
    /// matching `Data::AlbumStoriesIds` in `data_stories_ids.cpp`.
    ///
    /// Returns `(slice, should_load_more)`.
    pub fn build_album_window(
        loaded: &StoriesIds,
        total_count: usize,
        around_id: StoryId,
        limit: usize,
    ) -> (Self, bool) {
        let sorted = respecting_pinned(loaded);
        let center = sorted.iter().position(|&x| x == around_id).unwrap_or(0);
        let has_before = center;
        let has_after = sorted.len().saturating_sub(center);
        let should_load_more = has_after < limit;
        let take_before = has_before.min(limit);
        let take_after = has_after.min(limit);
        let start = center - take_before;
        let end = center + take_after;
        let ids = sorted[start..end].to_vec();
        let added = ids.len();
        let skipped_before = has_before - take_before;
        let skipped_after = total_count.saturating_sub(skipped_before + added);
        (
            Self {
                ids,
                full_count: Some(total_count),
                skipped_before: Some(skipped_before),
                skipped_after: Some(skipped_after),
            },
            should_load_more,
        )
    }
}
