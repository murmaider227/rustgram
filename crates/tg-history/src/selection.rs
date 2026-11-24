//! Text selection and cross-message selection model.
//!
//! Port of:
//! - `Telegram/SourceFiles/layout/layout_selection.h` & `layout_selection.cpp`
//! - `Telegram/SourceFiles/history/history_message_selection.h`
//! - `Telegram/SourceFiles/history/view/history_view_element.cpp` (`ShiftItemSelection`, `UnshiftItemSelection`)
//! - `Telegram/SourceFiles/history/view/history_view_list_widget.h` (multi-message selection model)

use tg_data::FullMsgId;

/// Sentinel representing a fully selected item (`FullSelection = TextSelection { 0xFFFF, 0xFFFF }`).
pub const FULL_SELECTION: TextSelection = TextSelection {
    from: 0xFFFF,
    to: 0xFFFF,
};

/// Sentinel representing all text selected within a single text object (`AllTextSelection`).
pub const ALL_TEXT_SELECTION: TextSelection = TextSelection {
    from: 0,
    to: 0xFFFF,
};

/// Maximum number of items addressable by a subgroup bitmask (`kMaxGroupSelectionItems = 0x0F`).
pub const MAX_GROUP_SELECTION_ITEMS: usize = 0x0F;

/// Symbol-offset selection range inside a single message (`TextSelection`).
///
/// Mirrors tdesktop's `uint16 from, to` encoding, including:
/// - Empty selection (`from == to && from != 0xFFFF`)
/// - Full item selection (`from == 0xFFFF && to == 0xFFFF`)
/// - Sub-group selection bitmask (`from == 0xFFFF && to != 0xFFFF`, where bit `i` selects group part `i`)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TextSelection {
    /// Start symbol offset (inclusive), or `0xFFFF` for full/subgroup sentinel.
    pub from: u16,
    /// End symbol offset (exclusive), or `0xFFFF` for full selection / bitmask for subgroup.
    pub to: u16,
}

impl TextSelection {
    /// Empty selection `(0, 0)`.
    pub const EMPTY: Self = Self { from: 0, to: 0 };

    /// Creates a selection range `[from, to)`.
    #[must_use]
    pub const fn new(from: u16, to: u16) -> Self {
        Self { from, to }
    }

    /// Returns `true` if no symbols (and no full/subgroup sentinel) are selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.from == self.to && self.from != 0xFFFF
    }

    /// Returns `true` if this equals [`FULL_SELECTION`].
    #[must_use]
    pub const fn is_full(self) -> bool {
        self.from == 0xFFFF && self.to == 0xFFFF
    }
}

/// Returns `true` if `selection` encodes a grouped-media item bitmask (`IsSubGroupSelection`).
#[must_use]
pub const fn is_sub_group_selection(selection: TextSelection) -> bool {
    selection.from == 0xFFFF && selection.to != 0xFFFF
}

/// Returns `true` if `selection` is a subgroup bitmask with item `index` selected (`IsGroupItemSelection`).
#[must_use]
pub const fn is_group_item_selection(selection: TextSelection, index: usize) -> bool {
    if index >= MAX_GROUP_SELECTION_ITEMS {
        return false;
    }
    is_sub_group_selection(selection) && (selection.to & (1u16 << index)) != 0
}

