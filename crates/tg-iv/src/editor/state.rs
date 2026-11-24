//! Port of `iv/editor/iv_editor_state.h`, `iv/editor/iv_editor_state.cpp`, and the headless
//! undo/redo + cursor/selection + toolbar-state model of `iv/editor/iv_editor_widget.{h,cpp}`.
//!
//! Pure-Rust rich document editor model with zero UI dependency. Ready to drive an `egui`
//! rich compose/edit window (`crates/tg-app/src/webview/rich_editor.rs`).

use serde::{Deserialize, Serialize};
use tg_tl::enums as tl_enums;

use crate::editor::auto_pair::{
    AutoPairOutcome, closing_delimiter, handle_auto_pair_backspace, handle_auto_pair_char,
};
use crate::editor::blocks::{
    InsertAction, ListStyle, OrderedListType, block_is_empty, joinable_text_block_kind,
    list_item_is_empty, make_block_for_action, make_paragraph_block,
    reset_non_ordered_list_metadata, split_task_state, stored_ordered_list_type,
};
use crate::editor::path::{
    BlockContainerPath, BlockPath, EditSelection, EditTableCellRange, LeafKind, LeafPath,
    resolve_block, resolve_block_mut, resolve_container, resolve_container_mut,
    resolve_list_item_mut, resolve_table_cell_mut,
};
use crate::editor::table::{
    TableSelectionInfo, add_table_column, add_table_row, build_table_grid,
    erase_table_rows_keeping_spans, inspect_table_selection, selected_table_grid_cells,
    split_table_cell, unite_table_cells,
};
use crate::editor::text_ops::{
    TextFormattingAction, apply_formatting_to_range, formula_source_to_rich_text, range_has_entity,
    replace_plain_range, replace_rich_range, split_text_span,
};
use crate::markdown::document::MarkdownPrepareLimits;
use crate::markdown::parser::markdown_to_rich_page;
use crate::markdown::serializer::rich_page_to_markdown;
use crate::model::Page;
use crate::rich_page::{
    EntityType, RichBlock, RichBlockKind, RichListItem, RichListKind, RichMediaResolver,
    RichMessageLimitError, RichMessageLimits, RichPage, RichTableAlignment,
    RichTableVerticalAlignment, RichTaskState, RichTextNode, SerializeRichMessageMode,
    SerializeRichMessageResult, SerializeRichMessageStatus, TextWithEntities,
    rich_page_to_model_page, rich_page_to_tl_blocks, serialize_input_rich_message,
    utf16_to_byte_offset, validate_rich_message,
};

/// Default maximum undo stack depth.
pub const DEFAULT_MAX_UNDO_ENTRIES: usize = 128;

/// Editing mode of the active text leaf (`Iv::Editor::State::FieldMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FieldMode {
    /// Rich formatted text (`TextWithEntities`).
    #[default]
    Rich,
    /// Raw unformatted text (`Code` block body or `Math` TeX source).
    Raw,
}

/// Result of applying an editor mutation (`Iv::Editor::State::ApplyResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyResult {
    /// Mutation was rejected (invalid target or limit violation).
    Failed,
    /// State was valid and unchanged.
    Unchanged,
    /// State changed and was committed to history.
    Changed,
}

/// Descriptor for one editable text/formula leaf in document traversal order
/// (`Iv::Editor::State::TextNodeDescriptor`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextNodeDescriptor {
    /// Deterministic structural path to the leaf.
    pub leaf: LeafPath,
    /// Whether this leaf accepts rich formatting or raw text.
    pub mode: FieldMode,
    /// Owning block kind (convenience for UI styling).
    pub block_kind: RichBlockKind,
    /// Heading level if `block_kind == Heading`.
    pub heading_level: u8,
}

/// Caret and selection within the active editable leaf (UTF-16 code-unit offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditorCursor {
    /// Index into [`EditorState::text_nodes`].
    pub leaf_ordinal: usize,
    /// Selection anchor offset in UTF-16 code units.
    pub anchor: usize,
    /// Moving caret head offset in UTF-16 code units.
    pub head: usize,
}

impl EditorCursor {
    /// Creates a collapsed caret at `(leaf_ordinal, offset)`.
    #[must_use]
    pub const fn collapsed(leaf_ordinal: usize, offset: usize) -> Self {
        Self {
            leaf_ordinal,
            anchor: offset,
            head: offset,
        }
    }

    /// Returns normalized `(start, end)` selection offsets (`start <= end`).
    #[must_use]
    pub const fn selection_range(self) -> (usize, usize) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Returns true if a non-empty range is selected.
    #[must_use]
    pub const fn has_selection(self) -> bool {
        self.anchor != self.head
    }

    /// Returns true if the cursor is collapsed (no selection).
    #[must_use]
    pub const fn is_collapsed(self) -> bool {
        self.anchor == self.head
    }
}

/// Undo/redo snapshot (`Iv::Editor::State::Snapshot` + view selection).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorSnapshot {
    /// Complete document snapshot.
    pub rich_page: RichPage,
    /// Active leaf path.
    pub active_leaf: Option<LeafPath>,
    /// Caret & inline selection state.
    pub cursor: EditorCursor,
    /// Structural selection (blocks, list items, table rows/cells).
    pub structural_selection: EditSelection,
}

/// Enabled/active state of a single toolbar button (`Widget::ToolbarActionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolbarActionState {
    /// Whether the action is applicable in the current mode.
    pub enabled: bool,
    /// Whether the formatting style is currently active at the selection/caret.
    pub active: bool,
}

/// Aggregated formatting toolbar state (`Widget::ToolbarState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolbarState {
    /// Undo button state.
    pub undo: ToolbarActionState,
    /// Redo button state.
    pub redo: ToolbarActionState,
    /// Bold toggle state.
    pub bold: ToolbarActionState,
    /// Italic toggle state.
    pub italic: ToolbarActionState,
    /// Underline toggle state.
    pub underline: ToolbarActionState,
    /// Strikethrough toggle state.
    pub strike_out: ToolbarActionState,
    /// Inline code toggle state.
    pub code: ToolbarActionState,
    /// Spoiler toggle state.
    pub spoiler: ToolbarActionState,
    /// Subscript toggle state.
    pub subscript: ToolbarActionState,
    /// Superscript toggle state.
    pub superscript: ToolbarActionState,
    /// Marked/highlight toggle state.
    pub marked: ToolbarActionState,
    /// Clear formatting button state.
    pub plain_text: ToolbarActionState,
    /// Link button state (`active` means selection already contains a link).
    pub link: ToolbarActionState,
    /// Inline math button state.
    pub math: ToolbarActionState,
}

/// Headless rich document editor state (`Iv::Editor::State` + undo/redo & selection controller).
#[derive(Debug, Clone)]
pub struct EditorState {
    page: RichPage,
    limits: RichMessageLimits,
    text_nodes: Vec<TextNodeDescriptor>,
    cursor: EditorCursor,
    structural_selection: EditSelection,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
    max_undo_entries: usize,
    transaction_base: Option<EditorSnapshot>,
    last_limit_error: Option<RichMessageLimitError>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    /// Creates a blank editor containing a single empty paragraph.
    #[must_use]
    pub fn new() -> Self {
        Self::from_rich_page(RichPage::default(), RichMessageLimits::default())
    }

    /// Creates an editor initialized from an existing [`RichPage`] and [`RichMessageLimits`].
    #[must_use]
    pub fn from_rich_page(mut page: RichPage, limits: RichMessageLimits) -> Self {
        if page.blocks.is_empty() {
            page.blocks.push(make_paragraph_block());
        }
        let mut state = Self {
            page,
            limits,
            text_nodes: Vec::new(),
            cursor: EditorCursor::default(),
            structural_selection: EditSelection::None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_undo_entries: DEFAULT_MAX_UNDO_ENTRIES,
            transaction_base: None,
            last_limit_error: None,
        };
        state.rebuild_text_nodes();
        if state.text_nodes.is_empty() {
            state.page.blocks.push(make_paragraph_block());
            state.rebuild_text_nodes();
        }
        state
    }

    /// Parses Markdown and initializes an [`EditorState`].
    pub fn from_markdown(markdown: &str, limits: RichMessageLimits) -> Result<Self, String> {
        let page = markdown_to_rich_page(markdown)?;
        Ok(Self::from_rich_page(page, limits))
    }

