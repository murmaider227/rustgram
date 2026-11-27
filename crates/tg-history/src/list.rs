//! History list model: ordering, visible range enumeration, Y hit-testing,
//! sticky userpics, scroll anchoring, jump-to-message highlighting, and preload triggers.
//!
//! Port of:
//! - `Telegram/SourceFiles/history/history_inner_widget.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_list_widget.h` & `history_view_list_widget.cpp`
//! - `Telegram/SourceFiles/history/history_view_highlight_manager.h` & `.cpp`

use std::collections::VecDeque;

use crate::consts::{
    ACTIVE_FADE_IN_DURATION_MS, ACTIVE_FADE_OUT_DURATION_MS, FADE_WRAP_DURATION_MS,
    HISTORY_PADDING_BOTTOM, HISTORY_UNREAD_BAR_MARGIN, LINE_WIDTH, MSG_MARGIN, MSG_PHOTO_SIZE,
    PRELOAD_IF_LESS_THAN_SCREENS, PRELOADED_SCREENS_COUNT, PRELOADED_SCREENS_COUNT_FULL,
};
use crate::element::{AttachMetadata, SequenceAttachState, compute_sequence_attach_states};
use crate::selection::{HistorySelection, TextSelection, is_sub_group_selection};
use tg_data::{FullMsgId, MessagePosition, MsgId};

/// Returns `true` if `pos` is a concrete (non-zero) message position.
#[must_use]
pub const fn is_message_position_valid(pos: MessagePosition) -> bool {
    pos.date != 0 || !pos.full_id.msg.is_zero()
}

/// A single laid-out entry stored in [`HistoryList`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryListItem {
    /// Unique full message ID.
    pub id: FullMsgId,
    /// Sortable position (`date`, `full_id`).
    pub position: MessagePosition,
    /// Attachment and date-separator metadata.
    pub attach_meta: AttachMetadata,
    /// Computed attachment & date-separator state.
    pub attach_state: SequenceAttachState,
    /// Logical Y coordinate relative to `items_top` (`view->y()`).
    pub y: i32,
    /// Laid-out vertical height in logical pixels (`view->height()`).
    pub height: i32,
    /// Top margin of this item (`view->marginTop()`).
    pub margin_top: i32,
    /// Bottom margin of this item (`view->marginBottom()`).
    pub margin_bottom: i32,
    /// True if this item has a sender avatar column (`hasFromPhoto()`).
    pub has_from_photo: bool,
    /// True if this item actually displays its sender avatar (typically bottom of an attached group).
    pub display_from_photo: bool,
    /// True if this item carries the `UnreadBar`.
    pub has_unread_bar: bool,
    /// Height of the date badge above this item (`0` if `!attach_state.display_date`).
    pub displayed_date_height: i32,
}

impl HistoryListItem {
    /// Creates a new list item with default `y = 0` and computed attachment state empty.
    #[must_use]
    pub fn new(
        id: FullMsgId,
        position: MessagePosition,
        attach_meta: AttachMetadata,
        height: i32,
    ) -> Self {
        Self {
            id,
            position,
            attach_meta,
            attach_state: SequenceAttachState::default(),
            y: 0,
            height,
            margin_top: MSG_MARGIN.top,
            margin_bottom: MSG_MARGIN.bottom,
            has_from_photo: false,
            display_from_photo: false,
            has_unread_bar: false,
            displayed_date_height: 0,
        }
    }
}

/// Optional vertical collapse gap injected at a logical Y offset (`ListWidget::collapseGaps`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CollapseGap {
    /// Where the gap begins, in item coordinates (`HistoryListItem::y`, without `items_top`, so
    /// the gap stays put when a short list's top margin changes): items with `y >= abs_y` are
    /// pushed down by `height`.
    pub abs_y: i32,
    /// Height of the gap in pixels.
    pub height: i32,
}

/// Visible item span yielded by [`HistoryList::visible_items_top_to_bottom`] or
/// [`HistoryList::visible_items_bottom_to_top`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleItem {
    /// Index in [`HistoryList::items`].
    pub index: usize,
    /// Message ID of the item.
    pub id: FullMsgId,
    /// Visual top Y coordinate (including `items_top` and any preceding collapse gaps).
    pub top: i32,
    /// Visual bottom Y coordinate (`top + height`).
    pub bottom: i32,
}