/// Returns the first selected item index in a subgroup bitmask, or `None` (`FirstGroupItemIndex`).
#[must_use]
pub const fn first_group_item_index(selection: TextSelection) -> Option<usize> {
    if !is_sub_group_selection(selection) {
        return None;
    }
    let mut i = 0;
    while i < MAX_GROUP_SELECTION_ITEMS {
        if (selection.to & (1u16 << i)) != 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Adds `index` to a subgroup bitmask selection (`AddGroupItemSelection`).
#[must_use]
pub const fn add_group_item_selection(selection: TextSelection, index: usize) -> TextSelection {
    if index >= MAX_GROUP_SELECTION_ITEMS {
        return selection;
    }
    let bit = 1u16 << index;
    TextSelection {
        from: 0xFFFF,
        to: if is_sub_group_selection(selection) {
            selection.to | bit
        } else {
            bit
        },
    }
}

/// Subtracts `by_length` from a text selection (`UnshiftItemSelection` in `history_view_element.cpp:680`).
#[must_use]
pub const fn unshift_item_selection(selection: TextSelection, by_length: u16) -> TextSelection {
    if selection.is_full() || is_sub_group_selection(selection) {
        return selection;
    }
    let from = selection.from.saturating_sub(by_length);
    let to = selection.to.saturating_sub(by_length);
    TextSelection { from, to }
}

/// Adds `by_length` to a text selection (`ShiftItemSelection` in `history_view_element.cpp:688`).
#[must_use]
pub const fn shift_item_selection(selection: TextSelection, by_length: u16) -> TextSelection {
    if selection.is_full() || is_sub_group_selection(selection) || selection.is_empty() {
        return selection;
    }
    let max_text = 0xFFFEu16;
    let from = if selection.from.saturating_add(by_length) > max_text {
        max_text
    } else {
        selection.from + by_length
    };
    let to = if selection.to.saturating_add(by_length) > max_text {
        max_text
    } else {
        selection.to + by_length
    };
    TextSelection { from, to }
}

/// A cursor endpoint inside a flat message text (`MessageSelectionFlatEndpoint` in `history_message_selection.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlatSelectionEndpoint {
    /// Character/symbol index within the message.
    pub symbol: u16,
    /// `true` if the cursor lies on the trailing half of `symbol`.
    pub after_symbol: bool,
}

impl FlatSelectionEndpoint {
    /// Creates a flat selection endpoint.
    #[must_use]
    pub const fn new(symbol: u16, after_symbol: bool) -> Self {
        Self {
            symbol,
            after_symbol,
        }
    }

    /// Effective boundary offset (`symbol + (after_symbol ? 1 : 0)`).
    #[must_use]
    pub const fn offset(self) -> u16 {
        self.symbol
            .saturating_add(if self.after_symbol { 1 } else { 0 })
    }
}

/// Selection state within a single message (`MessageSelection` in `history_message_selection.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageSelection {
    /// Normalized `[from, to)` symbol range.
    pub range: TextSelection,
    /// Selection drag anchor endpoint.
    pub anchor: FlatSelectionEndpoint,
    /// Selection drag focus endpoint.
    pub focus: FlatSelectionEndpoint,
}

impl MessageSelection {
    /// Builds a normalized `MessageSelection` from anchor and focus endpoints.
    #[must_use]
    pub const fn from_endpoints(
        anchor: FlatSelectionEndpoint,
        focus: FlatSelectionEndpoint,
    ) -> Self {
        let a = anchor.offset();
        let b = focus.offset();
        if a == b {
            return Self {
                range: TextSelection::EMPTY,
                anchor,
                focus,
            };
        }
        let range = if a < b {
            TextSelection::new(a, b)
        } else {
            TextSelection::new(b, a)
        };
        Self {
            range,
            anchor,
            focus,
        }
    }

    /// Returns `true` if no text is selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.range.is_empty()
    }

    /// Returns `true` if `symbol` lies inside `[range.from, range.to)`.
    #[must_use]
    pub const fn contains_symbol(self, symbol: u16) -> bool {
        !self.is_empty() && symbol >= self.range.from && symbol < self.range.to
    }
}

/// Endpoint for cross-message text or item selection in a history list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListSelectionEndpoint {
    /// Message ID of this endpoint.
    pub item_id: FullMsgId,
    /// Index of the message in the current list ordering.
    pub item_index: usize,
    /// Symbol cursor inside the message.
    pub text: FlatSelectionEndpoint,
}

/// Cross-message selection model spanning either part of one message or multiple messages.
///
/// Models tdesktop's selection state across a history list:
/// - Within a single item: sub-text selection `[from, to)`.
/// - Across multiple items: every item in `[min_index..=max_index]` receives [`FULL_SELECTION`]
///   (matching tdesktop's drag-select-across-messages behaviour where crossing into a second
///   message transitions into whole-message selection), or optionally partial-text selection
///   at endpoints via [`Self::selection_for_item_partial`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySelection {
    /// Drag start endpoint, if a selection is active.
    pub from: Option<ListSelectionEndpoint>,
    /// Drag end endpoint, if a selection is active.
    pub to: Option<ListSelectionEndpoint>,
    /// Explicitly toggled multi-selected items (checkbox / selection mode).
    pub selected_items: Vec<FullMsgId>,
}