    /// Sets the maximum number of retained undo entries.
    pub fn set_max_undo_entries(&mut self, max_entries: usize) {
        self.max_undo_entries = max_entries.max(1);
        while self.undo_stack.len() > self.max_undo_entries {
            self.undo_stack.remove(0);
        }
    }

    /// Returns the underlying [`RichPage`].
    #[must_use]
    pub const fn rich_page(&self) -> &RichPage {
        &self.page
    }

    /// Returns the active [`RichMessageLimits`].
    #[must_use]
    pub const fn limits(&self) -> &RichMessageLimits {
        &self.limits
    }

    /// Returns the most recent limit violation error, if any.
    #[must_use]
    pub const fn last_limit_error(&self) -> Option<RichMessageLimitError> {
        self.last_limit_error
    }

    /// Validates the current document against configured limits.
    #[must_use]
    pub fn validate(&self) -> Option<RichMessageLimitError> {
        validate_rich_message(&self.page, &self.limits)
    }

    /// Returns true if all blocks in the document are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.page.blocks.iter().all(block_is_empty)
    }

    /// Returns all editable leaves in document order.
    #[must_use]
    pub fn text_nodes(&self) -> &[TextNodeDescriptor] {
        &self.text_nodes
    }

    /// Number of editable leaves.
    #[must_use]
    pub fn text_node_count(&self) -> usize {
        self.text_nodes.len()
    }

    /// Current caret and inline selection.
    #[must_use]
    pub const fn cursor(&self) -> EditorCursor {
        self.cursor
    }

    /// Active leaf ordinal (`cursor().leaf_ordinal`).
    #[must_use]
    pub const fn active_text_ordinal(&self) -> usize {
        self.cursor.leaf_ordinal
    }

    /// Structural path of the active editable leaf, if any.
    #[must_use]
    pub fn active_leaf_path(&self) -> Option<&LeafPath> {
        self.text_nodes
            .get(self.cursor.leaf_ordinal)
            .map(|d| &d.leaf)
    }

    /// Field mode (`Rich` vs `Raw`) of the active leaf.
    #[must_use]
    pub fn active_field_mode(&self) -> FieldMode {
        self.text_nodes
            .get(self.cursor.leaf_ordinal)
            .map_or(FieldMode::Rich, |d| d.mode)
    }

    /// Returns the current structural selection.
    #[must_use]
    pub const fn structural_selection(&self) -> &EditSelection {
        &self.structural_selection
    }

    /// Sets the structural selection.
    pub fn set_structural_selection(&mut self, selection: EditSelection) {
        self.structural_selection = selection;
    }

    /// Returns the ordinal of a [`LeafPath`] in `text_nodes()`.
    #[must_use]
    pub fn text_ordinal_for_leaf(&self, path: &LeafPath) -> Option<usize> {
        self.text_nodes.iter().position(|d| &d.leaf == path)
    }

    /// Returns the rich text of the active leaf (or plain wrapped text for raw leaves).
    #[must_use]
    pub fn active_text(&self) -> TextWithEntities {
        let Some(desc) = self.text_nodes.get(self.cursor.leaf_ordinal) else {
            return TextWithEntities::default();
        };
        self.leaf_text(&desc.leaf)
    }

    /// Returns the raw string of the active leaf.
    #[must_use]
    pub fn active_raw_text(&self) -> String {
        self.active_text().text
    }

    /// Returns the UTF-16 length of the active leaf.
    #[must_use]
    pub fn active_text_len(&self) -> usize {
        self.active_text().utf16_len()
    }

    /// Returns the previous editable leaf ordinal, if any.
    #[must_use]
    pub const fn previous_editable_ordinal(&self) -> Option<usize> {
        self.cursor.leaf_ordinal.checked_sub(1)
    }

    /// Returns the next editable leaf ordinal, if any.
    #[must_use]
    pub fn next_editable_ordinal(&self) -> Option<usize> {
        let next = self.cursor.leaf_ordinal + 1;
        (next < self.text_nodes.len()).then_some(next)
    }

    // ========================================================================
    // Cursor & Selection Navigation
    // ========================================================================

    /// Activates leaf `ordinal`, placing the collapsed caret at its end.
    pub fn set_active_text_by_ordinal(&mut self, ordinal: usize) -> bool {
        if ordinal >= self.text_nodes.len() {
            return false;
        }
        let len = self.leaf_text(&self.text_nodes[ordinal].leaf).utf16_len();
        self.cursor = EditorCursor::collapsed(ordinal, len);
        self.structural_selection = EditSelection::None;
        true
    }

    /// Sets a collapsed cursor at `(leaf_ordinal, offset)` (clamped to leaf UTF-16 length).
    pub fn set_cursor(&mut self, leaf_ordinal: usize, offset: usize) -> bool {
        if leaf_ordinal >= self.text_nodes.len() {
            return false;
        }
        let max_len = self
            .leaf_text(&self.text_nodes[leaf_ordinal].leaf)
            .utf16_len();
        let clamped = offset.min(max_len);
        self.cursor = EditorCursor::collapsed(leaf_ordinal, clamped);
        self.structural_selection = EditSelection::None;
        true
    }

    /// Sets an inline selection `(anchor, head)` within `leaf_ordinal`.
    pub fn set_selection(&mut self, leaf_ordinal: usize, anchor: usize, head: usize) -> bool {
        if leaf_ordinal >= self.text_nodes.len() {
            return false;
        }
        let max_len = self
            .leaf_text(&self.text_nodes[leaf_ordinal].leaf)
            .utf16_len();
        self.cursor = EditorCursor {
            leaf_ordinal,
            anchor: anchor.min(max_len),
            head: head.min(max_len),
        };
        self.structural_selection = EditSelection::None;
        true
    }

    /// Selects all text in the active leaf.
    pub fn select_all_in_leaf(&mut self) {
        let len = self.active_text_len();
        self.cursor.anchor = 0;
        self.cursor.head = len;
    }

    /// Moves the caret one Unicode scalar value left (crossing to the previous leaf at offset 0).
    pub fn move_cursor_left(&mut self, extend_selection: bool) -> bool {
        if !extend_selection && self.cursor.has_selection() {
            let (start, _) = self.cursor.selection_range();
            self.cursor.anchor = start;
            self.cursor.head = start;
            return true;
        }
        let text = self.active_raw_text();
        if self.cursor.head > 0 {
            let byte_pos = utf16_to_byte_offset(&text, self.cursor.head);
            let prev_len = text[..byte_pos]
                .chars()
                .next_back()
                .map_or(1, char::len_utf16);
            self.cursor.head = self.cursor.head.saturating_sub(prev_len);
            if !extend_selection {
                self.cursor.anchor = self.cursor.head;
            }
            return true;
        }
        if !extend_selection && let Some(prev) = self.previous_editable_ordinal() {
            let len = self.leaf_text(&self.text_nodes[prev].leaf).utf16_len();
            self.cursor = EditorCursor::collapsed(prev, len);
            return true;
        }
        false
    }

    /// Moves the caret one Unicode scalar value right (crossing to the next leaf at end).
    pub fn move_cursor_right(&mut self, extend_selection: bool) -> bool {
        if !extend_selection && self.cursor.has_selection() {
            let (_, end) = self.cursor.selection_range();
            self.cursor.anchor = end;
            self.cursor.head = end;
            return true;
        }
        let text = self.active_raw_text();
        let max_len = text.encode_utf16().count();
        if self.cursor.head < max_len {
            let byte_pos = utf16_to_byte_offset(&text, self.cursor.head);
            let next_len = text[byte_pos..].chars().next().map_or(1, char::len_utf16);
            self.cursor.head = (self.cursor.head + next_len).min(max_len);
            if !extend_selection {
                self.cursor.anchor = self.cursor.head;
            }
            return true;
        }
        if !extend_selection && let Some(next) = self.next_editable_ordinal() {
            self.cursor = EditorCursor::collapsed(next, 0);
            return true;
        }
        false
    }

    // ========================================================================
    // Snapshots & Undo / Redo History
    // ========================================================================

    /// Captures a full restorable snapshot of document, cursor, and selection.
    #[must_use]
    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            rich_page: self.page.clone(),
            active_leaf: self.active_leaf_path().cloned(),
            cursor: self.cursor,
            structural_selection: self.structural_selection.clone(),
        }
    }

    /// Restores a previously captured [`EditorSnapshot`] (without altering undo history).
    pub fn restore_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.page = snapshot.rich_page;
        self.structural_selection = snapshot.structural_selection;
        self.rebuild_text_nodes();
        if let Some(leaf) = snapshot.active_leaf
            && let Some(ord) = self.text_ordinal_for_leaf(&leaf)
        {
            let max_len = self.leaf_text(&self.text_nodes[ord].leaf).utf16_len();
            self.cursor = EditorCursor {
                leaf_ordinal: ord,
                anchor: snapshot.cursor.anchor.min(max_len),
                head: snapshot.cursor.head.min(max_len),
            };
        } else {
            self.clamp_cursor();
        }
    }

    /// Returns true if an undo step is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns true if a redo step is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of stored undo states.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of stored redo states.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Undoes the most recent mutation, returning `true` if state changed.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.redo_stack.push(current);
        self.restore_snapshot(prev);
        self.last_limit_error = None;
        true
    }

    /// Re-applies the most recently undone mutation, returning `true` if state changed.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.undo_stack.push(current);
        self.restore_snapshot(next);
        self.last_limit_error = None;
        true
    }

    /// Begins a grouped transaction so multiple mutations coalesce into one undo entry.
    pub fn begin_transaction(&mut self) {
        if self.transaction_base.is_none() {
            self.transaction_base = Some(self.snapshot());
        }
    }

    /// Commits an active transaction as a single undo entry if the page changed.
    pub fn commit_transaction(&mut self) {
        if let Some(base) = self.transaction_base.take()
            && base.rich_page != self.page
        {
            self.push_undo_snapshot(base);
        }
    }

    /// Rolls back an active transaction to its starting snapshot.
    pub fn rollback_transaction(&mut self) {
        if let Some(base) = self.transaction_base.take() {
            self.restore_snapshot(base);
        }
    }

    fn push_undo_snapshot(&mut self, snap: EditorSnapshot) {
        self.redo_stack.clear();
        self.undo_stack.push(snap);
        while self.undo_stack.len() > self.max_undo_entries {
            self.undo_stack.remove(0);
        }
    }

    fn apply_checked<F>(&mut self, mutate: F) -> ApplyResult
    where
        F: FnOnce(&mut Self) -> ApplyResult,
    {
        let before = self.snapshot();
        let res = mutate(self);
        if res != ApplyResult::Changed {
            return res;
        }
        self.rebuild_text_nodes();
        if self.text_nodes.is_empty() {
            self.page.blocks.push(make_paragraph_block());
            self.rebuild_text_nodes();
        }
        if let Some(err) = validate_rich_message(&self.page, &self.limits) {
            self.last_limit_error = Some(err);
            self.restore_snapshot(before);
            return ApplyResult::Failed;
        }
        self.last_limit_error = None;
        self.clamp_cursor();
        if self.transaction_base.is_none() {
            self.push_undo_snapshot(before);
        }
        ApplyResult::Changed
    }

    // ========================================================================
    // Text & Inline Style Mutations
    // ========================================================================

    /// Replaces the entire active rich leaf text.
    pub fn apply_active_text(&mut self, text: TextWithEntities) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let current = s.leaf_text(&desc.leaf);
            if current == text {
                return ApplyResult::Unchanged;
            }
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            let len = s.leaf_text(&desc.leaf).utf16_len();
            s.cursor.anchor = s.cursor.anchor.min(len);
            s.cursor.head = s.cursor.head.min(len);
            ApplyResult::Changed
        })
    }

    /// Replaces the entire active leaf with raw unformatted text (code/math/plain).
    pub fn apply_active_raw_text(&mut self, raw: String) -> ApplyResult {
        self.apply_active_text(TextWithEntities::simple(raw))
    }

    /// Inserts plain text at the current cursor/selection.
    pub fn insert_text(&mut self, input: &str) -> ApplyResult {
        if input.is_empty() && !self.cursor.has_selection() {
            return ApplyResult::Unchanged;
        }
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let (from, till) = s.cursor.selection_range();
            let mut text = s.leaf_text(&desc.leaf);
            let new_pos = replace_plain_range(&mut text, from, till, input);
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            s.cursor.anchor = new_pos;
            s.cursor.head = new_pos;
            ApplyResult::Changed
        })
    }

    /// Replaces the current inline selection with formatted [`TextWithEntities`].
    pub fn replace_selection(&mut self, replacement: TextWithEntities) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let (from, till) = s.cursor.selection_range();
            let mut text = s.leaf_text(&desc.leaf);
            let new_pos = if desc.mode == FieldMode::Raw {
                replace_plain_range(&mut text, from, till, &replacement.text)
            } else {
                replace_rich_range(&mut text, from, till, &replacement)
            };
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            s.cursor.anchor = new_pos;
            s.cursor.head = new_pos;
            ApplyResult::Changed
        })
    }

    /// Inserts an inline math formula (`$tex$`) at the current cursor/selection.
    pub fn insert_inline_math(&mut self, tex: &str) -> ApplyResult {
        if self.active_field_mode() != FieldMode::Rich {
            return ApplyResult::Failed;
        }
        let formula = formula_source_to_rich_text(tex);
        if formula.is_empty() {
            return ApplyResult::Unchanged;
        }
        self.replace_selection(formula)
    }

    /// Deletes the selection, or one character before the caret, or joins with the previous block
    /// when pressed at offset 0.
    pub fn delete_backward(&mut self) -> ApplyResult {
        if !self.structural_selection.is_empty() {
            let sel = self.structural_selection.clone();
            return self.remove_structural_selection(sel);
        }
        if self.cursor.has_selection() {
            return self.insert_text("");
        }
        if self.cursor.head == 0 {
            return self.join_with_previous_block();
        }
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let mut text = s.leaf_text(&desc.leaf);
            let byte_pos = utf16_to_byte_offset(&text.text, s.cursor.head);
            let Some(prev_ch) = text.text[..byte_pos].chars().next_back() else {
                return ApplyResult::Unchanged;
            };
            let from = s.cursor.head.saturating_sub(prev_ch.len_utf16());
            let new_pos = replace_plain_range(&mut text, from, s.cursor.head, "");
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            s.cursor.anchor = new_pos;
            s.cursor.head = new_pos;
            ApplyResult::Changed
        })
    }

    /// Deletes the selection, or one character after the caret, or joins with the next block
    /// when pressed at the end of the leaf.
    pub fn delete_forward(&mut self) -> ApplyResult {
        if !self.structural_selection.is_empty() {
            let sel = self.structural_selection.clone();
            return self.remove_structural_selection(sel);
        }
        if self.cursor.has_selection() {
            return self.insert_text("");
        }
        let max_len = self.active_text_len();
        if self.cursor.head >= max_len {
            return self.join_with_next_block();
        }
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let mut text = s.leaf_text(&desc.leaf);
            let byte_pos = utf16_to_byte_offset(&text.text, s.cursor.head);
            let Some(next_ch) = text.text[byte_pos..].chars().next() else {
                return ApplyResult::Unchanged;
            };
            let till = s.cursor.head + next_ch.len_utf16();
            let new_pos = replace_plain_range(&mut text, s.cursor.head, till, "");
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            s.cursor.anchor = new_pos;
            s.cursor.head = new_pos;
            ApplyResult::Changed
        })
    }

    /// Applies auto-pairing rules when typing `ch`; falls back to [`Self::insert_text`] if unhandled.
    pub fn handle_auto_pair_char(&mut self, ch: char) -> ApplyResult {
        let mut applied_pair = false;
        let result = self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let mut text = s.leaf_text(&desc.leaf);
            let mut raw = text.text.clone();
            match handle_auto_pair_char(&mut raw, s.cursor.anchor, s.cursor.head, ch) {
                AutoPairOutcome::Unhandled => ApplyResult::Unchanged,
                AutoPairOutcome::Applied { anchor, head } => {
                    applied_pair = true;
                    // Skip-over moves caret without mutating text or pushing a spurious undo step.
                    if raw == text.text {
                        s.cursor.anchor = anchor;
                        s.cursor.head = head;
                        return ApplyResult::Unchanged;
                    }
                    // Update text with entity-preserving splice when wrapping or inserting pair.
                    let (from, till) = s.cursor.selection_range();
                    if s.cursor.has_selection() {
                        let close_ch = closing_delimiter(ch).unwrap_or(ch);
                        let mut open_s = [0u8; 4];
                        let mut close_s = [0u8; 4];
                        let _ = replace_plain_range(
                            &mut text,
                            till,
                            till,
                            close_ch.encode_utf8(&mut close_s),
                        );
                        let _ =
                            replace_plain_range(&mut text, from, from, ch.encode_utf8(&mut open_s));
                    } else {
                        let added_len = raw.len().saturating_sub(text.text.len());
                        let byte_from = utf16_to_byte_offset(&text.text, from);
                        let inserted = &raw[byte_from..byte_from + added_len];
                        let _ = replace_plain_range(&mut text, from, till, inserted);
                    }
                    if !s.set_leaf_text(&desc.leaf, text) {
                        return ApplyResult::Failed;
                    }
                    s.cursor.anchor = anchor;
                    s.cursor.head = head;
                    ApplyResult::Changed
                }
            }
        });
        if applied_pair {
            result
        } else {
            let mut buf = [0u8; 4];
            self.insert_text(ch.encode_utf8(&mut buf))
        }
    }

    /// Applies auto-pair backspace removal; falls back to [`Self::delete_backward`] if unhandled.
    pub fn handle_auto_pair_backspace(&mut self) -> ApplyResult {
        let mut applied_pair = false;
        let result = self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let mut text = s.leaf_text(&desc.leaf);
            let mut raw = text.text.clone();
            match handle_auto_pair_backspace(&mut raw, s.cursor.anchor, s.cursor.head) {
                AutoPairOutcome::Unhandled => ApplyResult::Unchanged,
                AutoPairOutcome::Applied { anchor, head } => {
                    applied_pair = true;
                    let from = s.cursor.head.saturating_sub(1);
                    let till = (s.cursor.head + 1).min(text.utf16_len());
                    let _ = replace_plain_range(&mut text, from, till, "");
                    if !s.set_leaf_text(&desc.leaf, text) {
                        return ApplyResult::Failed;
                    }
                    s.cursor.anchor = anchor;
                    s.cursor.head = head;
                    ApplyResult::Changed
                }
            }
        });
        if applied_pair {
            result
        } else {
            self.delete_backward()
        }
    }

    /// Applies or toggles an inline formatting style over the active selection.
    pub fn apply_formatting(
        &mut self,
        action: TextFormattingAction,
        enabled: Option<bool>,
    ) -> ApplyResult {
        if self.active_field_mode() != FieldMode::Rich || !self.cursor.has_selection() {
            return ApplyResult::Unchanged;
        }
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let (from, till) = s.cursor.selection_range();
            let mut text = s.leaf_text(&desc.leaf);
            if !apply_formatting_to_range(&mut text, from, till, &action, enabled) {
                return ApplyResult::Unchanged;
            }
            if !s.set_leaf_text(&desc.leaf, text) {
                return ApplyResult::Failed;
            }
            ApplyResult::Changed
        })
    }

    /// Toggles an inline formatting style on the current selection.
    pub fn toggle_formatting(&mut self, action: TextFormattingAction) -> ApplyResult {
        self.apply_formatting(action, None)
    }

    /// Computes the formatting toolbar button states for the current caret/selection.
    #[must_use]
    pub fn toolbar_state(&self) -> ToolbarState {
        let rich = self.active_field_mode() == FieldMode::Rich;
        let has_sel = self.cursor.has_selection();
        let text = self.active_text();
        let (from, till) = self.cursor.selection_range();
        let style_state = |kind: EntityType| ToolbarActionState {
            enabled: rich && has_sel,
            active: rich && range_has_entity(&text, from, till, kind),
        };
        ToolbarState {
            undo: ToolbarActionState {
                enabled: self.can_undo(),
                active: false,
            },
            redo: ToolbarActionState {
                enabled: self.can_redo(),
                active: false,
            },
            bold: style_state(EntityType::Bold),
            italic: style_state(EntityType::Italic),
            underline: style_state(EntityType::Underline),
            strike_out: style_state(EntityType::StrikeOut),
            code: style_state(EntityType::Code),
            spoiler: style_state(EntityType::Spoiler),
            subscript: style_state(EntityType::Subscript),
            superscript: style_state(EntityType::Superscript),
            marked: style_state(EntityType::Marked),
            plain_text: ToolbarActionState {
                enabled: rich && has_sel && !text.entities.is_empty(),
                active: false,
            },
            link: style_state(EntityType::CustomUrl),
            math: ToolbarActionState {
                enabled: rich,
                active: false,
            },
        }
    }

    // ========================================================================
    // Block & Structural Operations
    // ========================================================================

    /// Inserts a new block immediately after the active leaf's owning block.
    pub fn insert_block_after_active(&mut self, action: InsertAction) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let new_block = make_block_for_action(&action);
            let container = desc.leaf.block.container.clone();
            let insert_idx = desc.leaf.block.index + 1;
            let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                return ApplyResult::Failed;
            };
            let clamped_idx = insert_idx.min(vec.len());
            vec.insert(clamped_idx, new_block);
            let target_path = BlockPath {
                container,
                index: clamped_idx,
            };
            s.rebuild_text_nodes();
            if let Some(ord) = s.first_leaf_ordinal_in_block(&target_path) {
                s.cursor = EditorCursor::collapsed(ord, 0);
            }
            ApplyResult::Changed
        })
    }

    /// Converts the active top-level/container text block into the block type specified by `action`.
    pub fn convert_active_block(&mut self, action: InsertAction) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let block_path = desc.leaf.block.clone();
            let existing_text = s.leaf_text(&desc.leaf);
            let mut replacement = make_block_for_action(&action);
            match replacement.kind {
                RichBlockKind::Paragraph
                | RichBlockKind::Heading
                | RichBlockKind::Footer
                | RichBlockKind::Quote => {
                    replacement.text = RichTextNode::from_text(existing_text);
                }
                RichBlockKind::Code => {
                    replacement.text = RichTextNode::plain(existing_text.text);
                }
                RichBlockKind::Math => {
                    replacement.formula = existing_text.text;
                }
                RichBlockKind::List => {
                    if let Some(first) = replacement.list_items.first_mut() {
                        first.text = RichTextNode::from_text(existing_text);
                    }
                }
                RichBlockKind::Details => {
                    replacement.text = RichTextNode::from_text(existing_text);
                }
                _ => {}
            }
            let Some(slot) = resolve_block_mut(&mut s.page, &block_path) else {
                return ApplyResult::Failed;
            };
            *slot = replacement;
            s.rebuild_text_nodes();
            if let Some(ord) = s.first_leaf_ordinal_in_block(&block_path) {
                let len = s.leaf_text(&s.text_nodes[ord].leaf).utf16_len();
                s.cursor = EditorCursor::collapsed(ord, s.cursor.head.min(len));
            }
            ApplyResult::Changed
        })
    }

    /// Handles pressing `Enter` at the current cursor/selection:
    /// - Raw code / math blocks: inserts `\n`
    /// - Paragraphs / Headings / Footers: splits into two blocks (new paragraph after heading/footer)
    /// - List items: splits into a new list item, or exits an empty list item into a paragraph
    /// - Quotes: splits into nested paragraph or exits empty quote to a trailing paragraph
    pub fn handle_enter(&mut self) -> ApplyResult {
        if self.active_field_mode() == FieldMode::Raw {
            return self.insert_text("\n");
        }
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            let (from, till) = s.cursor.selection_range();
            let current = s.leaf_text(&desc.leaf);
            let Some((head, _sel, tail)) = split_text_span(&current, from, till) else {
                return ApplyResult::Failed;
            };

            match desc.leaf.kind {
                LeafKind::ListItemText => {
                    let Some(item_idx) = desc.leaf.list_item_index else {
                        return ApplyResult::Failed;
                    };
                    let block_path = desc.leaf.block.clone();
                    let is_empty_item = head.is_empty() && tail.is_empty();
                    if is_empty_item {
                        // Exit empty list item into a paragraph after the list
                        let Some(list_block) = resolve_block_mut(&mut s.page, &block_path) else {
                            return ApplyResult::Failed;
                        };
                        if item_idx < list_block.list_items.len() {
                            list_block.list_items.remove(item_idx);
                        }
                        let list_now_empty = list_block.list_items.is_empty();
                        let container = block_path.container.clone();
                        let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                            return ApplyResult::Failed;
                        };
                        let new_idx = if list_now_empty {
                            vec[block_path.index] = make_paragraph_block();
                            block_path.index
                        } else {
                            vec.insert(block_path.index + 1, make_paragraph_block());
                            block_path.index + 1
                        };
                        let target = BlockPath {
                            container,
                            index: new_idx,
                        };
                        s.rebuild_text_nodes();
                        if let Some(ord) = s.first_leaf_ordinal_in_block(&target) {
                            s.cursor = EditorCursor::collapsed(ord, 0);
                        }
                        return ApplyResult::Changed;
                    }

                    let Some(list_block) = resolve_block_mut(&mut s.page, &block_path) else {
                        return ApplyResult::Failed;
                    };
                    let Some(item) = list_block.list_items.get_mut(item_idx) else {
                        return ApplyResult::Failed;
                    };
                    let next_task = split_task_state(item.task_state);
                    item.text = RichTextNode::from_text(head);
                    let new_item = RichListItem {
                        task_state: next_task,
                        text: RichTextNode::from_text(tail),
                        ..RichListItem::default()
                    };
                    list_block.list_items.insert(item_idx + 1, new_item);
                    let target_leaf = LeafPath::list_item_text(block_path, item_idx + 1);
                    s.rebuild_text_nodes();
                    if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                        s.cursor = EditorCursor::collapsed(ord, 0);
                    }
                    ApplyResult::Changed
                }
                LeafKind::BlockText => {
                    let block_path = desc.leaf.block.clone();
                    let kind = desc.block_kind;
                    if kind == RichBlockKind::Table || kind == RichBlockKind::Details {
                        // Move focus into the first child cell / block
                        if let Some(next_ord) = s.next_editable_ordinal() {
                            s.cursor = EditorCursor::collapsed(next_ord, 0);
                            return ApplyResult::Unchanged;
                        }
                        return ApplyResult::Unchanged;
                    }
                    if !s.set_leaf_text(&desc.leaf, head) {
                        return ApplyResult::Failed;
                    }
                    let mut next_block = make_paragraph_block();
                    next_block.text = RichTextNode::from_text(tail);
                    let container = block_path.container.clone();
                    let insert_idx = block_path.index + 1;
                    let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                        return ApplyResult::Failed;
                    };
                    vec.insert(insert_idx, next_block);
                    let target_leaf = LeafPath::block_text(BlockPath {
                        container,
                        index: insert_idx,
                    });
                    s.rebuild_text_nodes();
                    if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                        s.cursor = EditorCursor::collapsed(ord, 0);
                    }
                    ApplyResult::Changed
                }
                LeafKind::TableCellText | LeafKind::BlockCaption | LeafKind::MathFormula => {
                    let mut text = s.leaf_text(&desc.leaf);
                    let new_pos = replace_plain_range(&mut text, from, till, "\n");
                    if !s.set_leaf_text(&desc.leaf, text) {
                        return ApplyResult::Failed;
                    }
                    s.cursor.anchor = new_pos;
                    s.cursor.head = new_pos;
                    ApplyResult::Changed
                }
            }
        })
    }

    /// Joins the active block/list-item backward with its previous sibling (`Backspace` at offset 0).
    pub fn join_with_previous_block(&mut self) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            // Case 1: ListItemText joining with previous list item, or lifting item 0 to paragraph
            if desc.leaf.kind == LeafKind::ListItemText
                && let Some(item_idx) = desc.leaf.list_item_index
            {
                let block_path = desc.leaf.block.clone();
                if item_idx > 0 {
                    let Some(list_block) = resolve_block_mut(&mut s.page, &block_path) else {
                        return ApplyResult::Failed;
                    };
                    if list_block.list_items[item_idx - 1].blocks.is_empty()
                        && list_block.list_items[item_idx].blocks.is_empty()
                    {
                        let removed = list_block.list_items.remove(item_idx);
                        let prev = &mut list_block.list_items[item_idx - 1];
                        let seam = prev.text.text.utf16_len();
                        prev.text.text.append(removed.text.text);
                        let target_leaf = LeafPath::list_item_text(block_path, item_idx - 1);
                        s.rebuild_text_nodes();
                        if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                            s.cursor = EditorCursor::collapsed(ord, seam);
                        }
                        return ApplyResult::Changed;
                    }
                } else {
                    // Lift first list item into a paragraph before/replacing the list
                    let Some(list_block) = resolve_block_mut(&mut s.page, &block_path) else {
                        return ApplyResult::Failed;
                    };
                    if list_block.list_items[0].blocks.is_empty() {
                        let first = list_block.list_items.remove(0);
                        let list_empty = list_block.list_items.is_empty();
                        let mut para = make_paragraph_block();
                        para.text = first.text;
                        let container = block_path.container.clone();
                        let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                            return ApplyResult::Failed;
                        };
                        if list_empty {
                            vec[block_path.index] = para;
                        } else {
                            vec.insert(block_path.index, para);
                        }
                        let target = LeafPath::block_text(block_path);
                        s.rebuild_text_nodes();
                        if let Some(ord) = s.text_ordinal_for_leaf(&target) {
                            s.cursor = EditorCursor::collapsed(ord, 0);
                        }
                        return ApplyResult::Changed;
                    }
                }
            }

            // Case 2: Non-paragraph heading/footer/quote at offset 0 resets to paragraph if empty
            if desc.leaf.kind == LeafKind::BlockText
                && desc.block_kind != RichBlockKind::Paragraph
                && s.leaf_text(&desc.leaf).is_empty()
            {
                let bp = desc.leaf.block.clone();
                if let Some(blk) = resolve_block_mut(&mut s.page, &bp)
                    && matches!(
                        blk.kind,
                        RichBlockKind::Heading | RichBlockKind::Footer | RichBlockKind::Quote
                    )
                {
                    *blk = make_paragraph_block();
                    s.rebuild_text_nodes();
                    if let Some(ord) = s.text_ordinal_for_leaf(&LeafPath::block_text(bp)) {
                        s.cursor = EditorCursor::collapsed(ord, 0);
                    }
                    return ApplyResult::Changed;
                }
            }

            // Case 3: Joinable text block joining into preceding joinable sibling
            if desc.leaf.kind == LeafKind::BlockText
                && joinable_text_block_kind(desc.block_kind)
                && desc.leaf.block.index > 0
            {
                let container = desc.leaf.block.container.clone();
                let idx = desc.leaf.block.index;
                let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                    return ApplyResult::Failed;
                };
                if joinable_text_block_kind(vec[idx - 1].kind) {
                    let removed = vec.remove(idx);
                    let seam = vec[idx - 1].text.text.utf16_len();
                    vec[idx - 1].text.text.append(removed.text.text);
                    let target = LeafPath::block_text(BlockPath {
                        container,
                        index: idx - 1,
                    });
                    s.rebuild_text_nodes();
                    if let Some(ord) = s.text_ordinal_for_leaf(&target) {
                        s.cursor = EditorCursor::collapsed(ord, seam);
                    }
                    return ApplyResult::Changed;
                }
            }

            ApplyResult::Unchanged
        })
    }

    /// Joins the active block/list-item forward with its next sibling (`Delete` at end of leaf).
    pub fn join_with_next_block(&mut self) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            if desc.leaf.kind == LeafKind::ListItemText
                && let Some(item_idx) = desc.leaf.list_item_index
            {
                let block_path = desc.leaf.block.clone();
                let Some(list_block) = resolve_block_mut(&mut s.page, &block_path) else {
                    return ApplyResult::Failed;
                };
                if item_idx + 1 < list_block.list_items.len()
                    && list_block.list_items[item_idx].blocks.is_empty()
                    && list_block.list_items[item_idx + 1].blocks.is_empty()
                {
                    let next_item = list_block.list_items.remove(item_idx + 1);
                    let cur = &mut list_block.list_items[item_idx];
                    let seam = cur.text.text.utf16_len();
                    cur.text.text.append(next_item.text.text);
                    s.cursor = EditorCursor::collapsed(s.cursor.leaf_ordinal, seam);
                    return ApplyResult::Changed;
                }
            }
            if desc.leaf.kind == LeafKind::BlockText && joinable_text_block_kind(desc.block_kind) {
                let container = desc.leaf.block.container.clone();
                let idx = desc.leaf.block.index;
                let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                    return ApplyResult::Failed;
                };
                if idx + 1 < vec.len() && joinable_text_block_kind(vec[idx + 1].kind) {
                    let next_block = vec.remove(idx + 1);
                    let seam = vec[idx].text.text.utf16_len();
                    vec[idx].text.text.append(next_block.text.text);
                    s.cursor = EditorCursor::collapsed(s.cursor.leaf_ordinal, seam);
                    return ApplyResult::Changed;
                }
            }
            ApplyResult::Unchanged
        })
    }

    /// Removes a single block at `path`.
    pub fn remove_block(&mut self, path: BlockPath) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(vec) = resolve_container_mut(&mut s.page, &path.container) else {
                return ApplyResult::Failed;
            };
            if path.index >= vec.len() {
                return ApplyResult::Failed;
            }
            vec.remove(path.index);
            if s.page.blocks.is_empty() {
                s.page.blocks.push(make_paragraph_block());
            }
            s.structural_selection = EditSelection::None;
            ApplyResult::Changed
        })
    }

    /// Removes a structural selection (`Blocks`, `ListItems`, `TableRows`, or clears `TableCells`).
    pub fn remove_structural_selection(&mut self, selection: EditSelection) -> ApplyResult {
        if selection.is_empty() {
            return ApplyResult::Unchanged;
        }
        self.apply_checked(|s| {
            match selection {
                EditSelection::None => return ApplyResult::Unchanged,
                EditSelection::Blocks(r) => {
                    let Some(vec) = resolve_container_mut(&mut s.page, &r.container) else {
                        return ApplyResult::Failed;
                    };
                    if r.till > vec.len() {
                        return ApplyResult::Failed;
                    }
                    vec.drain(r.from..r.till);
                    if s.page.blocks.is_empty() {
                        s.page.blocks.push(make_paragraph_block());
                    }
                }
                EditSelection::ListItems(r) => {
                    let Some(blk) = resolve_block_mut(&mut s.page, &r.block) else {
                        return ApplyResult::Failed;
                    };
                    if blk.kind != RichBlockKind::List || r.till > blk.list_items.len() {
                        return ApplyResult::Failed;
                    }
                    blk.list_items.drain(r.from..r.till);
                    if blk.list_items.is_empty()
                        && let Some(vec) = resolve_container_mut(&mut s.page, &r.block.container)
                    {
                        vec.remove(r.block.index);
                    }
                }
                EditSelection::TableRows(r) => {
                    let Some(blk) = resolve_block_mut(&mut s.page, &r.block) else {
                        return ApplyResult::Failed;
                    };
                    if blk.kind != RichBlockKind::Table || r.till > blk.table_rows.len() {
                        return ApplyResult::Failed;
                    }
                    erase_table_rows_keeping_spans(
                        blk,
                        MarkdownPrepareLimits::IV_DEFAULT.table_render,
                        r.from,
                        r.till,
                    );
                    if blk.table_rows.is_empty()
                        && let Some(vec) = resolve_container_mut(&mut s.page, &r.block.container)
                    {
                        vec.remove(r.block.index);
                    }
                }
                EditSelection::TableCells(r) => {
                    let Some(blk) = resolve_block_mut(&mut s.page, &r.block) else {
                        return ApplyResult::Failed;
                    };
                    let grid = build_table_grid(blk);
                    let cells = selected_table_grid_cells(&grid, &r);
                    for c in cells {
                        if let Some(cell) = blk
                            .table_rows
                            .get_mut(c.row_index)
                            .and_then(|row| row.cells.get_mut(c.cell_index))
                        {
                            cell.text = RichTextNode::default();
                        }
                    }
                }
            }
            s.structural_selection = EditSelection::None;
            ApplyResult::Changed
        })
    }

    // ========================================================================
    // List, Quote, Details, Code, Math & Media Block Commands
    // ========================================================================

    /// Toggles a task list item's checkbox (`Unchecked <-> Checked`).
    pub fn toggle_task_state(&mut self, block: &BlockPath, item_index: usize) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(item) = resolve_list_item_mut(&mut s.page, &block, item_index) else {
                return ApplyResult::Failed;
            };
            match item.task_state {
                RichTaskState::Unchecked => item.task_state = RichTaskState::Checked,
                RichTaskState::Checked => item.task_state = RichTaskState::Unchecked,
                RichTaskState::None => return ApplyResult::Unchanged,
            }
            ApplyResult::Changed
        })
    }

    /// Indents (sinks) the active list item as a nested sub-list under the previous item.
    pub fn sink_active_list_item(&mut self) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            if desc.leaf.kind != LeafKind::ListItemText {
                return ApplyResult::Unchanged;
            }
            let Some(item_idx) = desc.leaf.list_item_index else {
                return ApplyResult::Failed;
            };
            if item_idx == 0 {
                return ApplyResult::Unchanged;
            }
            let block_path = desc.leaf.block.clone();
            let Some(list) = resolve_block_mut(&mut s.page, &block_path) else {
                return ApplyResult::Failed;
            };
            let list_kind = list.list_kind;
            let moved = list.list_items.remove(item_idx);
            let parent_item = &mut list.list_items[item_idx - 1];
            let new_item_idx = if let Some(last) = parent_item.blocks.last_mut()
                && last.kind == RichBlockKind::List
                && last.list_kind == list_kind
            {
                let idx = last.list_items.len();
                last.list_items.push(moved);
                idx
            } else {
                let mut nl = RichBlock::new(RichBlockKind::List);
                nl.list_kind = list_kind;
                nl.list_items.push(moved);
                parent_item.blocks.push(nl);
                0
            };
            let sub_block_idx = parent_item.blocks.len() - 1;
            let sub_path = BlockPath {
                container: block_path.list_item_children_container(item_idx - 1),
                index: sub_block_idx,
            };
            let target_leaf = LeafPath::list_item_text(sub_path, new_item_idx);
            s.rebuild_text_nodes();
            if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                s.cursor = EditorCursor::collapsed(ord, s.cursor.head);
            }
            ApplyResult::Changed
        })
    }

    /// Outdents (lifts) the active nested list item up into its parent list (or converts to paragraph).
    pub fn lift_active_list_item(&mut self) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            if desc.leaf.kind != LeafKind::ListItemText {
                return ApplyResult::Unchanged;
            }
            let Some(item_idx) = desc.leaf.list_item_index else {
                return ApplyResult::Failed;
            };
            let block_path = desc.leaf.block.clone();
            let Some(last_step) = block_path.container.steps.last().copied() else {
                // Already top-level list: convert item into a paragraph after list if last
                let Some(list) = resolve_block_mut(&mut s.page, &block_path) else {
                    return ApplyResult::Failed;
                };
                if item_idx >= list.list_items.len() {
                    return ApplyResult::Failed;
                }
                let item = list.list_items.remove(item_idx);
                let empty_after = list.list_items.is_empty();
                let mut para = make_paragraph_block();
                para.text = item.text;
                let container = block_path.container.clone();
                let Some(vec) = resolve_container_mut(&mut s.page, &container) else {
                    return ApplyResult::Failed;
                };
                let target_idx = if empty_after {
                    vec[block_path.index] = para;
                    block_path.index
                } else {
                    vec.insert(block_path.index + 1, para);
                    block_path.index + 1
                };
                let target_leaf = LeafPath::block_text(BlockPath {
                    container,
                    index: target_idx,
                });
                s.rebuild_text_nodes();
                if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                    s.cursor = EditorCursor::collapsed(ord, s.cursor.head);
                }
                return ApplyResult::Changed;
            };

            if let Some(parent_item_idx) = last_step.list_item_index {
                // Remove from nested sub-list and insert into parent list right after parent item
                let Some(sub_list) = resolve_block_mut(&mut s.page, &block_path) else {
                    return ApplyResult::Failed;
                };
                if item_idx >= sub_list.list_items.len() {
                    return ApplyResult::Failed;
                }
                let moved = sub_list.list_items.remove(item_idx);
                let sub_empty = sub_list.list_items.is_empty();
                if sub_empty
                    && let Some(sub_vec) = resolve_container_mut(&mut s.page, &block_path.container)
                {
                    sub_vec.remove(block_path.index);
                }
                let mut parent_container = block_path.container.clone();
                parent_container.steps.pop();
                let parent_list_path = BlockPath {
                    container: parent_container,
                    index: last_step.block_index,
                };
                let Some(parent_list) = resolve_block_mut(&mut s.page, &parent_list_path) else {
                    return ApplyResult::Failed;
                };
                let insert_pos = (parent_item_idx + 1).min(parent_list.list_items.len());
                parent_list.list_items.insert(insert_pos, moved);
                let target_leaf = LeafPath::list_item_text(parent_list_path, insert_pos);
                s.rebuild_text_nodes();
                if let Some(ord) = s.text_ordinal_for_leaf(&target_leaf) {
                    s.cursor = EditorCursor::collapsed(ord, s.cursor.head);
                }
                return ApplyResult::Changed;
            }
            ApplyResult::Unchanged
        })
    }

    /// Changes the style (`Bullet`, `Ordered`, `Task`) of the list at `block`.
    pub fn set_list_style(&mut self, block: &BlockPath, style: ListStyle) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::List {
                return ApplyResult::Failed;
            }
            match style {
                ListStyle::Ordered => {
                    blk.list_kind = RichListKind::Ordered;
                    for item in &mut blk.list_items {
                        item.task_state = RichTaskState::None;
                    }
                }
                ListStyle::Bullet => {
                    blk.list_kind = RichListKind::Bullet;
                    reset_non_ordered_list_metadata(blk);
                    for item in &mut blk.list_items {
                        item.task_state = RichTaskState::None;
                    }
                }
                ListStyle::Task => {
                    blk.list_kind = RichListKind::Bullet;
                    reset_non_ordered_list_metadata(blk);
                    for item in &mut blk.list_items {
                        if item.task_state == RichTaskState::None {
                            item.task_state = RichTaskState::Unchecked;
                        }
                    }
                }
            }
            ApplyResult::Changed
        })
    }

    /// Sets the marker type (`1`, `a`, `A`, `i`, `I`) of an ordered list.
    pub fn set_list_ordered_type(
        &mut self,
        block: &BlockPath,
        list_type: OrderedListType,
    ) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::List || blk.list_kind != RichListKind::Ordered {
                return ApplyResult::Failed;
            }
            let stored = stored_ordered_list_type(list_type, false);
            if blk.ordered_list.list_type == stored {
                return ApplyResult::Unchanged;
            }
            blk.ordered_list.list_type = stored;
            ApplyResult::Changed
        })
    }

    /// Sets the `reversed` flag on an ordered list.
    pub fn set_list_ordered_reversed(&mut self, block: &BlockPath, reversed: bool) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::List || blk.list_kind != RichListKind::Ordered {
                return ApplyResult::Failed;
            }
            if blk.ordered_list.reversed == reversed {
                return ApplyResult::Unchanged;
            }
            blk.ordered_list.reversed = reversed;
            ApplyResult::Changed
        })
    }

    /// Toggles `open` on a `<details>` block.
    pub fn toggle_details_open(&mut self, block: &BlockPath) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::Details {
                return ApplyResult::Failed;
            }
            blk.open = !blk.open;
            ApplyResult::Changed
        })
    }

    /// Toggles `collapsed` on a blockquote.
    pub fn toggle_quote_collapsed(&mut self, block: &BlockPath) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::Quote || blk.pullquote {
                return ApplyResult::Failed;
            }
            blk.collapsed = !blk.collapsed;
            ApplyResult::Changed
        })
    }

    /// Updates the language tag of a code block.
    pub fn set_code_block_language(&mut self, block: &BlockPath, language: String) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::Code {
                return ApplyResult::Failed;
            }
            if blk.language == language {
                return ApplyResult::Unchanged;
            }
            blk.language = language;
            ApplyResult::Changed
        })
    }

    /// Edits the TeX formula source of the active display math block.
    pub fn edit_active_display_math(&mut self, source: String) -> ApplyResult {
        self.apply_checked(|s| {
            let Some(desc) = s.text_nodes.get(s.cursor.leaf_ordinal).cloned() else {
                return ApplyResult::Failed;
            };
            if desc.leaf.kind != LeafKind::MathFormula {
                return ApplyResult::Failed;
            }
            let Some(blk) = resolve_block_mut(&mut s.page, &desc.leaf.block) else {
                return ApplyResult::Failed;
            };
            if blk.formula == source {
                return ApplyResult::Unchanged;
            }
            blk.formula = source;
            ApplyResult::Changed
        })
    }

    /// Toggles or sets the spoiler flag on a photo/video block.
    pub fn toggle_spoiler_on_block(
        &mut self,
        block: &BlockPath,
        enabled: Option<bool>,
    ) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if !matches!(blk.kind, RichBlockKind::Photo | RichBlockKind::Video) {
                return ApplyResult::Failed;
            }
            let next = enabled.unwrap_or(!blk.spoiler);
            if blk.spoiler == next {
                return ApplyResult::Unchanged;
            }
            blk.spoiler = next;
            ApplyResult::Changed
        })
    }

    // ========================================================================
    // Table Editing Commands
    // ========================================================================

    /// Inspects the table selection over `range`.
    #[must_use]
    pub fn table_selection_info(&self, range: &EditTableCellRange) -> TableSelectionInfo {
        let Some(blk) = resolve_block(&self.page, &range.block) else {
            return TableSelectionInfo::default();
        };
        inspect_table_selection(blk, range)
    }

    /// Adds a row before or after `range`.
    pub fn add_table_row(&mut self, range: &EditTableCellRange, after: bool) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            if add_table_row(blk, &range, after) {
                ApplyResult::Changed
            } else {
                ApplyResult::Failed
            }
        })
    }

    /// Adds a column before or after `range`.
    pub fn add_table_column(&mut self, range: &EditTableCellRange, after: bool) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            if add_table_column(blk, &range, after) {
                ApplyResult::Changed
            } else {
                ApplyResult::Failed
            }
        })
    }

    /// Removes the rows covered by `range`, preserving spanning cells.
    pub fn remove_table_rows(&mut self, range: &EditTableCellRange) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::Table
                || range.is_empty()
                || range.row_till > blk.table_rows.len()
            {
                return ApplyResult::Failed;
            }
            erase_table_rows_keeping_spans(
                blk,
                MarkdownPrepareLimits::IV_DEFAULT.table_render,
                range.row_from,
                range.row_till,
            );
            if blk.table_rows.is_empty()
                && let Some(vec) = resolve_container_mut(&mut s.page, &range.block.container)
            {
                vec.remove(range.block.index);
            }
            ApplyResult::Changed
        })
    }

    /// Sets `header` on all cells intersecting `range`.
    pub fn set_table_header(&mut self, range: &EditTableCellRange, header: bool) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            let grid = build_table_grid(blk);
            let cells = selected_table_grid_cells(&grid, &range);
            if cells.is_empty() {
                return ApplyResult::Failed;
            }
            let mut changed = false;
            for c in cells {
                if let Some(cell) = blk
                    .table_rows
                    .get_mut(c.row_index)
                    .and_then(|r| r.cells.get_mut(c.cell_index))
                    && cell.header != header
                {
                    cell.header = header;
                    changed = true;
                }
            }
            if changed {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    /// Sets horizontal alignment on all cells intersecting `range`.
    pub fn set_table_alignment(
        &mut self,
        range: &EditTableCellRange,
        alignment: RichTableAlignment,
    ) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            let grid = build_table_grid(blk);
            let cells = selected_table_grid_cells(&grid, &range);
            if cells.is_empty() {
                return ApplyResult::Failed;
            }
            let mut changed = false;
            for c in cells {
                if let Some(cell) = blk
                    .table_rows
                    .get_mut(c.row_index)
                    .and_then(|r| r.cells.get_mut(c.cell_index))
                    && cell.alignment != alignment
                {
                    cell.alignment = alignment;
                    changed = true;
                }
            }
            if changed {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    /// Sets vertical alignment on all cells intersecting `range`.
    pub fn set_table_vertical_alignment(
        &mut self,
        range: &EditTableCellRange,
        alignment: RichTableVerticalAlignment,
    ) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            let grid = build_table_grid(blk);
            let cells = selected_table_grid_cells(&grid, &range);
            if cells.is_empty() {
                return ApplyResult::Failed;
            }
            let mut changed = false;
            for c in cells {
                if let Some(cell) = blk
                    .table_rows
                    .get_mut(c.row_index)
                    .and_then(|r| r.cells.get_mut(c.cell_index))
                    && cell.vertical_alignment != alignment
                {
                    cell.vertical_alignment = alignment;
                    changed = true;
                }
            }
            if changed {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    /// Merges the rectangular cell range into a single spanning cell.
    pub fn unite_table_cells(&mut self, range: &EditTableCellRange) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            if unite_table_cells(blk, &range) {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    /// Splits a single merged cell back into 1x1 cells.
    pub fn split_table_cell(&mut self, range: &EditTableCellRange) -> ApplyResult {
        let range = range.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &range.block) else {
                return ApplyResult::Failed;
            };
            if split_table_cell(blk, &range) {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    /// Sets table style flags (`bordered`, `striped`, `compact`).
    pub fn set_table_style_flags(
        &mut self,
        block: &BlockPath,
        bordered: Option<bool>,
        striped: Option<bool>,
        compact: Option<bool>,
    ) -> ApplyResult {
        let block = block.clone();
        self.apply_checked(|s| {
            let Some(blk) = resolve_block_mut(&mut s.page, &block) else {
                return ApplyResult::Failed;
            };
            if blk.kind != RichBlockKind::Table {
                return ApplyResult::Failed;
            }
            let mut changed = false;
            if let Some(v) = bordered
                && blk.bordered != v
            {
                blk.bordered = v;
                changed = true;
            }
            if let Some(v) = striped
                && blk.striped != v
            {
                blk.striped = v;
                changed = true;
            }
            if let Some(v) = compact
                && blk.compact != v
            {
                blk.compact = v;
                changed = true;
            }
            if changed {
                ApplyResult::Changed
            } else {
                ApplyResult::Unchanged
            }
        })
    }

    // ========================================================================
    // Serialization / Export Conveniences
    // ========================================================================

    /// Serializes the current document to canonical Markdown.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        rich_page_to_markdown(&self.page)
    }

    /// Serializes the current document to Layer-233 TL `InputRichMessage`.
    #[must_use]
    pub fn to_input_rich_message(
        &self,
        mode: SerializeRichMessageMode,
        resolver: &dyn RichMediaResolver,
    ) -> SerializeRichMessageResult {
        serialize_input_rich_message(&self.page, mode, resolver)
    }

    /// Serializes the current document to Layer-233 TL `PageBlock`s.
    pub fn to_tl_blocks(
        &self,
        mode: SerializeRichMessageMode,
        resolver: &dyn RichMediaResolver,
    ) -> Result<Vec<tl_enums::PageBlock>, SerializeRichMessageStatus> {
        rich_page_to_tl_blocks(&self.page, mode, resolver)
    }

    /// Converts the current document to [`Page`] for Instant View HTML preview.
    #[must_use]
    pub fn to_model_page(&self) -> Page {
        rich_page_to_model_page(&self.page)
    }

    // ========================================================================
    // Internal Leaf Indexing & Accessors
    // ========================================================================

    fn rebuild_text_nodes(&mut self) {
        self.text_nodes.clear();
        let blocks = self.page.blocks.clone();
        self.collect_text_nodes(&blocks, &BlockContainerPath::root());
    }

    fn collect_text_nodes(&mut self, blocks: &[RichBlock], container: &BlockContainerPath) {
        for (idx, block) in blocks.iter().enumerate() {
            let bp = BlockPath {
                container: container.clone(),
                index: idx,
            };
            match block.kind {
                RichBlockKind::Paragraph | RichBlockKind::Heading | RichBlockKind::Footer => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_text(bp),
                        mode: FieldMode::Rich,
                        block_kind: block.kind,
                        heading_level: block.heading_level,
                    });
                }
                RichBlockKind::Code => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_text(bp),
                        mode: FieldMode::Raw,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                }
                RichBlockKind::Math => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::math_formula(bp),
                        mode: FieldMode::Raw,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                }
                RichBlockKind::Quote => {
                    if block.blocks.is_empty() || !block.text.is_structurally_empty() {
                        self.text_nodes.push(TextNodeDescriptor {
                            leaf: LeafPath::block_text(bp.clone()),
                            mode: FieldMode::Rich,
                            block_kind: block.kind,
                            heading_level: 0,
                        });
                    }
                    if !block.blocks.is_empty() {
                        self.collect_text_nodes(&block.blocks, &bp.children_container());
                    }
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_caption(bp),
                        mode: FieldMode::Rich,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                }
                RichBlockKind::Details => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_text(bp.clone()),
                        mode: FieldMode::Rich,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                    self.collect_text_nodes(&block.blocks, &bp.children_container());
                }
                RichBlockKind::List => {
                    for (item_idx, item) in block.list_items.iter().enumerate() {
                        if item.blocks.is_empty() || !item.text.is_structurally_empty() {
                            self.text_nodes.push(TextNodeDescriptor {
                                leaf: LeafPath::list_item_text(bp.clone(), item_idx),
                                mode: FieldMode::Rich,
                                block_kind: block.kind,
                                heading_level: 0,
                            });
                        }
                        if !item.blocks.is_empty() {
                            self.collect_text_nodes(
                                &item.blocks,
                                &bp.list_item_children_container(item_idx),
                            );
                        }
                    }
                }
                RichBlockKind::Table => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_text(bp.clone()),
                        mode: FieldMode::Rich,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                    for (r_idx, row) in block.table_rows.iter().enumerate() {
                        for c_idx in 0..row.cells.len() {
                            self.text_nodes.push(TextNodeDescriptor {
                                leaf: LeafPath::table_cell_text(bp.clone(), r_idx, c_idx),
                                mode: FieldMode::Rich,
                                block_kind: block.kind,
                                heading_level: 0,
                            });
                        }
                    }
                }
                RichBlockKind::Photo
                | RichBlockKind::Video
                | RichBlockKind::Audio
                | RichBlockKind::File
                | RichBlockKind::Map
                | RichBlockKind::GroupedMedia => {
                    self.text_nodes.push(TextNodeDescriptor {
                        leaf: LeafPath::block_caption(bp),
                        mode: FieldMode::Rich,
                        block_kind: block.kind,
                        heading_level: 0,
                    });
                }
                _ => {}
            }
        }
        // Silence unused helper warning if list_item_is_empty is checked by callers
        let _ = list_item_is_empty;
        let _ = resolve_container;
    }

    fn first_leaf_ordinal_in_block(&self, block: &BlockPath) -> Option<usize> {
        self.text_nodes.iter().position(|d| &d.leaf.block == block)
    }

    fn clamp_cursor(&mut self) {
        if self.text_nodes.is_empty() {
            self.cursor = EditorCursor::default();
            return;
        }
        if self.cursor.leaf_ordinal >= self.text_nodes.len() {
            self.cursor.leaf_ordinal = self.text_nodes.len() - 1;
        }
        let max_len = self
            .leaf_text(&self.text_nodes[self.cursor.leaf_ordinal].leaf)
            .utf16_len();
        self.cursor.anchor = self.cursor.anchor.min(max_len);
        self.cursor.head = self.cursor.head.min(max_len);
    }

    /// Returns the rich text (or raw text wrapped as plain) of any leaf addressed by `path`.
    #[must_use]
    pub fn leaf_text(&self, path: &LeafPath) -> TextWithEntities {
        let Some(block) = resolve_block(&self.page, &path.block) else {
            return TextWithEntities::default();
        };
        match path.kind {
            LeafKind::BlockText => block.text.text.clone(),
            LeafKind::BlockCaption => block.caption.text.clone(),
            LeafKind::MathFormula => TextWithEntities::simple(block.formula.clone()),
            LeafKind::ListItemText => path
                .list_item_index
                .and_then(|i| block.list_items.get(i))
                .map(|i| i.text.text.clone())
                .unwrap_or_default(),
            LeafKind::TableCellText => path
                .table_row_index
                .zip(path.table_cell_index)
                .and_then(|(r, c)| block.table_rows.get(r)?.cells.get(c))
                .map(|c| c.text.text.clone())
                .unwrap_or_default(),
        }
    }

    fn set_leaf_text(&mut self, path: &LeafPath, text: TextWithEntities) -> bool {
        match path.kind {
            LeafKind::BlockText => {
                let Some(blk) = resolve_block_mut(&mut self.page, &path.block) else {
                    return false;
                };
                blk.text.text = text;
                true
            }
            LeafKind::BlockCaption => {
                let Some(blk) = resolve_block_mut(&mut self.page, &path.block) else {
                    return false;
                };
                blk.caption.text = text;
                true
            }
            LeafKind::MathFormula => {
                let Some(blk) = resolve_block_mut(&mut self.page, &path.block) else {
                    return false;
                };
                blk.formula = text.text;
                true
            }
            LeafKind::ListItemText => {
                let Some(idx) = path.list_item_index else {
                    return false;
                };
                let Some(item) = resolve_list_item_mut(&mut self.page, &path.block, idx) else {
                    return false;
                };
                item.text.text = text;
                true
            }
            LeafKind::TableCellText => {
                let Some((r, c)) = path.table_row_index.zip(path.table_cell_index) else {
                    return false;
                };
                let Some(cell) = resolve_table_cell_mut(&mut self.page, &path.block, r, c) else {
                    return false;
                };
                cell.text.text = text;
                true
            }
        }
    }
}
