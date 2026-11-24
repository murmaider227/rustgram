//! Exact port of `layout/layout_mosaic.h`, `layout/layout_mosaic.cpp`, and
//! `layout/layout_position.cpp` (used for inline bot photo/GIF/thumbnail galleries).

use serde::{Deserialize, Serialize};

use crate::keyboard_layout::Rect;

/// Row multiplier for encoding `(row, column)` into a single `i32` index.
///
/// Port of `kMatrixRowShift` in `layout/layout_position.cpp:13`.
pub const MATRIX_ROW_SHIFT: i32 = 40_000;

/// Maximum number of items placed into a single mosaic row.
///
/// Port of `AbstractMosaicLayout::kInlineItemsMaxPerRow` (`layout/layout_mosaic.h:61`).
pub const INLINE_ITEMS_MAX_PER_ROW: usize = 5;

/// Default minimum laid-out width of a mosaic cell (`st::inlineResultsMinWidth = 48px`).
pub const DEFAULT_INLINE_RESULTS_MIN_WIDTH: i32 = 48;

/// Row/column coordinates inside a mosaic layout (`Layout::Position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Position {
    /// Zero-based row index, or `-1` if invalid.
    pub row: i32,
    /// Zero-based column index, or `-1` if invalid.
    pub column: i32,
}

/// Encodes `(row, column)` into a flat position index (`row * 40000 + column`).
///
/// Port of `Layout::PositionToIndex` (`layout/layout_position.cpp:23-25`).
pub const fn position_to_index(row: i32, column: i32) -> i32 {
    row * MATRIX_ROW_SHIFT + column
}

/// Decodes a flat position index back into [`Position`].
///
/// Port of `Layout::IndexToPosition` (`layout/layout_position.cpp:17-21`).
pub const fn index_to_position(index: i32) -> Position {
    if index >= 0 {
        Position {
            row: index / MATRIX_ROW_SHIFT,
            column: index % MATRIX_ROW_SHIFT,
        }
    } else {
        Position {
            row: -1,
            column: -1,
        }
    }
}

/// Padding margins around the mosaic content (`QMargins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Margins {
    /// Left padding in pixels.
    pub left: i32,
    /// Top padding in pixels.
    pub top: i32,
    /// Right padding in pixels.
    pub right: i32,
    /// Bottom padding in pixels.
    pub bottom: i32,
}

impl Margins {
    /// Creates [`Margins`] from `(left, top, right, bottom)`.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// Hit-test result returned by [`MosaicLayout::find_by_point`].
///
/// Port of `Mosaic::Layout::FoundItem` (`layout/layout_mosaic.h:15-19`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FoundItem {
    /// Encoded `(row, column)` index (`position_to_index`), or `-1` if no item.
    pub index: i32,
    /// `true` if the point landed strictly inside an item's bounds.
    pub exact: bool,
    /// X offset relative to the found item's top-left corner.
    pub relative_x: i32,
    /// Y offset relative to the found item's top-left corner.
    pub relative_y: i32,
}

impl Default for FoundItem {
    fn default() -> Self {
        Self {
            index: -1,
            exact: false,
            relative_x: 0,
            relative_y: 0,
        }
    }
}

/// Interface for an item placed inside [`MosaicLayout`], mirroring `AbstractLayoutItem`.
pub trait MosaicLayoutItem {
    /// Desired natural maximum width of the item (`maxWidth()`).
    fn max_width(&self) -> i32;
    /// Current laid-out width (`width()`).
    fn width(&self) -> i32;
    /// Current laid-out height (`height()`).
    fn height(&self) -> i32;
    /// Current encoded position index (`position()`), or `-1` if unplaced.
    fn position(&self) -> i32;
    /// Updates the item's encoded position index (`setPosition()`).
    fn set_position(&mut self, position: i32);
    /// Resizes the item to `new_width` and returns its resulting height (`resizeGetHeight()`).
    fn resize_get_height(&mut self, new_width: i32) -> i32;
}

/// Simple fixed-height item for [`MosaicLayout`] (suitable for GIFs, photos, thumbnails,
/// stickers, or tests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleMosaicItem {
    /// Caller-assigned identifier (e.g. index into results list).
    pub id: usize,
    /// Natural maximum width (`maxWidth()`).
    pub max_width: i32,
    /// Fixed height returned by `resize_get_height` (`_minh`).
    pub min_height: i32,
    /// Laid-out width set by `resize_get_height`.
    pub width: i32,
    /// Laid-out height set by `resize_get_height`.
    pub height: i32,
    /// Encoded mosaic position (`position_to_index(row, col)`), or `-1`.
    pub position: i32,
}