/// Positioned sticky sender avatar for an attached message group (`ListWidget::enumerateUserpics`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserpicPlacement {
    /// Index of the bottom-most visible item in the attached group.
    pub item_index: usize,
    /// Message ID owning the userpic.
    pub item_id: FullMsgId,
    /// Top Y coordinate at which the `MSG_PHOTO_SIZE` avatar should be painted.
    pub top: i32,
}

/// Saved scroll anchor (`ListWidget::ScrollTopState` in `history_view_list_widget.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollTopState {
    /// Anchored message position (invalid/empty when anchored to the newest edge).
    pub item: MessagePosition,
    /// Pixel offset from the anchored item's top to `visible_top` (`visible_top - item_top`).
    pub shift: i32,
}

/// Preload trigger recommendation (`ListWidget::checkMoveToOtherViewer`,
/// `history_view_list_widget.cpp:2544-2630`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreloadDecision {
    /// True if the viewport is within `2 * visible_height` of the top and older items are not yet loaded.
    pub preload_top: bool,
    /// True if the viewport is within `2 * visible_height` of the bottom and newer items are not yet loaded.
    pub preload_bottom: bool,
    /// Recommended message ID limit (`preloadIdsLimit`) for the viewer slice.
    pub recommended_ids_limit: i32,
    /// Target item index around which to reload, if a preload is needed.
    pub around_index: Option<usize>,
}

/// Ordered history list model managing item coordinates, visibility, hit-testing,
/// scroll anchoring, selection, and preload triggers.
#[derive(Debug, Clone)]
pub struct HistoryList {
    items: Vec<HistoryListItem>,
    items_top: i32,
    items_height: i32,
    bottom_padding: i32,
    collapse_gaps: Vec<CollapseGap>,
    /// True when newest items are at index 0 (inverted chat mode).
    pub inverted: bool,
    /// True when the oldest boundary (top in normal mode) is completely loaded.
    pub loaded_at_top: bool,
    /// True when the newest boundary (bottom in normal mode) is completely loaded.
    pub loaded_at_bottom: bool,
    /// Active scroll anchor preserved across prepends / height changes.
    pub scroll_top_state: ScrollTopState,
    /// Active text / multi-item selection state.
    pub selection: HistorySelection,
    /// Index of the item carrying the unread bar, if any.
    pub unread_bar_index: Option<usize>,
    /// Current `idsLimit` tracked by the viewer (used by preload calculation).
    pub ids_limit: i32,
    /// Current `aroundIndex` tracked by the viewer.
    pub around_index: Option<usize>,
}

