//! Port of `iv/editor/iv_editor_page_table_grid.h`, `iv/editor/iv_editor_page_table_grid.cpp`,
//! `iv/editor/iv_editor_page_table_remove.h`, and `iv/editor/iv_editor_page_table_remove.cpp`.
//!
//! Computes 2D occupancy grids for tables with arbitrary `colspan`/`rowspan`, supports
//! span-preserving row/column insertion & deletion, cell uniting/splitting, and table
//! selection inspection.

use serde::{Deserialize, Serialize};

use crate::editor::path::EditTableCellRange;
use crate::markdown::document::MarkdownPrepareTableRenderLimits;
use crate::rich_page::{
    RichBlock, RichBlockKind, RichTableAlignment, RichTableCell, RichTableRow,
    RichTableVerticalAlignment,
};

/// Reference to a placed cell in a [`TableGrid`] (`Iv::Editor::TableGridCellReference`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableGridCellRef {
    /// Source row index in `RichBlock::table_rows`.
    pub row_index: usize,
    /// Source cell index in `RichTableRow::cells`.
    pub cell_index: usize,
    /// Inclusive top grid row.
    pub row_from: usize,
    /// Exclusive bottom grid row.
    pub row_till: usize,
    /// Inclusive left grid column.
    pub column_from: usize,
    /// Exclusive right grid column.
    pub column_till: usize,
}

/// 2D table occupancy grid (`Iv::Editor::TableGrid`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableGrid {
    /// Placed cells sorted in `(row_index, cell_index)` order.
    pub cells: Vec<TableGridCellRef>,
    /// Per-row slot occupancy matrix.
    pub occupancy: Vec<Vec<bool>>,
    /// Total rows in grid.
    pub row_count: usize,
    /// Total columns in grid.
    pub column_count: usize,
}

/// Summary of the currently selected table cell range (`Iv::Editor::State::TableSelectionInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TableSelectionInfo {
    /// True if the selection targets a valid table block.
    pub valid: bool,
    /// True if all selected cells have `header == true`.
    pub all_header: bool,
    /// True if all selected cells are left-aligned.
    pub all_align_left: bool,
    /// True if all selected cells are center-aligned.
    pub all_align_center: bool,
    /// True if all selected cells are right-aligned.
    pub all_align_right: bool,
    /// True if all selected cells are top-aligned.
    pub all_align_top: bool,
    /// True if all selected cells are middle-aligned.
    pub all_align_middle: bool,
    /// True if all selected cells are bottom-aligned.
    pub all_align_bottom: bool,
    /// True if exactly one cell is selected.
    pub single_cell: bool,
    /// True if the selected cell has `colspan > 1 || rowspan > 1`.
    pub can_split_cell: bool,
    /// True if multiple cells form a clean uniteable rectangle.
    pub can_unite_cells: bool,
    /// Number of selected rows.
    pub selected_rows: usize,
    /// Number of selected columns.
    pub selected_columns: usize,
    /// Total rows in table.
    pub total_rows: usize,
    /// Total columns in table.
    pub total_columns: usize,
    /// Table bordered flag.
    pub bordered: bool,
    /// Table striped flag.
    pub striped: bool,
    /// Table compact flag.
    pub compact: bool,
}

/// Normalizes any span `<= 0` to `1` (`NormalizeTableSpan`).
#[must_use]
pub const fn normalize_table_span(span: i32) -> usize {
    if span < 1 { 1 } else { span as usize }
}

/// Clamps `raw_rowspan` to the remaining rows in the table (`ClampTableRowspan`).
#[must_use]
pub fn clamp_table_rowspan(raw_rowspan: i32, row: usize, row_count: usize) -> usize {
    if row >= row_count || row_count == 0 {
        return 0;
    }
    normalize_table_span(raw_rowspan).min(row_count - row)
}

/// Clamps `raw_colspan` to the remaining columns up to `max_columns` (`ClampTableColspan`).
#[must_use]
pub fn clamp_table_colspan(raw_colspan: i32, column: usize, max_columns: usize) -> usize {
    if column >= max_columns || max_columns == 0 {
        return 0;
    }
    normalize_table_span(raw_colspan).min(max_columns - column)
}