impl SimpleMosaicItem {
    /// Creates a new unplaced [`SimpleMosaicItem`].
    pub const fn new(id: usize, max_width: i32, min_height: i32) -> Self {
        Self {
            id,
            max_width,
            min_height,
            width: 0,
            height: 0,
            position: -1,
        }
    }
}

impl MosaicLayoutItem for SimpleMosaicItem {
    fn max_width(&self) -> i32 {
        self.max_width
    }

    fn width(&self) -> i32 {
        self.width
    }

    fn height(&self) -> i32 {
        self.height
    }

    fn position(&self) -> i32 {
        self.position
    }

    fn set_position(&mut self, position: i32) {
        self.position = position;
    }

    fn resize_get_height(&mut self, new_width: i32) -> i32 {
        self.width = new_width;
        self.height = self.min_height;
        self.height
    }
}

/// A finalized row inside [`MosaicLayout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MosaicRow<T> {
    /// Total natural width used to proportionally distribute row width, or `0` if row is
    /// not full/big (items keep their natural `max_width`).
    pub max_width: i32,
    /// Computed row height in pixels.
    pub height: i32,
    /// Items in this row.
    pub items: Vec<T>,
}

impl<T> Default for MosaicRow<T> {
    fn default() -> Self {
        Self {
            max_width: 0,
            height: 0,
            items: Vec::with_capacity(INLINE_ITEMS_MAX_PER_ROW),
        }
    }
}

/// Exact port of `Mosaic::Layout::AbstractMosaicLayout` (`layout/layout_mosaic.cpp`).
#[derive(Debug, Clone)]
pub struct MosaicLayout<T: MosaicLayoutItem> {
    big_width: i32,
    width: i32,
    right_skip: i32,
    min_item_width: i32,
    padding: Margins,
    rtl: bool,
    rows: Vec<MosaicRow<T>>,
}

impl<T: MosaicLayoutItem> MosaicLayout<T> {
    /// Creates a new [`MosaicLayout`] with the given `big_width` threshold.
    ///
    /// In tdesktop's `InlineBots::Layout::Inner`, `big_width` is
    /// `emojiPanWidth - emojiScroll.width - inlineResultsLeft` (`345 - 14 - 11 = 320`).
    pub fn new(big_width: i32) -> Self {
        Self {
            big_width,
            width: 0,
            right_skip: 0,
            min_item_width: DEFAULT_INLINE_RESULTS_MIN_WIDTH,
            padding: Margins::default(),
            rtl: false,
            rows: Vec::new(),
        }
    }

    /// Sets the horizontal gap inserted between items in the same row (`setRightSkip`).
    pub fn set_right_skip(&mut self, right_skip: i32) {
        self.right_skip = right_skip;
    }

    /// Sets outer padding (`setPadding`).
    pub fn set_padding(&mut self, padding: Margins) {
        self.padding = padding;
    }

    /// Sets the total widget width (`setFullWidth`).
    pub fn set_full_width(&mut self, width: i32) {
        self.width = width;
    }

    /// Sets the minimum laid-out width per item (default `48` = `st::inlineResultsMinWidth`).
    pub fn set_min_item_width(&mut self, min_width: i32) {
        self.min_item_width = min_width;
    }

    /// Sets right-to-left mode.
    pub fn set_rtl(&mut self, rtl: bool) {
        self.rtl = rtl;
    }

    /// Returns whether right-to-left mode is enabled.
    pub const fn is_rtl(&self) -> bool {
        self.rtl
    }

    /// Returns `true` if the layout contains no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns the number of rows.
    pub fn rows_count(&self) -> usize {
        self.rows.len()
    }

    /// Returns all rows.
    pub fn rows(&self) -> &[MosaicRow<T>] {
        &self.rows
    }

    /// Returns the height of `row`, if in bounds.
    pub fn row_height_at(&self, row: usize) -> Option<i32> {
        self.rows.get(row).map(|r| r.height)
    }

    /// Returns the item at `(row, column)`, if in bounds.
    pub fn maybe_item_at(&self, row: i32, column: i32) -> Option<&T> {
        if row < 0 || column < 0 {
            return None;
        }
        self.rows
            .get(row as usize)
            .and_then(|r| r.items.get(column as usize))
    }

    /// Returns the item at encoded flat index (`position_to_index`), if in bounds.
    pub fn maybe_item_by_index(&self, index: i32) -> Option<&T> {
        let pos = index_to_position(index);
        self.maybe_item_at(pos.row, pos.column)
    }

    /// Clears all rows, resetting item positions to `-1` when `!results_deleted`.
    pub fn clear_rows(&mut self, results_deleted: bool) {
        if !results_deleted {
            for row in &mut self.rows {
                for item in &mut row.items {
                    item.set_position(-1);
                }
            }
        }
        self.rows.clear();
    }

