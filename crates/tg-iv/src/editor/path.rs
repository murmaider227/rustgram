//! Port of `iv/editor/iv_editor_page_path.h` and `iv/editor/iv_editor_page_path.cpp`,
//! plus structural selection ranges from `iv/markdown/iv_markdown_prepare.h`.
//!
//! Defines deterministic structural paths to blocks, nested containers (quotes, details,
//! list item children), editable leaves (text, captions, list items, table cells, display
//! math), and structural selection ranges.

use serde::{Deserialize, Serialize};

use crate::rich_page::{RichBlock, RichBlockKind, RichListItem, RichPage, RichTableCell};

/// Kind of block container step (`Iv::Editor::BlockContainerKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BlockContainerKind {
    /// Top-level `RichPage::blocks`.
    #[default]
    Root,
    /// Nested `RichBlock::blocks` (Quote / Details).
    BlockChildren,
    /// Nested `RichListItem::blocks` inside a List block.
    ListItemChildren,
}

/// One traversal step into a nested container (`Iv::Editor::BlockContainerStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockContainerStep {
    /// Step container kind.
    pub kind: BlockContainerKind,
    /// Index of the owning block in its parent container.
    pub block_index: usize,
    /// Index of the list item (only used when `kind == ListItemChildren`).
    pub list_item_index: Option<usize>,
}

/// Path from `RichPage::blocks` to any block vector (`Iv::Editor::BlockContainerPath`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct BlockContainerPath {
    /// Sequence of container descent steps (empty means root `RichPage::blocks`).
    pub steps: Vec<BlockContainerStep>,
}

impl BlockContainerPath {
    /// Root container (`RichPage::blocks`).
    #[must_use]
    pub fn root() -> Self {
        Self { steps: Vec::new() }
    }

    /// Returns true if this points to the top-level `RichPage::blocks`.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Path to a single [`RichBlock`] within a [`RichPage`] (`Iv::Editor::BlockPath`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct BlockPath {
    /// Owning block container.
    pub container: BlockContainerPath,
    /// Index inside `container`.
    pub index: usize,
}

impl BlockPath {
    /// Creates a top-level block path.
    #[must_use]
    pub fn root(index: usize) -> Self {
        Self {
            container: BlockContainerPath::root(),
            index,
        }
    }

    /// Returns the container path for this block's child `blocks` (`BlockChildrenContainer`).
    #[must_use]
    pub fn children_container(&self) -> BlockContainerPath {
        let mut container = self.container.clone();
        container.steps.push(BlockContainerStep {
            kind: BlockContainerKind::BlockChildren,
            block_index: self.index,
            list_item_index: None,
        });
        container
    }

    /// Returns the container path for a list item's child `blocks` (`ListItemChildrenContainer`).
    #[must_use]
    pub fn list_item_children_container(&self, item_index: usize) -> BlockContainerPath {
        let mut container = self.container.clone();
        container.steps.push(BlockContainerStep {
            kind: BlockContainerKind::ListItemChildren,
            block_index: self.index,
            list_item_index: Some(item_index),
        });
        container
    }
}

/// Target for replacing a block or grouped-media item (`Iv::Editor::ReplaceTarget`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceTarget {
    /// Path to the target block.
    pub path: BlockPath,
    /// Block kind being replaced.
    pub kind: RichBlockKind,
    /// Media photo/document ID.
    pub media_id: u64,
    /// Item index inside `GroupedMedia`, or `None` for standalone blocks.
    pub item_index: Option<usize>,
}

/// Kind of editable leaf inside a [`RichBlock`] (`Iv::Editor::LeafKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LeafKind {
    /// Primary `RichBlock::text` (Paragraph, Heading, Footer, Code, Quote, Table title, Details summary).
    #[default]
    BlockText,
    /// Secondary `RichBlock::caption` (Quote, Photo, Video, Audio, File, Map, GroupedMedia).
    BlockCaption,
    /// Inline `RichListItem::text`.
    ListItemText,
    /// `RichTableCell::text`.
    TableCellText,
    /// Raw TeX `RichBlock::formula`.
    MathFormula,
}