/// Returns true if `[row..row+rowspan) x [column..column+colspan)` is unoccupied (`CanOccupyTableSlots`).
#[must_use]
pub fn can_occupy_table_slots(
    occupancy: &[Vec<bool>],
    row: usize,
    column: usize,
    rowspan: usize,
    colspan: usize,
) -> bool {
    if rowspan == 0 || colspan == 0 || row >= occupancy.len() {
        return false;
    }
    let row_limit = row.saturating_add(rowspan).min(occupancy.len());
    let col_limit = column.saturating_add(colspan);
    for occ_row in &occupancy[row..row_limit] {
        let check_limit = col_limit.min(occ_row.len());
        if occ_row[column.min(check_limit)..check_limit]
            .iter()
            .any(|&used| used)
        {
            return false;
        }
    }
    true
}

/// Finds the leftmost available column in `row` fitting `rowspan x colspan` (`FirstAvailableTableColumn`).
#[must_use]
pub fn first_available_table_column(
    occupancy: &[Vec<bool>],
    row: usize,
    rowspan: usize,
    colspan: usize,
    max_columns: usize,
) -> Option<usize> {
    if row >= occupancy.len() || rowspan == 0 || colspan == 0 || max_columns == 0 {
        return None;
    }
    for col in 0..max_columns {
        let eff_colspan = colspan.min(max_columns - col);
        if eff_colspan > 0 && can_occupy_table_slots(occupancy, row, col, rowspan, eff_colspan) {
            return Some(col);
        }
    }
    None
}

/// Marks `[row..row+rowspan) x [column..column+colspan)` as occupied (`MarkTableSlots`).
pub fn mark_table_slots(
    occupancy: &mut [Vec<bool>],
    row: usize,
    column: usize,
    rowspan: usize,
    colspan: usize,
) {
    if rowspan == 0 || colspan == 0 || row >= occupancy.len() {
        return;
    }
    let row_limit = row.saturating_add(rowspan).min(occupancy.len());
    let col_limit = column.saturating_add(colspan);
    for occ_row in &mut occupancy[row..row_limit] {
        if col_limit > occ_row.len() {
            occ_row.resize(col_limit, false);
        }
        for slot in &mut occ_row[column..col_limit] {
            *slot = true;
        }
    }
}

/// Computes the widest row span sum in `table` (`TableMaxColumns`).
#[must_use]
pub fn table_max_columns(table: &RichBlock) -> usize {
    table
        .table_rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|c| normalize_table_span(c.colspan))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
}

/// Builds an unbounded [`TableGrid`] for `table` (`BuildTableGrid(table)`).
#[must_use]
pub fn build_table_grid(table: &RichBlock) -> TableGrid {
    let row_count = table.table_rows.len();
    let max_columns = table_max_columns(table);
    let mut grid = TableGrid {
        cells: Vec::new(),
        occupancy: vec![Vec::new(); row_count],
        row_count,
        column_count: 0,
    };
    if row_count == 0 || max_columns == 0 {
        return grid;
    }
    for (row_idx, row) in table.table_rows.iter().enumerate() {
        for (cell_idx, cell) in row.cells.iter().enumerate() {
            let norm_colspan = normalize_table_span(cell.colspan);
            let rowspan = clamp_table_rowspan(cell.rowspan, row_idx, row_count);
            if rowspan == 0 {
                continue;
            }
            let Some(col) = first_available_table_column(
                &grid.occupancy,
                row_idx,
                rowspan,
                norm_colspan,
                max_columns,
            ) else {
                continue;
            };
            let colspan = clamp_table_colspan(cell.colspan, col, max_columns);
            if colspan == 0 {
                continue;
            }
            grid.cells.push(TableGridCellRef {
                row_index: row_idx,
                cell_index: cell_idx,
                row_from: row_idx,
                row_till: row_idx + rowspan,
                column_from: col,
                column_till: col + colspan,
            });
            mark_table_slots(&mut grid.occupancy, row_idx, col, rowspan, colspan);
        }
    }
    grid.column_count = grid.occupancy.iter().map(Vec::len).max().unwrap_or(0);
    grid
}