    /// Appends a batch of items, finalizing rows whenever `row.items.len() >= 5`
    /// or `sum_width >= big_width`, and force-finalizing the trailing row at the end.
    ///
    /// Port of `AbstractMosaicLayout::addItems` (`layout/layout_mosaic.h:80-91`,
    /// `layout/layout_mosaic.cpp:301-339`).
    pub fn add_items(&mut self, items: Vec<T>) {
        if items.is_empty() {
            return;
        }
        self.rows.reserve(items.len());
        let mut row = MosaicRow::default();
        let mut sum_width = 0_i32;
        for item in items {
            self.add_item(item, &mut row, &mut sum_width);
        }
        self.row_finalize(&mut row, &mut sum_width, true);
    }

    fn add_item(&mut self, mut item: T, row: &mut MosaicRow<T>, sum_width: &mut i32) {
        item.set_position(position_to_index(
            self.rows.len() as i32,
            row.items.len() as i32,
        ));
        if self.row_finalize(row, sum_width, false) {
            item.set_position(position_to_index(self.rows.len() as i32, 0));
        }

        *sum_width += item.max_width();
        if !row.items.is_empty() && self.right_skip != 0 {
            *sum_width += self.right_skip;
        }

        row.items.push(item);
    }

    fn row_finalize(&mut self, row: &mut MosaicRow<T>, sum_width: &mut i32, force: bool) -> bool {
        if row.items.is_empty() {
            return false;
        }
        let full = row.items.len() >= INLINE_ITEMS_MAX_PER_ROW;
        let big = *sum_width >= self.big_width;
        if full || big || force {
            row.max_width = if full || big { *sum_width } else { 0 };
            let full_width = self.width;
            Self::layout_row_with(
                row,
                full_width,
                self.padding,
                self.right_skip,
                self.min_item_width,
            );
            let finished = std::mem::take(row);
            self.rows.push(finished);
            *sum_width = 0;
            return true;
        }
        false
    }

    /// Re-layouts every row for `new_width` (or `self.width` when `new_width == 0`)
    /// and returns total height including top and bottom padding.
    ///
    /// Port of `AbstractMosaicLayout::countDesiredHeight` (`layout/layout_mosaic.cpp:24-31`).
    pub fn count_desired_height(&mut self, new_width: i32) -> i32 {
        let target_width = if new_width != 0 {
            new_width
        } else {
            self.width
        };
        let mut result = 0_i32;
        for row in &mut self.rows {
            Self::layout_row_with(
                row,
                target_width,
                self.padding,
                self.right_skip,
                self.min_item_width,
            );
            result += row.height;
        }
        self.padding.top + result + self.padding.bottom
    }

    /// Exact port of `AbstractMosaicLayout::layoutRow` (`layout/layout_mosaic.cpp:341-377`).
    ///
    /// Sorts item indices by ascending `max_width()` so narrower items get clamped to
    /// `min_item_width` first and the remaining width is proportionally redistributed
    /// among wider items in exact integer arithmetic.
    fn layout_row_with(
        row: &mut MosaicRow<T>,
        full_width: i32,
        padding: Margins,
        right_skip: i32,
        min_item_width: i32,
    ) {
        let count = row.items.len();
        debug_assert!(count <= INLINE_ITEMS_MAX_PER_ROW);
        if count == 0 {
            row.height = 0;
            return;
        }

        let mut indices = [0usize; INLINE_ITEMS_MAX_PER_ROW];
        for (i, slot) in indices.iter_mut().take(count).enumerate() {
            *slot = i;
        }
        indices[..count].sort_by_key(|&idx| row.items[idx].max_width());

        let mut desired_width = row.max_width;
        row.height = 0;
        let mut available_width = full_width - padding.left - padding.right;

        for &index in &indices[..count] {
            let item_max_w = row.items[index].max_width();
            let w = if desired_width != 0 {
                (item_max_w * available_width) / desired_width
            } else {
                item_max_w
            };
            let actual_width = w.max(min_item_width);
            let h = row.items[index].resize_get_height(actual_width);
            row.height = row.height.max(h);

            if desired_width != 0 {
                available_width -= actual_width;
                desired_width -= item_max_w;
                if index > 0 && right_skip != 0 {
                    available_width -= right_skip;
                    desired_width -= right_skip;
                }
            }
        }
    }