/// Deterministic address of an editable text/formula leaf (`Iv::Editor::LeafPath`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LeafPath {
    /// Leaf kind.
    pub kind: LeafKind,
    /// Owning block path.
    pub block: BlockPath,
    /// List item index (when `kind == ListItemText`).
    pub list_item_index: Option<usize>,
    /// Table row index (when `kind == TableCellText`).
    pub table_row_index: Option<usize>,
    /// Table cell index (when `kind == TableCellText`).
    pub table_cell_index: Option<usize>,
}

impl LeafPath {
    /// Creates a `BlockText` leaf path.
    #[must_use]
    pub fn block_text(block: BlockPath) -> Self {
        Self {
            kind: LeafKind::BlockText,
            block,
            list_item_index: None,
            table_row_index: None,
            table_cell_index: None,
        }
    }

    /// Creates a `BlockCaption` leaf path.
    #[must_use]
    pub fn block_caption(block: BlockPath) -> Self {
        Self {
            kind: LeafKind::BlockCaption,
            block,
            list_item_index: None,
            table_row_index: None,
            table_cell_index: None,
        }
    }

    /// Creates a `ListItemText` leaf path.
    #[must_use]
    pub fn list_item_text(block: BlockPath, list_item_index: usize) -> Self {
        Self {
            kind: LeafKind::ListItemText,
            block,
            list_item_index: Some(list_item_index),
            table_row_index: None,
            table_cell_index: None,
        }
    }

    /// Creates a `TableCellText` leaf path.
    #[must_use]
    pub fn table_cell_text(
        block: BlockPath,
        table_row_index: usize,
        table_cell_index: usize,
    ) -> Self {
        Self {
            kind: LeafKind::TableCellText,
            block,
            list_item_index: None,
            table_row_index: Some(table_row_index),
            table_cell_index: Some(table_cell_index),
        }
    }

    /// Creates a `MathFormula` leaf path.
    #[must_use]
    pub fn math_formula(block: BlockPath) -> Self {
        Self {
            kind: LeafKind::MathFormula,
            block,
            list_item_index: None,
            table_row_index: None,
            table_cell_index: None,
        }
    }
}

/// Contiguous range `[from, till)` of blocks in a container (`PreparedEditBlockRange`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditBlockRange {
    /// Owning container.
    pub container: BlockContainerPath,
    /// Inclusive start block index.
    pub from: usize,
    /// Exclusive end block index.
    pub till: usize,
}

impl EditBlockRange {
    /// Returns true when the range selects no blocks.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.till <= self.from
    }
}

/// Contiguous range `[from, till)` of list items in a list block (`PreparedEditListItemRange`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditListItemRange {
    /// Owning list block path.
    pub block: BlockPath,
    /// Inclusive start item index.
    pub from: usize,
    /// Exclusive end item index.
    pub till: usize,
}

impl EditListItemRange {
    /// Returns true when no items are selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.till <= self.from
    }
}

/// Contiguous range `[from, till)` of table rows (`PreparedEditTableRowRange`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditTableRowRange {
    /// Owning table block path.
    pub block: BlockPath,
    /// Inclusive start row index.
    pub from: usize,
    /// Exclusive end row index.
    pub till: usize,
}

impl EditTableRowRange {
    /// Returns true when no rows are selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.till <= self.from
    }
}

/// Rectangular cell range `[row_from..row_till) x [column_from..column_till)` in a table
/// (`PreparedEditTableCellRange`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditTableCellRange {
    /// Owning table block path.
    pub block: BlockPath,
    /// Inclusive start row.
    pub row_from: usize,
    /// Exclusive end row.
    pub row_till: usize,
    /// Inclusive start column.
    pub column_from: usize,
    /// Exclusive end column.
    pub column_till: usize,
}

impl EditTableCellRange {
    /// Returns true when the rectangular region is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.row_till <= self.row_from || self.column_till <= self.column_from
    }
}

/// Structural selection over blocks, list items, table rows, or table cells (`PreparedEditSelection`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EditSelection {
    /// No structural selection active.
    #[default]
    None,
    /// Selected range of sibling blocks.
    Blocks(EditBlockRange),
    /// Selected range of list items in one list.
    ListItems(EditListItemRange),
    /// Selected range of table rows.
    TableRows(EditTableRowRange),
    /// Selected rectangular cell region in a table.
    TableCells(EditTableCellRange),
}