impl Default for HistoryList {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryList {
    /// Creates an empty history list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            items_top: 0,
            items_height: 0,
            bottom_padding: HISTORY_PADDING_BOTTOM,
            collapse_gaps: Vec::new(),
            inverted: false,
            loaded_at_top: false,
            loaded_at_bottom: true,
            scroll_top_state: ScrollTopState::default(),
            selection: HistorySelection::new(),
            unread_bar_index: None,
            ids_limit: 0,
            around_index: None,
        }
    }

    /// Returns all items in list order.
    #[must_use]
    pub fn items(&self) -> &[HistoryListItem] {
        &self.items
    }

    /// Returns a mutable slice of items (call [`Self::relayout_positions`] after modifying heights).
    pub fn items_mut(&mut self) -> &mut [HistoryListItem] {
        &mut self.items
    }

    /// Number of items in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if the list contains no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Y offset where the first item starts (`_itemsTop`).
    #[must_use]
    pub const fn items_top(&self) -> i32 {
        self.items_top
    }

    /// Sets `items_top` (e.g. when content is shorter than the viewport and bottom-aligned).
    pub fn set_items_top(&mut self, top: i32) {
        self.items_top = top;
    }

    /// Total height of all items excluding gaps and bottom padding (`_itemsHeight`).
    #[must_use]
    pub const fn items_height(&self) -> i32 {
        self.items_height
    }

    /// Total scrollable height of the list widget.
    #[must_use]
    pub fn total_height(&self) -> i32 {
        self.items_top + self.items_height + self.collapse_gap_total() + self.bottom_padding
    }

    /// Sets active collapse gaps.
    pub fn set_collapse_gaps(&mut self, gaps: Vec<CollapseGap>) {
        self.collapse_gaps = gaps;
    }

    /// Sum of all collapse gap heights.
    #[must_use]
    pub fn collapse_gap_total(&self) -> i32 {
        self.collapse_gaps.iter().map(|g| g.height).sum()
    }

    /// Logical top coordinate of item `index` (`itemTop(view) = _itemsTop + view->y()`).
    #[must_use]
    pub fn item_top(&self, index: usize) -> i32 {
        self.items_top + self.items[index].y
    }

    /// Visual top coordinate of item `index`, adding any collapse gaps above it.
    #[must_use]
    pub fn visual_item_top(&self, index: usize) -> i32 {
        let logical = self.item_top(index);
        let item_y = self.items[index].y;
        let shift: i32 = self
            .collapse_gaps
            .iter()
            .filter(|g| item_y >= g.abs_y)
            .map(|g| g.height)
            .sum();
        logical + shift
    }

    /// Replaces the entire list of items, recalculates attachment/date states, and assigns `y` offsets.
    pub fn set_items(&mut self, items: Vec<HistoryListItem>) {
        self.items = items;
        self.recount_attachments();
        self.relayout_positions();
    }

    /// Appends an item to the bottom (newest end) and updates attachment + layout.
    pub fn push_back(&mut self, item: HistoryListItem) {
        self.items.push(item);
        self.recount_attachments();
        self.relayout_positions();
    }

    /// Prepends older items to the top of the list (e.g. when loading history upward).
    ///
    /// Preserves any existing `unread_bar_index` by shifting it forward by `older.len()`.
    /// Call [`Self::save_scroll_state`] before `prepend_older` and [`Self::restore_scroll_top`]
    /// afterward to keep the user's viewport anchored to the same item!
    pub fn prepend_older(&mut self, mut older: Vec<HistoryListItem>) {
        let added = older.len();
        if added == 0 {
            return;
        }
        if let Some(ref mut idx) = self.unread_bar_index {
            *idx += added;
        }
        if let Some(ref mut around) = self.around_index {
            *around += added;
        }
        older.append(&mut self.items);
        self.items = older;
        self.recount_attachments();
        self.relayout_positions();
    }

    /// Sets the unread bar onto item `index` (or clears it with `None`) and updates attachments.
    pub fn set_unread_bar(&mut self, index: Option<usize>) {
        self.unread_bar_index = index.filter(|&i| i < self.items.len());
        for (i, item) in self.items.iter_mut().enumerate() {
            item.has_unread_bar = self.unread_bar_index == Some(i);
        }
        self.recount_attachments();
    }

    /// Recomputes date separators, attach-to-previous/next flags, and `display_from_photo`
    /// across all items.
    pub fn recount_attachments(&mut self) {
        let metas: Vec<AttachMetadata> =
            self.items.iter().map(|it| it.attach_meta.clone()).collect();
        let states = compute_sequence_attach_states(&metas, self.unread_bar_index);
        for (item, state) in self.items.iter_mut().zip(states) {
            item.attach_state = state;
            item.display_from_photo = item.has_from_photo && !state.attached_to_next;
        }
    }

    /// Recomputes `y` coordinates and `items_height` from item heights.
    pub fn relayout_positions(&mut self) {
        let mut y = 0;
        for item in &mut self.items {
            item.y = y;
            y += item.height;
        }
        self.items_height = y;
    }

    /// Finds the index of the item matching `id`.
    #[must_use]
    pub fn index_of_id(&self, id: FullMsgId) -> Option<usize> {
        self.items.iter().position(|it| it.id == id)
    }

    /// Finds the index of the item nearest to `position` via binary search
    /// (`ListWidget::findNearestItem`).
    #[must_use]
    pub fn find_nearest_item(&self, position: MessagePosition) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let idx = if self.inverted {
            self.items.partition_point(|it| it.position > position)
        } else {
            self.items.partition_point(|it| it.position < position)
        };
        Some(idx.min(self.items.len() - 1))
    }

    /// Hit-tests `y` and returns the clamped item index (`ListWidget::findItemIndexByY`,
    /// `history_view_list_widget.cpp:4065-4076`).
    #[must_use]
    pub fn find_item_index_by_y(&self, y: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        // Binary search for the first item whose visual bottom > y.
        let idx = self.items.partition_point(|item| {
            let logical = self.items_top + item.y;
            let shift: i32 = self
                .collapse_gaps
                .iter()
                .filter(|g| item.y >= g.abs_y)
                .map(|g| g.height)
                .sum();
            logical + shift + item.height <= y
        });
        Some(idx.min(self.items.len() - 1))
    }

    /// Hit-tests `y` strictly within `[items_top, items_top + items_height + gap_total)`
    /// (`ListWidget::strictFindItemByY`, `history_view_list_widget.cpp:4078-4089`).
    #[must_use]
    pub fn strict_find_item_by_y(&self, y: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let gap_total = self.collapse_gap_total();
        if y >= self.items_top && y < self.items_top + self.items_height + gap_total {
            self.find_item_index_by_y(y)
        } else {
            None
        }
    }

    /// Computes all items intersecting `[visible_top, visible_bottom)` in top-to-bottom order
    /// (`ListWidget::enumerateItems<TopToBottom>`, `history_view_list_widget.cpp:278-396`).
    #[must_use]
    pub fn visible_items_top_to_bottom(
        &self,
        visible_top: i32,
        visible_bottom: i32,
    ) -> Vec<VisibleItem> {
        let gap_total = self.collapse_gap_total();
        if self.items.is_empty()
            || visible_bottom <= self.items_top
            || self.items_top + self.items_height + gap_total <= visible_top
            || visible_top >= visible_bottom
        {
            return Vec::new();
        }

        let target = visible_top - gap_total;
        let mut from = self
            .items
            .partition_point(|elem| self.items_top + elem.y + elem.height <= target);
        if from == self.items.len() {
            from -= 1;
        }

        let gap_count = self.collapse_gaps.len();
        let mut next_gap_idx = 0usize;
        let mut collapse_shift = 0i32;
        let first_y = self.items[from].y;
        while next_gap_idx < gap_count && first_y >= self.collapse_gaps[next_gap_idx].abs_y {
            collapse_shift += self.collapse_gaps[next_gap_idx].height;
            next_gap_idx += 1;
        }

        let mut out = Vec::new();
        while from < self.items.len() {
            let logical_top = self.item_top(from);
            while next_gap_idx < gap_count
                && self.items[from].y >= self.collapse_gaps[next_gap_idx].abs_y
            {
                collapse_shift += self.collapse_gaps[next_gap_idx].height;
                next_gap_idx += 1;
            }
            let item_top = logical_top + collapse_shift;
            let item_bottom = item_top + self.items[from].height;

            if item_bottom <= visible_top {
                from += 1;
                continue;
            }
            if item_top >= visible_bottom {
                break;
            }
            out.push(VisibleItem {
                index: from,
                id: self.items[from].id,
                top: item_top,
                bottom: item_bottom,
            });
            if item_bottom >= visible_bottom {
                break;
            }
            from += 1;
        }
        out
    }

    /// Computes all items intersecting `[visible_top, visible_bottom)` in bottom-to-top order
    /// (`ListWidget::enumerateItems<BottomToTop>`).
    #[must_use]
    pub fn visible_items_bottom_to_top(
        &self,
        visible_top: i32,
        visible_bottom: i32,
    ) -> Vec<VisibleItem> {
        let mut items = self.visible_items_top_to_bottom(visible_top, visible_bottom);
        items.reverse();
        items
    }

    /// Computes sticky userpic placements along attached message groups
    /// (`ListWidget::enumerateUserpics`, `history_view_list_widget.cpp:399-450`).
    #[must_use]
    pub fn enumerate_userpics(
        &self,
        visible_top: i32,
        visible_bottom: i32,
    ) -> Vec<UserpicPlacement> {
        let visible = self.visible_items_top_to_bottom(visible_top, visible_bottom);
        let mut placements = Vec::new();
        let mut lowest_attached_item_top: Option<i32> = None;
        let userpic_min_bottom_skip = HISTORY_PADDING_BOTTOM + MSG_MARGIN.bottom;

        for vis in visible {
            let item = &self.items[vis.index];
            if item.attach_meta.is_service {
                continue;
            }
            if lowest_attached_item_top.is_none() && item.attach_state.attached_to_next {
                lowest_attached_item_top = Some(vis.top + item.margin_top);
            }
            if item.display_from_photo
                || (item.has_from_photo
                    && item.attach_state.attached_to_next
                    && vis.bottom >= visible_bottom)
            {
                let pack_top = lowest_attached_item_top.unwrap_or(vis.top + item.margin_top);
                let mut userpic_bottom =
                    (vis.bottom - item.margin_bottom).min(visible_bottom - userpic_min_bottom_skip);
                userpic_bottom = userpic_bottom.max(pack_top + MSG_PHOTO_SIZE);
                placements.push(UserpicPlacement {
                    item_index: vis.index,
                    item_id: item.id,
                    top: userpic_bottom - MSG_PHOTO_SIZE,
                });
            }
            if !item.attach_state.attached_to_next {
                lowest_attached_item_top = None;
            }
        }
        placements
    }

    // -----------------------------------------------------------------------
    // Scroll anchoring & jump-to-message (history_view_list_widget.cpp:1080-1616)
    // -----------------------------------------------------------------------

    /// Returns `true` if the viewport is pinned to the newest edge (`ListWidget::atNewestEdge`).
    #[must_use]
    pub fn at_newest_edge(&self, visible_top: i32, visible_bottom: i32) -> bool {
        if self.inverted {
            visible_top == 0
        } else {
            visible_bottom >= self.total_height()
        }
    }

    /// Computes the current scroll anchor (`ListWidget::countScrollState`,
    /// `history_view_list_widget.cpp:4091-4108`).
    #[must_use]
    pub fn count_scroll_state(&self, visible_top: i32, visible_bottom: i32) -> ScrollTopState {
        if self.items.is_empty()
            || (self.loaded_at_bottom && self.at_newest_edge(visible_top, visible_bottom))
        {
            return ScrollTopState::default();
        }
        let Some(index) = self.find_item_index_by_y(visible_top) else {
            return ScrollTopState::default();
        };
        for i in index..self.items.len() {
            let item = &self.items[i];
            if !item.attach_meta.is_sponsored {
                return ScrollTopState {
                    item: item.position,
                    shift: visible_top - self.item_top(i),
                };
            }
        }
        ScrollTopState::default()
    }

    /// Saves the current scroll anchor into `self.scroll_top_state` if one is not already stored
    /// (`ListWidget::saveScrollState`).
    pub fn save_scroll_state(&mut self, visible_top: i32, visible_bottom: i32) {
        if !is_message_position_valid(self.scroll_top_state.item) {
            self.scroll_top_state = self.count_scroll_state(visible_top, visible_bottom);
        }
    }

    /// Unconditionally remembers the top visible item as scroll anchor before an insertion
    /// (`ListWidget::rememberScrollAnchor`, `history_view_list_widget.cpp:1080-1086`).
    pub fn remember_scroll_anchor(&mut self, visible_top: i32) {
        if !is_message_position_valid(self.scroll_top_state.item)
            && let Some(index) = self.find_item_index_by_y(visible_top)
        {
            self.scroll_top_state = ScrollTopState {
                item: self.items[index].position,
                shift: visible_top - self.item_top(index),
            };
        }
    }

    /// Restores the scroll top from `self.scroll_top_state` and clears the saved anchor
    /// (`ListWidget::restoreScrollState` / `applyScrollTopState`,
    /// `history_view_list_widget.cpp:1578-1616`).
    ///
    /// Returns `Some(new_visible_top)` when an anchor was active and found.
    pub fn restore_scroll_top(&mut self) -> Option<i32> {
        if !is_message_position_valid(self.scroll_top_state.item) {
            return None;
        }
        let saved = self.scroll_top_state;
        self.scroll_top_state = ScrollTopState::default();
        let index = self.find_nearest_item(saved.item)?;
        Some((self.item_top(index) + saved.shift).max(0))
    }

    /// Computes the target `scroll_top` to center item `index` in the viewport
    /// (`ListWidget::scrollTopForView`, `history_view_list_widget.cpp:1203-1226`).
    #[must_use]
    pub fn scroll_top_for_index(
        &self,
        index: usize,
        visible_top: i32,
        visible_bottom: i32,
    ) -> Option<i32> {
        let item = self.items.get(index)?;
        let available = visible_bottom - visible_top;
        if available <= 0 {
            return None;
        }
        let top = self.item_top(index);
        let height_left = available - item.height;
        if height_left >= 0 {
            Some((top - height_left / 2).max(0))
        } else {
            Some(top)
        }
    }

    /// Computes the target `scroll_top` for jumping to `position` (supports
    /// [`MessagePosition::MAX`], [`MessagePosition::UNREAD`], and concrete positions;
    /// `ListWidget::scrollTopForPosition`, `history_view_list_widget.cpp:1157-1201`).
    #[must_use]
    pub fn scroll_top_for_position(
        &self,
        mut position: MessagePosition,
        visible_top: i32,
        visible_bottom: i32,
    ) -> Option<i32> {
        if position == MessagePosition::UNREAD {
            if let Some(bar_idx) = self.unread_bar_index {
                let shift = LINE_WIDTH + HISTORY_UNREAD_BAR_MARGIN;
                return Some(self.item_top(bar_idx) + shift);
            }
            position = MessagePosition::MAX;
        }
        if visible_top >= visible_bottom {
            return None;
        }
        if position == MessagePosition::MAX {
            if self.inverted {
                return self.loaded_at_top.then_some(0);
            }
            if self.loaded_at_bottom {
                return Some((self.total_height() - (visible_bottom - visible_top)).max(0));
            }
            return None;
        }
        if self.items.is_empty() {
            return None;
        }
        // Check if target lies outside loaded range.
        if !self.loaded_at_top {
            let first_pos = self.items[0].position;
            let below = if self.inverted {
                first_pos < position
            } else {
                first_pos > position
            };
            if below {
                return None;
            }
        }
        if !self.loaded_at_bottom {
            let last_pos = self.items[self.items.len() - 1].position;
            let above = if self.inverted {
                last_pos > position
            } else {
                last_pos < position
            };
            if above {
                return None;
            }
        }
        let index = self.find_nearest_item(position)?;
        self.scroll_top_for_index(index, visible_top, visible_bottom)
    }

    // -----------------------------------------------------------------------
    // Preload triggers (history_view_list_widget.cpp:2544-2630)
    // -----------------------------------------------------------------------

    /// Evaluates whether more messages should be preloaded above or below the viewport
    /// (`ListWidget::checkMoveToOtherViewer`, `history_view_list_widget.cpp:2544-2630`).
    #[must_use]
    pub fn check_preload(&self, visible_top: i32, visible_bottom: i32) -> PreloadDecision {
        let visible_height = visible_bottom - visible_top;
        if visible_height <= 0
            || self.items.is_empty()
            || is_message_position_valid(self.scroll_top_state.item)
        {
            return PreloadDecision {
                preload_top: false,
                preload_bottom: false,
                recommended_ids_limit: self.ids_limit,
                around_index: None,
            };
        }

        let avg_height = (self.items_height / (self.items.len() as i32)).max(1);
        let preloaded_height = PRELOADED_SCREENS_COUNT_FULL * visible_height;
        let preloaded_count = preloaded_height / avg_height;
        let preload_ids_limit_min = (preloaded_count / 2) + 1;
        let preload_ids_limit = preload_ids_limit_min + (visible_height / avg_height);

        let preload_before = PRELOAD_IF_LESS_THAN_SCREENS * visible_height;
        let preload_top = (visible_top < preload_before) && !self.loaded_at_top;
        let preload_bottom =
            (self.total_height() - visible_bottom < preload_before) && !self.loaded_at_bottom;

        let min_screen_delta = PRELOADED_SCREENS_COUNT - PRELOAD_IF_LESS_THAN_SCREENS;
        let min_id_delta = (min_screen_delta * visible_height) / avg_height;

        let needs_reload = |target_index: usize| -> bool {
            if self.ids_limit < preload_ids_limit_min {
                return true;
            }
            match self.around_index {
                Some(around) => {
                    (target_index as isize - around as isize).unsigned_abs() as i32 >= min_id_delta
                }
                None => true,
            }
        };

        let mut around = None;
        if preload_top {
            if let Some(top_idx) = self.find_item_index_by_y(visible_top)
                && needs_reload(top_idx)
            {
                around = Some(top_idx);
            }
        } else if preload_bottom
            && let Some(bot_idx) = self.find_item_index_by_y(visible_bottom)
            && needs_reload(bot_idx)
        {
            around = Some(bot_idx);
        }

        PreloadDecision {
            preload_top,
            preload_bottom,
            recommended_ids_limit: preload_ids_limit,
            around_index: around,
        }
    }
}