/// Builds a render-limited [`TableGrid`] respecting `limits` (`BuildTableGrid(table, limits)`).
#[must_use]
pub fn build_table_grid_limited(
    table: &RichBlock,
    limits: MarkdownPrepareTableRenderLimits,
) -> TableGrid {
    let row_count = table.table_rows.len().min(limits.max_rows);
    let mut grid = TableGrid {
        cells: Vec::new(),
        occupancy: vec![Vec::new(); row_count],
        row_count,
        column_count: 0,
    };
    let mut occupied_slots = 0usize;
    for (row_idx, row) in table.table_rows.iter().enumerate().take(row_count) {
        for (cell_idx, cell) in row.cells.iter().enumerate() {
            let norm_colspan = normalize_table_span(cell.colspan);
            let rowspan = clamp_table_rowspan(cell.rowspan, row_idx, row_count);
            if rowspan == 0 {
                continue;
            }
            let Some(col) = first_available_table_column(
                &grid.occupancy,
                row_idx,
                rowspan,
                norm_colspan,
                limits.max_columns,
            ) else {
                continue;
            };
            let colspan = clamp_table_colspan(cell.colspan, col, limits.max_columns);
            if colspan == 0 {
                continue;
            }
            let growth = rowspan.saturating_mul(colspan);
            if growth > limits.max_cells || occupied_slots.saturating_add(growth) > limits.max_cells
            {
                continue;
            }
            grid.cells.push(TableGridCellRef {
                row_index: row_idx,
                cell_index: cell_idx,
                row_from: row_idx,
                row_till: row_idx + rowspan,
                column_from: col,
                column_till: col + colspan,
            });
            mark_table_slots(&mut grid.occupancy, row_idx, col, rowspan, colspan);
            occupied_slots += growth;
        }
    }
    grid.column_count = grid.occupancy.iter().map(Vec::len).max().unwrap_or(0);
    grid
}

/// Returns true if `cell` intersects `range`.
#[must_use]
pub const fn cell_intersects_range(cell: &TableGridCellRef, range: &EditTableCellRange) -> bool {
    cell.row_from < range.row_till
        && cell.row_till > range.row_from
        && cell.column_from < range.column_till
        && cell.column_till > range.column_from
}

/// Returns all cells intersecting `range` (`SelectedTableGridCells`).
#[must_use]
pub fn selected_table_grid_cells(
    grid: &TableGrid,
    range: &EditTableCellRange,
) -> Vec<TableGridCellRef> {
    grid.cells
        .iter()
        .copied()
        .filter(|c| cell_intersects_range(c, range))
        .collect()
}

/// Removes rows `[from, till)` while preserving and relocating cells whose `rowspan`
/// extends past `till` (`EraseTableRowsKeepingSpans` in `iv_editor_page_table_remove.cpp`).
pub fn erase_table_rows_keeping_spans(
    table: &mut RichBlock,
    limits: MarkdownPrepareTableRenderLimits,
    from: usize,
    till: usize,
) {
    let row_count = table.table_rows.len();
    if from >= till || till > row_count {
        return;
    }
    let grid = build_table_grid_limited(table, limits);
    let mut occupancy = vec![Vec::<bool>::new(); 1];
    let mut staged: Vec<(usize, usize, Option<TableGridCellRef>)> = Vec::new();

    for (row_idx, row) in table.table_rows.iter_mut().enumerate().take(till) {
        for (cell_idx, cell) in row.cells.iter_mut().enumerate() {
            let cell_till = row_idx + clamp_table_rowspan(cell.rowspan, row_idx, row_count);
            let covered = cell_till.min(till).saturating_sub(row_idx.max(from));
            if covered == 0 {
                continue;
            }
            let new_span = normalize_table_span(cell.rowspan)
                .saturating_sub(covered)
                .max(1);
            cell.rowspan = new_span as i32;
            if cell_till <= till {
                continue;
            }
            let reference = grid
                .cells
                .iter()
                .copied()
                .find(|c| c.row_index == row_idx && c.cell_index == cell_idx);
            if row_idx >= from {
                staged.push((row_idx, cell_idx, reference));
            } else if let Some(ref r) = reference {
                mark_table_slots(
                    &mut occupancy,
                    0,
                    r.column_from,
                    1,
                    r.column_till - r.column_from,
                );
            }
        }
    }

    if !staged.is_empty() && till < row_count {
        for cell_idx in 0..table.table_rows[till].cells.len() {
            let reference = grid
                .cells
                .iter()
                .copied()
                .find(|c| c.row_index == till && c.cell_index == cell_idx);
            staged.push((till, cell_idx, reference));
        }
        staged.sort_by_key(|(_, _, r)| r.map_or(usize::MAX, |x| x.column_from));
        let mut destination = Vec::with_capacity(staged.len());
        for (r_idx, c_idx, reference) in staged {
            let cell = std::mem::take(&mut table.table_rows[r_idx].cells[c_idx]);
            let colspan = normalize_table_span(cell.colspan);
            if let Some(r) = reference {
                while let Some(fit) =
                    first_available_table_column(&occupancy, 0, 1, colspan, limits.max_columns)
                {
                    if fit >= r.column_from {
                        break;
                    }
                    if let Some(slot) =
                        first_available_table_column(&occupancy, 0, 1, 1, limits.max_columns)
                    {
                        mark_table_slots(&mut occupancy, 0, slot, 1, 1);
                        destination.push(RichTableCell::default());
                    } else {
                        break;
                    }
                }
                mark_table_slots(
                    &mut occupancy,
                    0,
                    r.column_from,
                    1,
                    r.column_till - r.column_from,
                );
            }
            destination.push(cell);
        }
        table.table_rows[till].cells = destination;
    }

    table.table_rows.drain(from..till);
}