impl HistorySelection {
    /// Creates an empty selection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            from: None,
            to: None,
            selected_items: Vec::new(),
        }
    }

    /// Clears all text and item selections.
    pub fn clear(&mut self) {
        self.from = None;
        self.to = None;
        self.selected_items.clear();
    }

    /// Starts a drag selection at `endpoint`.
    pub fn start_drag(&mut self, endpoint: ListSelectionEndpoint) {
        self.from = Some(endpoint);
        self.to = Some(endpoint);
    }

    /// Updates the drag focus to `endpoint`.
    pub fn update_drag(&mut self, endpoint: ListSelectionEndpoint) {
        if self.from.is_none() {
            self.from = Some(endpoint);
        }
        self.to = Some(endpoint);
    }

    /// Toggles whole-item selection for `item_id` (used in multi-select checkbox mode).
    pub fn toggle_item(&mut self, item_id: FullMsgId) {
        if let Some(pos) = self.selected_items.iter().position(|&id| id == item_id) {
            self.selected_items.remove(pos);
        } else {
            self.selected_items.push(item_id);
        }
    }

    /// Returns `true` if whole-item multi-selection mode is active.
    #[must_use]
    pub fn has_selected_items(&self) -> bool {
        !self.selected_items.is_empty()
    }

    /// Returns `true` if either text drag selection or multi-item selection is non-empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        if !self.selected_items.is_empty() {
            return false;
        }
        match (self.from, self.to) {
            (Some(a), Some(b)) => {
                a.item_index == b.item_index && a.text.offset() == b.text.offset()
            }
            _ => true,
        }
    }

    /// Returns the effective [`TextSelection`] for the item at `(item_index, item_id)`
    /// following tdesktop's behaviour: crossing into another message selects whole bubbles.
    #[must_use]
    pub fn selection_for_item(&self, item_index: usize, item_id: FullMsgId) -> TextSelection {
        if self.selected_items.contains(&item_id) {
            return FULL_SELECTION;
        }
        let (Some(a), Some(b)) = (self.from, self.to) else {
            return TextSelection::EMPTY;
        };
        if a.item_index == b.item_index {
            if item_index == a.item_index {
                MessageSelection::from_endpoints(a.text, b.text).range
            } else {
                TextSelection::EMPTY
            }
        } else {
            let (lo, hi) = if a.item_index < b.item_index {
                (a.item_index, b.item_index)
            } else {
                (b.item_index, a.item_index)
            };
            if item_index >= lo && item_index <= hi {
                FULL_SELECTION
            } else {
                TextSelection::EMPTY
            }
        }
    }

    /// Returns a partial-text selection across message boundaries where the first item
    /// selects `[start..text_len)`, middle items select `[0..text_len)`, and the last item
    /// selects `[0..end)`.
    #[must_use]
    pub fn selection_for_item_partial(&self, item_index: usize, text_len: u16) -> TextSelection {
        let (Some(a), Some(b)) = (self.from, self.to) else {
            return TextSelection::EMPTY;
        };
        if a.item_index == b.item_index {
            return if item_index == a.item_index {
                MessageSelection::from_endpoints(a.text, b.text).range
            } else {
                TextSelection::EMPTY
            };
        }
        let (first, last) = if a.item_index < b.item_index {
            (a, b)
        } else {
            (b, a)
        };
        if item_index < first.item_index || item_index > last.item_index {
            TextSelection::EMPTY
        } else if item_index == first.item_index {
            let start = first.text.offset().min(text_len);
            TextSelection::new(start, text_len)
        } else if item_index == last.item_index {
            let end = last.text.offset().min(text_len);
            TextSelection::new(0, end)
        } else {
            TextSelection::new(0, text_len)
        }
    }
}