// ---------------------------------------------------------------------------
// Jump-to-message / highlight animation state machine
// Port of Telegram/SourceFiles/history/history_view_highlight_manager.cpp
// ---------------------------------------------------------------------------

/// A request to highlight a jumped-to message (or a quote range inside it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightTarget {
    /// Target message ID (album leader if grouped).
    pub item_id: FullMsgId,
    /// Optional sub-range or sub-group bitmask within the message.
    pub range: TextSelection,
    /// Optional poll option ID highlighted within a poll.
    pub poll_option: Vec<u8>,
}

impl HighlightTarget {
    /// Highlights the full message `item_id`.
    #[must_use]
    pub const fn whole_message(item_id: FullMsgId) -> Self {
        Self {
            item_id,
            range: TextSelection::EMPTY,
            poll_option: Vec::new(),
        }
    }

    /// Returns `true` if this highlights a specific text quote or poll option
    /// (triggers the collapse-then-fade curve).
    #[must_use]
    pub fn has_text_part(&self) -> bool {
        (!self.range.is_empty() || !self.poll_option.is_empty())
            && !is_sub_group_selection(self.range)
    }
}

/// Paint-time highlight state returned for an item (`Ui::ChatPaintHighlight`).
#[derive(Debug, Clone, PartialEq)]
pub struct HighlightPaintState {
    /// Highlight overlay opacity in `0.0..=1.0`.
    pub opacity: f64,
    /// Quote highlight collapse progress in `0.0..=1.0`.
    pub collapsion: f64,
    /// Highlighted sub-range or subgroup bitmask.
    pub range: TextSelection,
}