    /// Hit-tests a point against the mosaic rows.
    ///
    /// Port of `AbstractMosaicLayout::findByPoint` (`layout/layout_mosaic.cpp:33-83`).
    pub fn find_by_point(&self, px: i32, py: i32) -> FoundItem {
        let mut sx = px - self.padding.left;
        let mut sy = py - self.padding.top;
        let rows_count = self.rows.len() as i32;
        let mut row: i32;
        let mut exact = true;

        if sy >= 0 {
            row = 0;
            while row < rows_count {
                let row_height = self.rows[row as usize].height;
                if sy < row_height {
                    break;
                }
                sy -= row_height;
                row += 1;
            }
        } else {
            row = 0;
            exact = false;
        }
        if row >= rows_count {
            row = rows_count - 1;
            exact = false;
        }
        if sx < 0 {
            sx = 0;
            exact = false;
        }

        let sel = if sx >= 0 && row >= 0 && row < rows_count {
            let cols_count = self.rows[row as usize].items.len() as i32;
            let mut col = 0;
            while col < cols_count {
                let width = self.rows[row as usize].items[col as usize].width();
                if sx < width {
                    break;
                }
                sx -= width;
                sx -= self.right_skip;
                col += 1;
            }
            if col >= cols_count {
                col = cols_count - 1;
                exact = false;
            }
            position_to_index(row, col)
        } else {
            -1
        };

        FoundItem {
            index: sel,
            exact,
            relative_x: sx,
            relative_y: sy,
        }
    }

    /// Finds the screen rectangle of the item at encoded `index`.
    ///
    /// Port of `AbstractMosaicLayout::findRect` (`layout/layout_mosaic.cpp:85-124`).
    pub fn find_rect(&self, index: i32) -> Option<Rect> {
        let from_x = 0_i32;
        let to_x = self.width;
        let mut top = 0_i32;
        for row in &self.rows {
            let mut left = 0_i32;
            for item in &row.items {
                if left >= to_x {
                    break;
                }
                let w = item.width();
                if left + w > from_x && item.position() == index {
                    return Some(Rect::new(
                        left + self.padding.left,
                        top + self.padding.top,
                        item.width(),
                        item.height(),
                    ));
                }
                left += w;
                left += self.right_skip;
            }
            top += row.height;
        }
        None
    }

    /// Returns all item rectangles in paint order `(encoded_position, Rect)`.
    pub fn item_rects(&self) -> Vec<(i32, Rect)> {
        let mut out = Vec::new();
        let mut top = self.padding.top;
        for row in &self.rows {
            let mut left = self.padding.left;
            for item in &row.items {
                let w = item.width();
                let h = item.height();
                out.push((item.position(), Rect::new(left, top, w, h)));
                left += w + self.right_skip;
            }
            top += row.height;
        }
        out
    }

    /// Validates existing rows against an updated list of length `count`, returning the
    /// number of prefix items still valid (so only `until..count` need to be laid out).
    ///
    /// `is_mismatch(item, flat_index)` returns `true` when the item does NOT match
    /// (matching `checkItem` in `layout/layout_mosaic.cpp:237-299`).
    pub fn validate_existing_rows<F>(&mut self, mut is_mismatch: F, count: usize) -> usize
    where
        F: FnMut(&T, usize) -> bool,
    {
        let mut until = 0usize;
        let mut until_row = 0usize;
        let mut until_col = 0usize;
        while until < count {
            if until_row >= self.rows.len()
                || is_mismatch(&self.rows[until_row].items[until_col], until)
            {
                break;
            }
            until += 1;
            until_col += 1;
            if until_col == self.rows[until_row].items.len() {
                until_row += 1;
                until_col = 0;
            }
        }

        if until == count {
            if until_row == self.rows.len() {
                return until;
            }
            let mut skip = until_col;
            for row in &mut self.rows[until_row..] {
                for item in &mut row.items {
                    if skip > 0 {
                        skip -= 1;
                    } else {
                        item.set_position(-1);
                    }
                }
            }
            if until_col == 0 {
                self.rows.truncate(until_row);
                return until;
            }
            self.rows.truncate(until_row + 1);
            self.rows[until_row].items.truncate(until_col);
            self.rows[until_row].max_width = self.rows[until_row]
                .items
                .iter()
                .map(MosaicLayoutItem::max_width)
                .sum();
            let full_width = self.width;
            Self::layout_row_with(
                &mut self.rows[until_row],
                full_width,
                self.padding,
                self.right_skip,
                self.min_item_width,
            );
            return until;
        }

        if until_row > 0 && until_col == 0 {
            // Remove last row, maybe it is not full.
            until_row -= 1;
            until_col = self.rows[until_row].items.len();
        }
        until -= until_col;

        for row in &mut self.rows[until_row..] {
            for item in &mut row.items {
                item.set_position(-1);
            }
        }
        self.rows.truncate(until_row);

        until
    }
}