/// Computes [`TableSelectionInfo`] for `range` in `table`.
#[must_use]
pub fn inspect_table_selection(
    table: &RichBlock,
    range: &EditTableCellRange,
) -> TableSelectionInfo {
    if table.kind != RichBlockKind::Table || range.is_empty() {
        return TableSelectionInfo::default();
    }
    let grid = build_table_grid(table);
    let selected = selected_table_grid_cells(&grid, range);
    if selected.is_empty() {
        return TableSelectionInfo::default();
    }
    let mut info = TableSelectionInfo {
        valid: true,
        all_header: true,
        all_align_left: true,
        all_align_center: true,
        all_align_right: true,
        all_align_top: true,
        all_align_middle: true,
        all_align_bottom: true,
        single_cell: selected.len() == 1,
        can_split_cell: false,
        can_unite_cells: selected.len() > 1,
        selected_rows: range.row_till - range.row_from,
        selected_columns: range.column_till - range.column_from,
        total_rows: grid.row_count,
        total_columns: grid.column_count,
        bordered: table.bordered,
        striped: table.striped,
        compact: table.compact,
    };
    for cell_ref in &selected {
        let Some(cell) = table
            .table_rows
            .get(cell_ref.row_index)
            .and_then(|r| r.cells.get(cell_ref.cell_index))
        else {
            continue;
        };
        info.all_header &= cell.header;
        info.all_align_left &= cell.alignment == RichTableAlignment::Left;
        info.all_align_center &= cell.alignment == RichTableAlignment::Center;
        info.all_align_right &= cell.alignment == RichTableAlignment::Right;
        info.all_align_top &= cell.vertical_alignment == RichTableVerticalAlignment::Top;
        info.all_align_middle &= cell.vertical_alignment == RichTableVerticalAlignment::Middle;
        info.all_align_bottom &= cell.vertical_alignment == RichTableVerticalAlignment::Bottom;
        if info.single_cell {
            info.can_split_cell = cell.colspan > 1 || cell.rowspan > 1;
        }
    }
    info
}

/// Inserts a blank row before (`after == false`) or after (`after == true`) `range`.
pub fn add_table_row(table: &mut RichBlock, range: &EditTableCellRange, after: bool) -> bool {
    if table.kind != RichBlockKind::Table || range.is_empty() || table.table_rows.is_empty() {
        return false;
    }
    let grid = build_table_grid(table);
    let insert_at = if after {
        range.row_till.min(table.table_rows.len())
    } else {
        range.row_from.min(table.table_rows.len())
    };
    let cols = grid.column_count.max(1);
    let new_row = RichTableRow {
        cells: (0..cols).map(|_| RichTableCell::default()).collect(),
    };
    table.table_rows.insert(insert_at, new_row);
    true
}