impl Default for HighlightPaintState {
    fn default() -> Self {
        Self {
            opacity: 0.0,
            collapsion: 0.0,
            range: TextSelection::EMPTY,
        }
    }
}

/// Manages jump-to-message highlight animations and queueing
/// (`HistoryView::ElementHighlighter` in `history_view_highlight_manager.cpp`).
#[derive(Debug, Clone, Default)]
pub struct ElementHighlighter {
    active: Option<HighlightTarget>,
    started_at_ms: i64,
    last_highlighted_id: FullMsgId,
    queue: VecDeque<HighlightTarget>,
}

impl ElementHighlighter {
    /// Creates an idle highlighter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts highlighting `target` immediately at timestamp `now_ms`.
    pub fn highlight(&mut self, target: HighlightTarget, now_ms: i64) {
        self.active = Some(target);
        self.started_at_ms = now_ms;
    }

    /// Queues `target` if another highlight is currently active, otherwise starts it now.
    pub fn enqueue(&mut self, target: HighlightTarget, now_ms: i64) {
        self.tick(now_ms);
        if self.active.is_none() && self.queue.is_empty() {
            self.highlight(target, now_ms);
        } else if self.active.as_ref() != Some(&target) && !self.queue.contains(&target) {
            self.queue.push_back(target);
        }
    }