impl EditSelection {
    /// Returns true if no structural items are selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        match self {
            Self::None => true,
            Self::Blocks(r) => r.is_empty(),
            Self::ListItems(r) => r.is_empty(),
            Self::TableRows(r) => r.is_empty(),
            Self::TableCells(r) => r.is_empty(),
        }
    }
}

// ---- Path resolution helpers on `RichPage` ----

/// Resolves a [`BlockContainerPath`] to an immutable slice of [`RichBlock`]s.
#[must_use]
pub fn resolve_container<'a>(
    page: &'a RichPage,
    path: &BlockContainerPath,
) -> Option<&'a [RichBlock]> {
    let mut current: &[RichBlock] = &page.blocks;
    for step in &path.steps {
        let block = current.get(step.block_index)?;
        match step.kind {
            BlockContainerKind::Root => return None,
            BlockContainerKind::BlockChildren => {
                current = &block.blocks;
            }
            BlockContainerKind::ListItemChildren => {
                let item_idx = step.list_item_index?;
                let item = block.list_items.get(item_idx)?;
                current = &item.blocks;
            }
        }
    }
    Some(current)
}

/// Resolves a [`BlockContainerPath`] to a mutable `Vec<RichBlock>`.
pub fn resolve_container_mut<'a>(
    page: &'a mut RichPage,
    path: &BlockContainerPath,
) -> Option<&'a mut Vec<RichBlock>> {
    let mut current: &mut Vec<RichBlock> = &mut page.blocks;
    for step in &path.steps {
        let block = current.get_mut(step.block_index)?;
        match step.kind {
            BlockContainerKind::Root => return None,
            BlockContainerKind::BlockChildren => {
                current = &mut block.blocks;
            }
            BlockContainerKind::ListItemChildren => {
                let item_idx = step.list_item_index?;
                let item = block.list_items.get_mut(item_idx)?;
                current = &mut item.blocks;
            }
        }
    }
    Some(current)
}

/// Resolves a [`BlockPath`] to an immutable [`RichBlock`].
#[must_use]
pub fn resolve_block<'a>(page: &'a RichPage, path: &BlockPath) -> Option<&'a RichBlock> {
    resolve_container(page, &path.container)?.get(path.index)
}

/// Resolves a [`BlockPath`] to a mutable [`RichBlock`].
pub fn resolve_block_mut<'a>(
    page: &'a mut RichPage,
    path: &BlockPath,
) -> Option<&'a mut RichBlock> {
    resolve_container_mut(page, &path.container)?.get_mut(path.index)
}

/// Resolves a list item by owning block path and index.
#[must_use]
pub fn resolve_list_item<'a>(
    page: &'a RichPage,
    block: &BlockPath,
    item_index: usize,
) -> Option<&'a RichListItem> {
    resolve_block(page, block)?.list_items.get(item_index)
}

/// Resolves a mutable list item by owning block path and index.
pub fn resolve_list_item_mut<'a>(
    page: &'a mut RichPage,
    block: &BlockPath,
    item_index: usize,
) -> Option<&'a mut RichListItem> {
    resolve_block_mut(page, block)?
        .list_items
        .get_mut(item_index)
}

/// Resolves a table cell by owning block path, row index, and cell index.
#[must_use]
pub fn resolve_table_cell<'a>(
    page: &'a RichPage,
    block: &BlockPath,
    row_index: usize,
    cell_index: usize,
) -> Option<&'a RichTableCell> {
    resolve_block(page, block)?
        .table_rows
        .get(row_index)?
        .cells
        .get(cell_index)
}

/// Resolves a mutable table cell by owning block path, row index, and cell index.
pub fn resolve_table_cell_mut<'a>(
    page: &'a mut RichPage,
    block: &BlockPath,
    row_index: usize,
    cell_index: usize,
) -> Option<&'a mut RichTableCell> {
    resolve_block_mut(page, block)?
        .table_rows
        .get_mut(row_index)?
        .cells
        .get_mut(cell_index)
}