/// Inserts a blank column before or after `range`.
pub fn add_table_column(table: &mut RichBlock, range: &EditTableCellRange, after: bool) -> bool {
    if table.kind != RichBlockKind::Table || range.is_empty() || table.table_rows.is_empty() {
        return false;
    }
    let grid = build_table_grid(table);
    let target_col = if after {
        range.column_till
    } else {
        range.column_from
    };
    for (row_idx, row) in table.table_rows.iter_mut().enumerate() {
        let mut insert_idx = row.cells.len();
        for c in &grid.cells {
            if c.row_index == row_idx && c.column_from >= target_col {
                insert_idx = c.cell_index;
                break;
            }
        }
        let header = row.cells.first().is_some_and(|c| c.header);
        row.cells.insert(
            insert_idx.min(row.cells.len()),
            RichTableCell {
                header,
                ..RichTableCell::default()
            },
        );
    }
    true
}

/// Unites the rectangular cell selection into the top-left cell, concatenating non-empty cell texts.
pub fn unite_table_cells(table: &mut RichBlock, range: &EditTableCellRange) -> bool {
    if table.kind != RichBlockKind::Table || range.is_empty() {
        return false;
    }
    let grid = build_table_grid(table);
    let selected = selected_table_grid_cells(&grid, range);
    if selected.len() <= 1 {
        return false;
    }
    let min_r = selected.iter().map(|c| c.row_from).min().unwrap_or(0);
    let max_r = selected.iter().map(|c| c.row_till).max().unwrap_or(0);
    let min_c = selected.iter().map(|c| c.column_from).min().unwrap_or(0);
    let max_c = selected.iter().map(|c| c.column_till).max().unwrap_or(0);
    let anchor = selected[0];

    let mut merged_text = table.table_rows[anchor.row_index].cells[anchor.cell_index]
        .text
        .clone();
    let mut to_remove: Vec<(usize, usize)> = Vec::new();
    for cell_ref in selected.iter().skip(1) {
        let cell = &table.table_rows[cell_ref.row_index].cells[cell_ref.cell_index];
        if !cell.text.text.is_empty() {
            if !merged_text.text.is_empty() {
                merged_text.text.push_str("\n");
            }
            merged_text.text.append(cell.text.text.clone());
        }
        to_remove.push((cell_ref.row_index, cell_ref.cell_index));
    }

    let target = &mut table.table_rows[anchor.row_index].cells[anchor.cell_index];
    target.text = merged_text;
    target.rowspan = (max_r - min_r).max(1) as i32;
    target.colspan = (max_c - min_c).max(1) as i32;

    // Remove absorbed cells in reverse index order per row
    to_remove.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (r_idx, c_idx) in to_remove {
        if let Some(row) = table.table_rows.get_mut(r_idx)
            && c_idx < row.cells.len()
        {
            row.cells.remove(c_idx);
        }
    }
    true
}

/// Splits a merged cell (`colspan > 1 || rowspan > 1`) back into individual 1x1 cells.
pub fn split_table_cell(table: &mut RichBlock, range: &EditTableCellRange) -> bool {
    if table.kind != RichBlockKind::Table || range.is_empty() {
        return false;
    }
    let grid = build_table_grid(table);
    let selected = selected_table_grid_cells(&grid, range);
    if selected.len() != 1 {
        return false;
    }
    let cell_ref = selected[0];
    let cell = &mut table.table_rows[cell_ref.row_index].cells[cell_ref.cell_index];
    let colspan = normalize_table_span(cell.colspan);
    let rowspan = normalize_table_span(cell.rowspan);
    if colspan <= 1 && rowspan <= 1 {
        return false;
    }
    let header = cell.header;
    cell.colspan = 1;
    cell.rowspan = 1;

    // Insert extra cells in the same row right after `cell_index`
    for offset in 1..colspan {
        table.table_rows[cell_ref.row_index].cells.insert(
            cell_ref.cell_index + offset,
            RichTableCell {
                header,
                ..RichTableCell::default()
            },
        );
    }
    // Insert extra cells into subsequent covered rows
    for row_idx in
        (cell_ref.row_index + 1)..(cell_ref.row_index + rowspan).min(table.table_rows.len())
    {
        let mut insert_at = table.table_rows[row_idx].cells.len();
        for c in &grid.cells {
            if c.row_index == row_idx && c.column_from >= cell_ref.column_from {
                insert_at = c.cell_index;
                break;
            }
        }
        for offset in 0..colspan {
            let pos = (insert_at + offset).min(table.table_rows[row_idx].cells.len());
            table.table_rows[row_idx].cells.insert(
                pos,
                RichTableCell {
                    header,
                    ..RichTableCell::default()
                },
            );
        }
    }
    true
}