    /// Clears active and queued highlights (`ElementHighlighter::clear`).
    pub fn clear(&mut self) {
        self.active = None;
        self.last_highlighted_id = FullMsgId::ZERO;
        self.queue.clear();
    }

    /// Advances the highlight timeline to `now_ms`, finishing completed highlights
    /// and starting the next queued item if any.
    pub fn tick(&mut self, now_ms: i64) {
        let Some(ref target) = self.active else {
            return;
        };
        let total_duration = Self::total_duration_ms(target.has_text_part());
        if now_ms - self.started_at_ms >= total_duration {
            self.last_highlighted_id = target.item_id;
            self.active = None;
            if let Some(next) = self.queue.pop_front() {
                self.highlight(next, now_ms);
            }
        }
    }

    /// Returns `true` if a highlight animation is currently active at `now_ms`.
    #[must_use]
    pub fn is_animating(&self, now_ms: i64) -> bool {
        let Some(ref target) = self.active else {
            return false;
        };
        now_ms - self.started_at_ms < Self::total_duration_ms(target.has_text_part())
    }

    /// Returns the most recently highlighted message ID (`latestSingleHighlightedMsgId`).
    #[must_use]
    pub fn latest_highlighted_msg_id(&self) -> MsgId {
        match &self.active {
            Some(t) => t.item_id.msg,
            None => self.last_highlighted_id.msg,
        }
    }

