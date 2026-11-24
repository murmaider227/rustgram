//! Headless rich document editor model (`Telegram/SourceFiles/iv/editor/*`).
//!
//! Zero UI dependency — designed to be driven by an `egui` rich compose/edit window
//! (`crates/tg-app/src/webview/rich_editor.rs`).
//!
//! - [`path`]: Structural container, block, leaf, and selection paths
//! - [`blocks`]: Block constructors, list metadata helpers, editability & sanitization rules
//! - [`table`]: 2D table occupancy grid, span-preserving row removal, cell merge/split
//! - [`auto_pair`]: Bracket/quote auto-pairing and backspace pair deletion
//! - [`text_ops`]: UTF-16 entity splicing, inline formatting toggles, inline formula creation
//! - [`state`]: [`EditorState`] document model with undo/redo, cursor/selection, and toolbar state
//! - [`clipboard_import`]: HTML/Markdown/TSV clipboard sanitizer into IV blocks (`iv_editor_clipboard_import.cpp`)
//! - [`math`]: Inline/display LaTeX formula validation & live preview state (`iv_editor_math_box.cpp`)

pub mod auto_pair;
pub mod blocks;
pub mod clipboard_import;
pub mod math;
pub mod path;
pub mod state;
pub mod table;
pub mod text_ops;

pub use auto_pair::{AutoPairOutcome, handle_auto_pair_backspace, handle_auto_pair_char};
pub use blocks::{
    InsertAction, InsertBlockType, ListStyle, OrderedListType, block_can_own_child_container,
    block_conversion_expands_to_active_line, block_is_empty, block_supports_block_caption,
    block_supports_block_text, can_edit_block, can_edit_blocks, can_edit_rich_page,
    current_list_style, degrade_block_only_entities, degrade_edit_mode_buttons,
    effective_ordered_item_value, is_task_list, joinable_text_block_kind, list_item_is_empty,
    lists_join_seamlessly, make_anchor_block, make_block_for_action, make_code_block,
    make_details_block, make_divider_block, make_footer_block, make_heading_block, make_list_block,
    make_map_block, make_math_block, make_paragraph_block, make_quote_block, make_table_block,
    merge_rich_text_anchors, ordered_items_support_roman, reset_non_ordered_list_metadata,
    resolve_ordered_list_type, split_task_state, stored_ordered_list_type,
    strip_edit_mode_wrapper_entities, strip_edit_mode_wrapper_entities_in_blocks,
};
pub use clipboard_import::{
    BlocksImportResult, ClipboardPayload, IMPORTED_MEDIA_PLACEHOLDER_BASE,
    IMPORTED_MEDIA_PLACEHOLDER_LIMIT, ImportedLocalMedia, MAX_CELL_LENGTH,
    MAX_DATA_URI_HEADER_LENGTH, MAX_DATA_URI_LENGTH, MAX_MARKDOWN_IMPORT_LENGTH,
    MAX_MARKDOWN_QUOTE_DEPTH, TableImportLimits, TableImportResult, blocks_from_html,
    blocks_from_markdown, can_paste_without_confirmation, data_uri_image_content,
    decode_html_entities, import_from_clipboard, imported_media_paths_to_confirm,
    imported_media_placeholder_id, imported_media_placeholder_index, is_network_path,
    looks_like_exported_html, looks_like_media_extension, resolve_local_media,
    rich_blocks_carry_structure, table_from_delimited_text, table_import_limits_for,
    text_has_markdown_structure,
};
pub use math::{
    DEFAULT_DISPLAY_TEXT_SIZE, DEFAULT_INLINE_TEXT_SIZE, DEFAULT_PREVIEW_MIN_HEIGHT,
    DEFAULT_PREVIEW_PADDING_X, DEFAULT_PREVIEW_PADDING_Y, DEFAULT_RENDER_HEIGHT_CAP,
    DEFAULT_RENDER_WIDTH_CAP, FORMULA_SAMPLES, MathBoxState, MathPreviewState, MathSubmission,
    MathValidationError, normalize_math_source, strip_math_delimiters, validate_math_formula,
};
pub use path::{
    BlockContainerKind, BlockContainerPath, BlockContainerStep, BlockPath, EditBlockRange,
    EditListItemRange, EditSelection, EditTableCellRange, EditTableRowRange, LeafKind, LeafPath,
    ReplaceTarget, resolve_block, resolve_block_mut, resolve_container, resolve_container_mut,
    resolve_list_item, resolve_list_item_mut, resolve_table_cell, resolve_table_cell_mut,
};
pub use state::{
    ApplyResult, DEFAULT_MAX_UNDO_ENTRIES, EditorCursor, EditorSnapshot, EditorState, FieldMode,
    TextNodeDescriptor, ToolbarActionState, ToolbarState,
};
pub use table::{
    TableGrid, TableGridCellRef, TableSelectionInfo, add_table_column, add_table_row,
    build_table_grid, build_table_grid_limited, can_occupy_table_slots, cell_intersects_range,
    clamp_table_colspan, clamp_table_rowspan, erase_table_rows_keeping_spans,
    first_available_table_column, inspect_table_selection, mark_table_slots, normalize_table_span,
    selected_table_grid_cells, split_table_cell, table_max_columns, unite_table_cells,
};
pub use text_ops::{
    MAX_COMMITTED_FIELD_LENGTH, MAX_RICH_TEXT_NODE_LENGTH, TextFormattingAction,
    adjust_entity_for_replacement, apply_formatting_to_range, formula_source_to_rich_text,
    range_has_entity, replace_plain_range, replace_rich_range, split_text_span,
};