    /// Computes the paint-time highlight state for `item_id` at `now_ms`.
    ///
    /// Exact timeline port of `ElementHighlighter::AnimationManager::start`
    /// (`history_view_highlight_manager.cpp:238-308`):
    /// - Whole message: fade in `0 -> 1` over `500ms`, then fade out `1 -> 0` over `3000ms`.
    /// - Text quote part: fade in `0 -> 1` over `500ms`, hold `500ms`, collapse `0 -> 1` over
    ///   `200ms`, hold `3000ms`, then fade out `1 -> 0` over `500ms`.
    #[must_use]
    pub fn state_for(&self, item_id: FullMsgId, now_ms: i64) -> HighlightPaintState {
        let Some(ref target) = self.active else {
            return HighlightPaintState::default();
        };
        if target.item_id != item_id {
            return HighlightPaintState::default();
        }
        let elapsed = (now_ms - self.started_at_ms).max(0);
        let fade_in = ACTIVE_FADE_IN_DURATION_MS;
        let fade_out = ACTIVE_FADE_OUT_DURATION_MS;
        let wrap = FADE_WRAP_DURATION_MS;

        let (opacity, collapsion) = if !target.has_text_part() {
            if elapsed < fade_in {
                (elapsed as f64 / fade_in as f64, 0.0)
            } else if elapsed < fade_in + fade_out {
                let t = (elapsed - fade_in) as f64 / fade_out as f64;
                (1.0 - t, 0.0)
            } else {
                (0.0, 0.0)
            }
        } else if elapsed < fade_in {
            (elapsed as f64 / fade_in as f64, 0.0)
        } else if elapsed < 2 * fade_in {
            (1.0, 0.0)
        } else if elapsed < 2 * fade_in + wrap {
            let t = (elapsed - 2 * fade_in) as f64 / wrap as f64;
            (1.0, t)
        } else if elapsed < 2 * fade_in + wrap + fade_out {
            (1.0, 1.0)
        } else if elapsed < 3 * fade_in + wrap + fade_out {
            let t = (elapsed - (2 * fade_in + wrap + fade_out)) as f64 / fade_in as f64;
            (1.0 - t, 1.0)
        } else {
            (0.0, 1.0)
        };

        HighlightPaintState {
            opacity,
            collapsion,
            range: target.range,
        }
    }

    const fn total_duration_ms(with_text_part: bool) -> i64 {
        if with_text_part {
            3 * ACTIVE_FADE_IN_DURATION_MS + FADE_WRAP_DURATION_MS + ACTIVE_FADE_OUT_DURATION_MS
        } else {
            ACTIVE_FADE_IN_DURATION_MS + ACTIVE_FADE_OUT_DURATION_MS
        }
    }
}
