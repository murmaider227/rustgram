//! Rich document editor window (`egui` port of `Telegram/SourceFiles/iv/editor/*`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/iv/editor/iv_editor_window.{h,cpp}`
//! - `Telegram/SourceFiles/iv/editor/iv_editor_widget.{h,cpp}`
//! - `Telegram/SourceFiles/iv/editor/iv_editor_toolbar_pill.{h,cpp}`
//! - `Telegram/SourceFiles/iv/editor/iv_editor_structure_menu.{h,cpp}`
//! - `Telegram/SourceFiles/iv/editor/iv_editor_math_box.{h,cpp}`
//!
//! Drives [`tg_iv::editor::EditorState`] directly:
//! - renders the full [`RichPage`] block and leaf tree with UTF-16 ↔ byte-offset caret & selection
//!   mapping via [`tg_iv::rich_page::utf16_to_byte_offset`] and
//!   [`tg_iv::rich_page::byte_to_utf16_offset`];
//! - routes keyboard input through `handle_auto_pair_char`, `handle_auto_pair_backspace`,
//!   `delete_backward`, `delete_forward`, `handle_enter`, and cursor moves;
//! - builds the formatting toolbar from [`EditorState::toolbar_state`] plus Insert / Convert block
//!   menus and contextual Table editing commands;
//! - displays limit-error toasts from [`EditorState::last_limit_error`];
//! - provides Markdown import/export and a live Preview tab powered by
//!   [`crate::webview::rich_page_view`] and [`crate::webview::formula_widget`];
//! - submits via [`EditorState::to_input_rich_message`] with
//!   [`SerializeRichMessageMode::FinalSubmit`] and [`InMemoryMediaResolver`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Id, Key, Pos2, Rect, RichText, Role,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, WidgetInfo,
};
use tg_iv::editor::{
    ApplyResult, BlockContainerPath, BlockPath, ClipboardPayload, EditTableCellRange, EditorState,
    FieldMode, InsertAction, InsertBlockType, LeafKind, LeafPath, ListStyle, MathBoxState,
    OrderedListType, TextFormattingAction, ToolbarActionState, import_from_clipboard,
    resolve_block, rich_blocks_carry_structure, text_has_markdown_structure,
};
use tg_iv::markdown::{MathKind, MathLayoutEngine};
use tg_iv::rich_page::{
    InMemoryMediaResolver, RichBlock, RichBlockKind, RichListKind, RichMessageLimitError,
    RichMessageLimits, RichPage, RichTableAlignment, RichTableVerticalAlignment, RichTaskState,
    SerializeRichMessageMode, SerializeRichMessageStatus, TextWithEntities, byte_to_utf16_offset,
    utf16_to_byte_offset,
};
use tg_iv::{
    DiskExportResult, HtmlExportOptions, HtmlExportResult, MediaProvider, export_rich_page_html,
    export_rich_page_to_dir, rich_blocks_clipboard_html,
};
use tg_lang::tr;
use tg_tl::types as tl_types;
use tg_ui::tg_style::st;

use crate::webview::formula_widget::{
    FormulaWidgetStyle, layout_formula_for_style, paint_formula_at_baseline,
};
use crate::webview::rich_page_view::{
    RichPageViewStyle, build_rich_leaf_layout_job, format_ordered_marker, has_inline_objects,
    heading_font_size, paint_inline_rich_text, paint_rich_page,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Active tab inside [`RichEditorWindow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RichEditorTab {
    /// Interactive WYSIWYG block/leaf editor.
    #[default]
    Edit,
    /// Markdown source import/export view.
    Markdown,
    /// Live rendered rich-message preview (blocks + formulas).
    Preview,
}

/// Action emitted by [`RichEditorWindow`] when the user submits or cancels.
#[derive(Debug, Clone)]
pub enum RichEditorAction {
    /// Submit serialized Layer-233 `InputRichMessage` and its [`RichPage`] (new or edited).
    Submit {
        /// Destination peer id.
        peer_id: PeerId,
        /// Existing message id when editing, or `None` when composing a new rich message.
        edit_msg_id: Option<MsgId>,
        /// Serialized TL `InputRichMessage` produced by `SerializeRichMessageMode::FinalSubmit`.
        input: tl_types::InputRichMessage,
        /// Final rich document page for immediate display/storage.
        page: RichPage,
    },
    /// Editor closed without sending.
    Cancel,
}

/// Abstraction over the native OS folder chooser for `"Save as HTML"` (`rfd::FileDialog`).
///
/// Production app startup injects [`NativeExportFolderDialog`]; headless tests inject a fake
/// implementation returning a temp directory or `None` (cancelled).
pub trait ExportFolderDialog: Send + Sync {
    /// Prompts the user to pick a target folder, optionally starting in `initial_dir`.
    fn pick_folder(&self, initial_dir: Option<&Path>) -> Option<PathBuf>;
}

/// Native OS folder chooser backed by [`rfd::FileDialog`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeExportFolderDialog;

impl ExportFolderDialog for NativeExportFolderDialog {
    fn pick_folder(&self, initial_dir: Option<&Path>) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = initial_dir
            && dir.is_dir()
        {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_folder()
    }
}

/// Interactive `egui` rich document editor window backed by [`EditorState`].
pub struct RichEditorWindow {
    peer_id: PeerId,
    edit_msg_id: Option<MsgId>,
    state: EditorState,
    media_resolver: InMemoryMediaResolver,
    math_engine: MathLayoutEngine,
    tab: RichEditorTab,
    markdown_buffer: String,
    markdown_dirty: bool,
    // Inline prompt states (link URL / MathBoxState formula modal)
    link_prompt_open: bool,
    link_url_input: String,
    math_box: Option<MathBoxState>,
    // Optional target directory, injectable folder dialog & last result for `tg_iv::html_export`
    export_dir: Option<PathBuf>,
    export_dialog: Option<Arc<dyn ExportFolderDialog>>,
    last_html_export: Option<DiskExportResult>,
    // Table cell multi-selection range when working inside a table
    table_selection: Option<EditTableCellRange>,
    // Ephemeral toast text + expiration timestamp (seconds)
    toast: Option<(String, f64)>,
    // Whether the document canvas currently has logical keyboard focus
    canvas_focused: bool,
    open: bool,
}

impl std::fmt::Debug for RichEditorWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RichEditorWindow")
            .field("peer_id", &self.peer_id)
            .field("edit_msg_id", &self.edit_msg_id)
            .field("tab", &self.tab)
            .field("open", &self.open)
            .finish()
    }
}

impl RichEditorWindow {
    /// Opens a blank rich document editor for composing a new rich message in `peer_id`.
    #[must_use]
    pub fn new_compose(peer_id: PeerId) -> Self {
        Self::from_editor_state(peer_id, None, EditorState::new())
    }

    /// Opens the rich document editor pre-populated with an existing message's [`RichPage`].
    #[must_use]
    pub fn new_edit(peer_id: PeerId, msg_id: MsgId, page: RichPage) -> Self {
        let state = EditorState::from_rich_page(page, RichMessageLimits::default());
        Self::from_editor_state(peer_id, Some(msg_id), state)
    }

    /// Creates an editor window wrapping an explicit [`EditorState`] (useful for tests/custom limits).
    #[must_use]
    pub fn from_editor_state(
        peer_id: PeerId,
        edit_msg_id: Option<MsgId>,
        state: EditorState,
    ) -> Self {
        let markdown_buffer = state.to_markdown();
        Self {
            peer_id,
            edit_msg_id,
            state,
            media_resolver: InMemoryMediaResolver::default(),
            math_engine: MathLayoutEngine::default(),
            tab: RichEditorTab::Edit,
            markdown_buffer,
            markdown_dirty: false,
            link_prompt_open: false,
            link_url_input: String::new(),
            math_box: None,
            export_dir: None,
            export_dialog: None,
            last_html_export: None,
            table_selection: None,
            toast: None,
            canvas_focused: true,
            open: true,
        }
    }

    /// Returns immutable access to the headless [`EditorState`].
    #[must_use]
    pub const fn state(&self) -> &EditorState {
        &self.state
    }

    /// Returns mutable access to the headless [`EditorState`].
    pub fn state_mut(&mut self) -> &mut EditorState {
        &mut self.state
    }

    /// Returns mutable access to the [`InMemoryMediaResolver`] used on submit.
    pub fn media_resolver_mut(&mut self) -> &mut InMemoryMediaResolver {
        &mut self.media_resolver
    }

    /// Returns the active [`RichEditorTab`].
    #[must_use]
    pub const fn active_tab(&self) -> RichEditorTab {
        self.tab
    }

    /// Switches the active tab (`Edit` / `Markdown` / `Preview`).
    pub fn set_tab(&mut self, tab: RichEditorTab) {
        if tab == RichEditorTab::Markdown && !self.markdown_dirty {
            self.markdown_buffer = self.state.to_markdown();
        }
        self.tab = tab;
    }

    /// Returns whether the editor window is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns the currently displayed toast message, if any.
    #[must_use]
    pub fn active_toast(&self) -> Option<&str> {
        self.toast.as_ref().map(|(msg, _)| msg.as_str())
    }

    /// Returns the target peer id.
    #[must_use]
    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Returns the message id being edited, if in edit mode.
    #[must_use]
    pub const fn edit_msg_id(&self) -> Option<MsgId> {
        self.edit_msg_id
    }

    /// Returns the currently open [`MathBoxState`], if any.
    #[must_use]
    pub const fn math_box(&self) -> Option<&MathBoxState> {
        self.math_box.as_ref()
    }

    /// Opens the formula editor panel backed by [`MathBoxState`].
    pub fn open_math_box(
        &mut self,
        start_source: &str,
        editing_existing: bool,
        separate_line: Option<bool>,
    ) {
        self.link_prompt_open = false;
        self.math_box = Some(MathBoxState::new(
            start_source,
            editing_existing,
            separate_line,
            340,
        ));
    }

    /// Closes the formula editor panel, restoring canvas focus.
    pub fn close_math_box(&mut self) {
        if let Some(ref mut mb) = self.math_box {
            mb.close();
        }
        self.math_box = None;
        self.canvas_focused = true;
    }

    /// Sets the default / initial target directory for `"Save as HTML"`.
    pub fn set_export_dir(&mut self, dir: Option<PathBuf>) {
        self.export_dir = dir;
    }

    /// Returns the configured default / initial HTML export directory, if any.
    #[must_use]
    pub fn export_dir(&self) -> Option<&Path> {
        self.export_dir.as_deref()
    }

    /// Installs an injectable folder chooser dialog for `"Save as HTML"` (e.g.
    /// [`NativeExportFolderDialog`] at app startup or a fake dialog in tests).
    pub fn set_export_dialog(&mut self, dialog: Option<Arc<dyn ExportFolderDialog>>) {
        self.export_dialog = dialog;
    }

    /// Returns metadata from the most recent [`Self::export_html_to_dir`] call.
    #[must_use]
    pub const fn last_html_export(&self) -> Option<&DiskExportResult> {
        self.last_html_export.as_ref()
    }

    fn html_export_options(&self) -> HtmlExportOptions {
        HtmlExportOptions {
            missing_media_label: tr::lng_export_html_media_missing(),
            embed_fallback_label: tr::lng_export_html_embed(),
            ..HtmlExportOptions::default()
        }
    }

    /// Exports the current rich document to a self-contained, XSS-sanitized HTML5 string via
    /// [`tg_iv::export_rich_page_html`].
    #[must_use]
    pub fn export_html(&self) -> HtmlExportResult {
        export_rich_page_html(self.state.rich_page(), &self.html_export_options())
    }

    /// Generates clipboard-ready HTML with optional inline base64 media data URIs via
    /// [`tg_iv::rich_blocks_clipboard_html`].
    #[must_use]
    pub fn copy_clipboard_html<M: MediaProvider>(&self, media: Option<&M>) -> String {
        let page = self.state.rich_page();
        rich_blocks_clipboard_html(&page.blocks, page.rtl, media)
    }

    /// Exports the current rich document and any available media blobs to a subfolder under
    /// `base_dir` via [`tg_iv::export_rich_page_to_dir`].
    pub fn export_html_to_dir<M: MediaProvider>(
        &mut self,
        base_dir: &Path,
        media: Option<&M>,
    ) -> std::io::Result<DiskExportResult> {
        let opts = self.html_export_options();
        let result = export_rich_page_to_dir(self.state.rich_page(), base_dir, media, &opts)?;
        self.last_html_export = Some(result.clone());
        Ok(result)
    }

    /// Imports sanitized HTML, Markdown, or spreadsheet TSV clipboard content into the document
    /// via [`tg_iv::editor::import_from_clipboard`].
    ///
    /// Returns `true` when structured blocks or rich formatted HTML were imported.
    pub fn paste_clipboard_payload(&mut self, payload: ClipboardPayload, now_secs: f64) -> bool {
        let limits = *self.state.limits();
        let used_blocks = self.state.rich_page().blocks.len();
        let Some(imported) = import_from_clipboard(&payload, &limits, used_blocks) else {
            return false;
        };
        if imported.blocks.is_empty() {
            return false;
        }

        // Single formatted paragraph without nested blocks -> paste inline to keep undo/cursor intact.
        if imported.blocks.len() == 1
            && imported.blocks[0].kind == RichBlockKind::Paragraph
            && imported.blocks[0].blocks.is_empty()
            && !rich_blocks_carry_structure(&imported.blocks)
        {
            let res = self
                .state
                .replace_selection(imported.blocks[0].text.text.clone());
            self.check_limit_error(res, now_secs);
            self.markdown_dirty = false;
            return res == ApplyResult::Changed;
        }

        // Multi-block or structural paste -> splice imported blocks at the active top-level slot.
        let mut page = self.state.rich_page().clone();
        let active_top = self
            .state
            .active_leaf_path()
            .map(|leaf| top_level_block_index(&leaf.block))
            .unwrap_or_else(|| page.blocks.len().saturating_sub(1));
        let replace_empty_active = page.blocks.get(active_top).is_some_and(|b| {
            b.kind == RichBlockKind::Paragraph
                && b.text.is_structurally_empty()
                && b.blocks.is_empty()
        });
        let imported_count = imported.blocks.len();
        if replace_empty_active {
            page.blocks.remove(active_top);
            for (offset, blk) in imported.blocks.into_iter().enumerate() {
                page.blocks.insert(active_top + offset, blk);
            }
        } else {
            let insert_at = (active_top + 1).min(page.blocks.len());
            for (offset, blk) in imported.blocks.into_iter().enumerate() {
                page.blocks.insert(insert_at + offset, blk);
            }
        }

        self.state = EditorState::from_rich_page(page, limits);
        self.markdown_dirty = false;
        self.table_selection = None;
        let suffix = if imported.truncated {
            " (truncated to limits)"
        } else {
            ""
        };
        self.show_toast(
            format!("Imported {imported_count} block(s) from clipboard{suffix}."),
            now_secs,
        );
        true
    }

    /// Returns the active table cell range if the cursor is inside a table cell.
    #[must_use]
    pub fn active_table_range(&self) -> Option<EditTableCellRange> {
        if let Some(ref sel) = self.table_selection
            && !sel.is_empty()
            && resolve_block(self.state.rich_page(), &sel.block)
                .is_some_and(|b| b.kind == RichBlockKind::Table)
        {
            return Some(sel.clone());
        }
        let leaf = self.state.active_leaf_path()?;
        if leaf.kind == LeafKind::TableCellText
            && let (Some(r), Some(c)) = (leaf.table_row_index, leaf.table_cell_index)
        {
            return Some(EditTableCellRange {
                block: leaf.block.clone(),
                row_from: r,
                row_till: r + 1,
                column_from: c,
                column_till: c + 1,
            });
        }
        None
    }

    /// Formats a user-facing message for a [`RichMessageLimitError`].
    #[must_use]
    pub fn format_limit_error(&self, err: RichMessageLimitError) -> String {
        let limits = self.state.limits();
        match err {
            RichMessageLimitError::Length => format!(
                "Rich message text limit reached ({} characters).",
                limits.length_limit
            ),
            RichMessageLimitError::Blocks => {
                format!(
                    "Maximum block limit reached ({} blocks).",
                    limits.max_blocks
                )
            }
            RichMessageLimitError::Depth => {
                format!(
                    "Maximum nesting depth reached ({} levels).",
                    limits.max_depth
                )
            }
            RichMessageLimitError::Media => {
                format!("Maximum media items reached ({} items).", limits.max_media)
            }
            RichMessageLimitError::TableColumns => format!(
                "Maximum table columns reached ({} columns).",
                limits.max_table_cols
            ),
            RichMessageLimitError::Buttons => format!(
                "Maximum buttons per row reached ({} buttons).",
                limits.max_buttons
            ),
        }
    }

    fn show_toast(&mut self, msg: impl Into<String>, now_secs: f64) {
        self.toast = Some((msg.into(), now_secs + 3.5));
    }

    fn check_limit_error(&mut self, res: ApplyResult, now_secs: f64) {
        if res == ApplyResult::Failed
            && let Some(err) = self.state.last_limit_error()
        {
            let text = self.format_limit_error(err);
            self.show_toast(text, now_secs);
        }
    }

    /// Renders the editor as a floating `egui::Window` overlay (used inside `HistoryWidget`).
    pub fn show_window(&mut self, ctx: &egui::Context, is_night: bool) -> Option<RichEditorAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let mut still_open = true;
        let title = if self.edit_msg_id.is_some() {
            "Edit Rich Message"
        } else {
            "Rich Document Editor"
        };
        let vp = ctx.viewport_rect();
        let default_w = (vp.width() * 0.78).clamp(520.0, 860.0);
        let default_h = (vp.height() * 0.80).clamp(420.0, 680.0);

        egui::Window::new(title)
            .id(Id::unique("tg_rich_editor_window"))
            .open(&mut still_open)
            .resizable(true)
            .collapsible(false)
            .default_size(Vec2::new(default_w, default_h))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                action = self.show_contents(ui, is_night);
            });

        if !still_open {
            self.open = false;
            return Some(RichEditorAction::Cancel);
        }
        if action.is_some() {
            self.open = false;
        }
        action
    }

    /// Renders the complete rich document editor UI directly inside `ui`.
    ///
    /// Callable both from [`Self::show_window`] and directly from `egui_kittest` harnesses.
    pub fn show_contents(&mut self, ui: &mut Ui, is_night: bool) -> Option<RichEditorAction> {
        let now_secs = ui.ctx().input(|i| i.time);
        if let Some((_, expires)) = &self.toast
            && now_secs >= *expires
        {
            self.toast = None;
        }

        let style = if is_night {
            RichPageViewStyle::night()
        } else {
            RichPageViewStyle::day()
        };
        let mut action: Option<RichEditorAction> = None;

        // 1. Header tabs (Edit / Markdown / Preview) + document stats.
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.tab == RichEditorTab::Edit, "Edit")
                .clicked()
            {
                self.set_tab(RichEditorTab::Edit);
            }
            if ui
                .selectable_label(self.tab == RichEditorTab::Markdown, "Markdown")
                .clicked()
            {
                self.set_tab(RichEditorTab::Markdown);
            }
            if ui
                .selectable_label(self.tab == RichEditorTab::Preview, "Preview")
                .clicked()
            {
                self.set_tab(RichEditorTab::Preview);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let blocks_n = self.state.rich_page().blocks.len();
                let nodes_n = self.state.text_node_count();
                ui.label(
                    RichText::new(format!("{blocks_n} blocks · {nodes_n} leaves"))
                        .size(11.5)
                        .color(style.muted_fg),
                );
            });
        });

        ui.separator();

        // 2. Formatting toolbar + Insert / Convert menus (Edit tab).
        if self.tab == RichEditorTab::Edit {
            self.render_toolbar(ui, &style, now_secs);
            if self.link_prompt_open {
                self.render_link_prompt(ui, &style, now_secs);
            }
            if self.math_box.is_some() {
                self.render_math_prompt(ui, &style, now_secs);
            }
            if let Some(table_range) = self.active_table_range() {
                self.render_table_toolbar(ui, &table_range, &style, now_secs);
            }
            ui.separator();
        }

        // 3. Limit-error or status toast banner.
        if let Some((ref msg, _)) = self.toast {
            egui::Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(212, 68, 68, 36))
                .stroke(Stroke::new(1.0, Color32::from_rgb(212, 68, 68)))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new(msg).size(12.5).color(if is_night {
                        Color32::from_rgb(255, 175, 175)
                    } else {
                        Color32::from_rgb(160, 28, 28)
                    }));
                });
            ui.add_space(4.0);
        }

        // 4. Main body area (reserve space for bottom footer buttons).
        let footer_h = 40.0;
        let body_h = (ui.available_height() - footer_h).max(160.0);

        match self.tab {
            RichEditorTab::Edit => {
                // Process keyboard input when canvas has focus and no modal input prompt is open.
                let prompt_active = self.link_prompt_open || self.math_box.is_some();
                if self.canvas_focused && !prompt_active && !ui.ctx().egui_wants_keyboard_input() {
                    self.handle_keyboard_input(ui.ctx(), now_secs);
                }

                ScrollArea::vertical()
                    .id_salt("tg_rich_editor_canvas_scroll")
                    .max_height(body_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let avail_w = ui.available_width().max(200.0);
                        ui.set_min_height((body_h - 12.0).max(120.0));
                        self.render_editor_blocks(
                            ui,
                            &BlockContainerPath::root(),
                            avail_w,
                            &style,
                            0,
                            now_secs,
                        );
                    });
            }
            RichEditorTab::Markdown => {
                self.render_markdown_tab(ui, body_h, &style, now_secs);
            }
            RichEditorTab::Preview => {
                ScrollArea::vertical()
                    .id_salt("tg_rich_editor_preview_scroll")
                    .max_height(body_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let avail_w = ui.available_width().max(200.0);
                        let page = self.state.rich_page().clone();
                        let _ = paint_rich_page(
                            ui,
                            "tg_rich_editor_preview",
                            &mut self.math_engine,
                            &page,
                            avail_w,
                            &style,
                        );
                    });
            }
        }

        ui.separator();

        // 5. Footer action row: Cancel + Submit (`to_input_rich_message(FinalSubmit, &resolver)`).
        ui.horizontal(|ui| {
            let submit_label = if self.edit_msg_id.is_some() {
                "Save Changes"
            } else {
                "Send Rich Message"
            };
            if ui.button(submit_label).clicked()
                && let Some(sub) = self.try_submit(now_secs)
            {
                action = Some(sub);
            }
            if ui.button("Cancel").clicked() {
                action = Some(RichEditorAction::Cancel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cursor = self.state.cursor();
                let mode_str = match self.state.active_field_mode() {
                    FieldMode::Rich => "Rich",
                    FieldMode::Raw => "Raw",
                };
                ui.label(
                    RichText::new(format!(
                        "Leaf {} · UTF-16 {}..{} · {}",
                        cursor.leaf_ordinal, cursor.anchor, cursor.head, mode_str
                    ))
                    .size(11.0)
                    .color(style.muted_fg),
                );
            });
        });

        action
    }

    fn try_submit(&mut self, now_secs: f64) -> Option<RichEditorAction> {
        if let Some(limit_err) = self.state.validate() {
            let msg = self.format_limit_error(limit_err);
            self.show_toast(msg, now_secs);
            return None;
        }
        let serialized = self
            .state
            .to_input_rich_message(SerializeRichMessageMode::FinalSubmit, &self.media_resolver);
        match (serialized.status, serialized.value) {
            (SerializeRichMessageStatus::Success, Some(input)) => Some(RichEditorAction::Submit {
                peer_id: self.peer_id,
                edit_msg_id: self.edit_msg_id,
                input,
                page: self.state.rich_page().clone(),
            }),
            (SerializeRichMessageStatus::EmptyContent, _) => {
                self.show_toast("Cannot send an empty rich message.", now_secs);
                None
            }
            _ => {
                self.show_toast("Failed to serialize rich message.", now_secs);
                None
            }
        }
    }

    fn render_toolbar(&mut self, ui: &mut Ui, style: &RichPageViewStyle, now_secs: f64) {
        let tb = self.state.toolbar_state();

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            if toolbar_button(ui, "Undo", "Undo (Ctrl+Z)", tb.undo, style) {
                self.state.undo();
                self.markdown_dirty = false;
            }
            if toolbar_button(ui, "Redo", "Redo (Ctrl+Y)", tb.redo, style) {
                self.state.redo();
                self.markdown_dirty = false;
            }

            ui.separator();

            let fmt_buttons: &[(&str, &str, ToolbarActionState, TextFormattingAction)] = &[
                ("Bold", "Bold (Ctrl+B)", tb.bold, TextFormattingAction::Bold),
                (
                    "Italic",
                    "Italic (Ctrl+I)",
                    tb.italic,
                    TextFormattingAction::Italic,
                ),
                (
                    "Underline",
                    "Underline (Ctrl+U)",
                    tb.underline,
                    TextFormattingAction::Underline,
                ),
                (
                    "Strike",
                    "Strikethrough (Ctrl+Shift+X)",
                    tb.strike_out,
                    TextFormattingAction::StrikeOut,
                ),
                (
                    "Code",
                    "Inline code (Ctrl+Shift+M)",
                    tb.code,
                    TextFormattingAction::Code,
                ),
                (
                    "Spoiler",
                    "Spoiler (Ctrl+Shift+P)",
                    tb.spoiler,
                    TextFormattingAction::Spoiler,
                ),
                (
                    "Sub",
                    "Subscript",
                    tb.subscript,
                    TextFormattingAction::Subscript,
                ),
                (
                    "Sup",
                    "Superscript",
                    tb.superscript,
                    TextFormattingAction::Superscript,
                ),
                (
                    "Mark",
                    "Highlight / Mark",
                    tb.marked,
                    TextFormattingAction::Marked,
                ),
                (
                    "Clear",
                    "Clear formatting (Ctrl+Shift+N)",
                    tb.plain_text,
                    TextFormattingAction::PlainText,
                ),
            ];

            for (label, tip, btn_state, action) in fmt_buttons {
                if toolbar_button(ui, label, tip, *btn_state, style) {
                    let res = self.state.toggle_formatting(action.clone());
                    self.check_limit_error(res, now_secs);
                }
            }

            if toolbar_button(ui, "Link", "Insert/edit link (Ctrl+K)", tb.link, style) {
                self.link_prompt_open = !self.link_prompt_open;
                self.close_math_box();
            }

            let math_btn_state = ToolbarActionState {
                enabled: tb.math.enabled,
                active: tb.math.active || self.math_box.is_some(),
            };
            if toolbar_button(
                ui,
                "Math",
                &tr::lng_formatting_math_create_title(),
                math_btn_state,
                style,
            ) {
                if self.math_box.is_some() {
                    self.close_math_box();
                } else {
                    self.open_math_box("", false, Some(false));
                }
            }

            ui.separator();

            // Block Insert menu
            ui.menu_button("Insert Block", |ui| {
                let items: &[(&str, InsertAction)] = &[
                    ("Paragraph", InsertAction::new(InsertBlockType::Paragraph)),
                    ("Heading 1", InsertAction::heading(1)),
                    ("Heading 2", InsertAction::heading(2)),
                    ("Heading 3", InsertAction::heading(3)),
                    ("Blockquote", InsertAction::new(InsertBlockType::Blockquote)),
                    ("Pullquote", InsertAction::new(InsertBlockType::Pullquote)),
                    ("Code Block", InsertAction::code("rust")),
                    ("Math Block ($$)", InsertAction::new(InsertBlockType::Math)),
                    (
                        "Bullet List",
                        InsertAction::new(InsertBlockType::BulletList),
                    ),
                    (
                        "Ordered List",
                        InsertAction::new(InsertBlockType::OrderedList),
                    ),
                    ("Task List", InsertAction::new(InsertBlockType::TaskList)),
                    ("Table (3x3)", InsertAction::new(InsertBlockType::Table)),
                    ("Details", InsertAction::new(InsertBlockType::Details)),
                    ("Footer", InsertAction::new(InsertBlockType::Footer)),
                    ("Divider", InsertAction::new(InsertBlockType::Divider)),
                ];
                for (name, act) in items {
                    if ui.button(*name).clicked() {
                        let res = self.state.insert_block_after_active(act.clone());
                        self.check_limit_error(res, now_secs);
                        self.table_selection = None;
                        ui.close();
                    }
                }
            });

            // Block Convert menu
            ui.menu_button("Convert Block", |ui| {
                let items: &[(&str, InsertAction)] = &[
                    ("Paragraph", InsertAction::new(InsertBlockType::Paragraph)),
                    ("Heading 1", InsertAction::heading(1)),
                    ("Heading 2", InsertAction::heading(2)),
                    ("Heading 3", InsertAction::heading(3)),
                    ("Blockquote", InsertAction::new(InsertBlockType::Blockquote)),
                    ("Pullquote", InsertAction::new(InsertBlockType::Pullquote)),
                    ("Code Block", InsertAction::code("")),
                    ("Math Block", InsertAction::new(InsertBlockType::Math)),
                    (
                        "Bullet List",
                        InsertAction::new(InsertBlockType::BulletList),
                    ),
                    (
                        "Ordered List",
                        InsertAction::new(InsertBlockType::OrderedList),
                    ),
                    ("Task List", InsertAction::new(InsertBlockType::TaskList)),
                    ("Details", InsertAction::new(InsertBlockType::Details)),
                    ("Footer", InsertAction::new(InsertBlockType::Footer)),
                ];
                for (name, act) in items {
                    if ui.button(*name).clicked() {
                        let res = self.state.convert_active_block(act.clone());
                        self.check_limit_error(res, now_secs);
                        self.table_selection = None;
                        ui.close();
                    }
                }
            });
        });
    }

    fn render_link_prompt(&mut self, ui: &mut Ui, style: &RichPageViewStyle, now_secs: f64) {
        egui::Frame::NONE
            .fill(style.code_bg)
            .stroke(Stroke::new(1.0, style.border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Link URL:");
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.link_url_input)
                            .id(Id::unique("tg_rich_editor_link_input"))
                            .desired_width(240.0)
                            .hint_text("https://example.org"),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("Apply Link").clicked() || enter {
                        let url = self.link_url_input.trim().to_owned();
                        let res = self
                            .state
                            .apply_formatting(TextFormattingAction::Link(url), Some(true));
                        self.check_limit_error(res, now_secs);
                        self.link_prompt_open = false;
                        self.link_url_input.clear();
                        self.canvas_focused = true;
                    }
                    if ui.button("Remove Link").clicked() {
                        let res = self.state.apply_formatting(
                            TextFormattingAction::Link(String::new()),
                            Some(false),
                        );
                        self.check_limit_error(res, now_secs);
                        self.link_prompt_open = false;
                        self.canvas_focused = true;
                    }
                    if ui.button("Close").clicked() {
                        self.link_prompt_open = false;
                        self.canvas_focused = true;
                    }
                });
            });
    }

    fn render_math_prompt(&mut self, ui: &mut Ui, style: &RichPageViewStyle, now_secs: f64) {
        let Some(mut math_box) = self.math_box.take() else {
            return;
        };
        let card_w = (ui.available_width() - 24.0).clamp(220.0, 620.0);
        math_box.set_card_width(card_w.round() as i32);

        let mut submitted_action = false;
        let mut close_requested = false;

        let title = if math_box.editing_existing {
            tr::lng_formatting_math_edit_title()
        } else {
            tr::lng_formatting_math_create_title()
        };

        egui::Frame::NONE
            .fill(style.code_bg)
            .stroke(Stroke::new(1.0, style.border))
            .corner_radius(CornerRadius::same(st::iv_formula_preview_radius.0 as u8))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                // Ensure unfocused input and checkbox borders stay clearly visible on light/dark cards.
                ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(1.0, style.border);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&title)
                            .strong()
                            .size(13.0)
                            .color(style.text_fg),
                    );
                    ui.label(
                        RichText::new(tr::lng_formatting_math_source_label())
                            .size(11.5)
                            .color(style.muted_fg),
                    );
                    if math_box.preview.using_sample && ui.small_button("Next Sample").clicked() {
                        math_box.cycle_sample();
                    }
                });

                ui.add_space(4.0);

                ui.horizontal_wrapped(|ui| {
                    let mut input_buf = math_box.raw_input.clone();
                    let input_w = (ui.available_width() - 180.0).clamp(160.0, 340.0);
                    let resp = ui.add(
                        TextEdit::singleline(&mut input_buf)
                            .id(Id::unique("tg_rich_editor_math_input"))
                            .desired_width(input_w)
                            .hint_text(tr::lng_formatting_math_source_placeholder()),
                    );
                    if resp.changed() {
                        math_box.set_input(&input_buf);
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let valid = math_box.is_valid();
                    let insert_btn = if valid {
                        egui::Button::new(RichText::new("Insert Formula").color(Color32::WHITE))
                            .fill(style.accent)
                            .stroke(Stroke::NONE)
                    } else {
                        egui::Button::new("Insert Formula")
                    };
                    if ui.add_enabled(valid, insert_btn).clicked() || (enter && valid) {
                        submitted_action = true;
                    }
                    if ui.button("Close").clicked() {
                        close_requested = true;
                    }
                });

                if let Some(mut sep) = math_box.separate_line {
                    ui.add_space(4.0);
                    if ui
                        .checkbox(&mut sep, tr::lng_formatting_math_separate_line())
                        .changed()
                    {
                        math_box.set_separate_line(sep);
                    }
                }

                if let Some(ref err) = math_box.preview.validation_error {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Formula error: {err}"))
                            .size(11.5)
                            .color(Color32::from_rgb(200, 48, 48)),
                    );
                }

                // Live preview card (`MathPreview` in `iv_editor_math_box.cpp`,
                // `st::ivFormulaPreviewMinHeight` / `st::ivFormulaPreviewRadius`).
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if math_box.preview.using_sample {
                        format!("{} (sample)", tr::lng_formatting_math_result_label())
                    } else {
                        tr::lng_formatting_math_result_label()
                    })
                    .size(11.0)
                    .color(style.muted_fg),
                );
                let pad_x = st::iv_formula_preview_padding.left.0 as i8;
                let pad_y = st::iv_formula_preview_padding.top.0 as i8;
                let min_h = (st::iv_formula_preview_min_height.0 as f32).min(64.0);
                let preview_fg = if math_box.preview.using_sample {
                    style.muted_fg
                } else {
                    style.text_fg
                };
                egui::Frame::NONE
                    .fill(style.table_stripe_bg)
                    .stroke(Stroke::new(0.75, style.border))
                    .corner_radius(CornerRadius::same(st::iv_formula_preview_radius.0 as u8))
                    .inner_margin(egui::Margin::symmetric(pad_x, pad_y))
                    .show(ui, |ui| {
                        ui.set_min_height(min_h - f32::from(pad_y) * 2.0);
                        ui.set_min_width((card_w - f32::from(pad_x) * 2.0).max(120.0));
                        if !math_box.preview.source.is_empty()
                            && math_box.preview.validation_error.is_none()
                        {
                            let mut fstyle = match math_box.kind() {
                                MathKind::Display => FormulaWidgetStyle::display(
                                    style.base_font_size * 1.15,
                                    preview_fg,
                                ),
                                MathKind::Inline => {
                                    FormulaWidgetStyle::inline(style.base_font_size, preview_fg)
                                }
                            };
                            fstyle.max_width = (card_w - 24.0).max(80.0).round() as i32;
                            let (rendered, metrics) = layout_formula_for_style(
                                &mut self.math_engine,
                                &math_box.preview.source,
                                &fstyle,
                            );
                            let avail_w = ui.available_width().max(metrics.size.x);
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(avail_w, metrics.size.y.max(24.0)),
                                Sense::hover(),
                            );
                            let x = rect.left() + ((rect.width() - metrics.size.x) * 0.5).max(0.0);
                            let y = rect.top() + metrics.baseline_y;
                            paint_formula_at_baseline(
                                ui.painter(),
                                Pos2::new(x, y),
                                &rendered,
                                &fstyle,
                            );
                        }
                    });
            });

        if submitted_action {
            match math_box.submit() {
                Ok(sub) => {
                    let res = if sub.separate_line {
                        let ins = self
                            .state
                            .insert_block_after_active(InsertAction::new(InsertBlockType::Math));
                        if ins == ApplyResult::Changed {
                            self.state.edit_active_display_math(sub.source)
                        } else {
                            ins
                        }
                    } else {
                        self.state.insert_inline_math(&sub.source)
                    };
                    self.check_limit_error(res, now_secs);
                    self.markdown_dirty = false;
                    self.canvas_focused = true;
                }
                Err(err) => {
                    self.show_toast(format!("Invalid formula: {err}"), now_secs);
                    self.math_box = Some(math_box);
                }
            }
        } else if close_requested {
            math_box.close();
            self.canvas_focused = true;
        } else {
            self.math_box = Some(math_box);
        }
    }

    fn render_table_toolbar(
        &mut self,
        ui: &mut Ui,
        range: &EditTableCellRange,
        style: &RichPageViewStyle,
        now_secs: f64,
    ) {
        let info = self.state.table_selection_info(range);
        let blk_flags = resolve_block(self.state.rich_page(), &range.block)
            .map(|b| (b.bordered, b.striped, b.compact))
            .unwrap_or((true, false, false));

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("Table:").size(11.5).color(style.muted_fg));

            if ui.small_button("+ Row Above").clicked() {
                let res = self.state.add_table_row(range, false);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("+ Row Below").clicked() {
                let res = self.state.add_table_row(range, true);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("+ Col Left").clicked() {
                let res = self.state.add_table_column(range, false);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("+ Col Right").clicked() {
                let res = self.state.add_table_column(range, true);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("Remove Row").clicked() {
                let res = self.state.remove_table_rows(range);
                self.check_limit_error(res, now_secs);
                self.table_selection = None;
            }
            if ui.selectable_label(info.all_header, "Header").clicked() {
                let res = self.state.set_table_header(range, !info.all_header);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("Align L").clicked() {
                let res = self
                    .state
                    .set_table_alignment(range, RichTableAlignment::Left);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("Align C").clicked() {
                let res = self
                    .state
                    .set_table_alignment(range, RichTableAlignment::Center);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("Align R").clicked() {
                let res = self
                    .state
                    .set_table_alignment(range, RichTableAlignment::Right);
                self.check_limit_error(res, now_secs);
            }
            if ui.small_button("V-Mid").clicked() {
                let res = self
                    .state
                    .set_table_vertical_alignment(range, RichTableVerticalAlignment::Middle);
                self.check_limit_error(res, now_secs);
            }
            if info.can_unite_cells && ui.small_button("Merge").clicked() {
                let res = self.state.unite_table_cells(range);
                self.check_limit_error(res, now_secs);
            }
            if info.can_split_cell && ui.small_button("Split").clicked() {
                let res = self.state.split_table_cell(range);
                self.check_limit_error(res, now_secs);
            }
            if ui.selectable_label(blk_flags.0, "Bordered").clicked() {
                let res =
                    self.state
                        .set_table_style_flags(&range.block, Some(!blk_flags.0), None, None);
                self.check_limit_error(res, now_secs);
            }
            if ui.selectable_label(blk_flags.1, "Striped").clicked() {
                let res =
                    self.state
                        .set_table_style_flags(&range.block, None, Some(!blk_flags.1), None);
                self.check_limit_error(res, now_secs);
            }
            if ui.selectable_label(blk_flags.2, "Compact").clicked() {
                let res =
                    self.state
                        .set_table_style_flags(&range.block, None, None, Some(!blk_flags.2));
                self.check_limit_error(res, now_secs);
            }
        });
    }

    fn render_markdown_tab(
        &mut self,
        ui: &mut Ui,
        body_h: f32,
        style: &RichPageViewStyle,
        now_secs: f64,
    ) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Import Markdown").clicked() {
                let limits = *self.state.limits();
                match EditorState::from_markdown(&self.markdown_buffer, limits) {
                    Ok(new_state) => {
                        self.state = new_state;
                        self.markdown_dirty = false;
                        self.table_selection = None;
                        self.tab = RichEditorTab::Edit;
                        self.show_toast("Imported Markdown document.", now_secs);
                    }
                    Err(err) => {
                        self.show_toast(format!("Markdown import error: {err}"), now_secs);
                    }
                }
            }
            if ui.button("Import HTML").clicked() {
                let payload = ClipboardPayload {
                    html: Some(self.markdown_buffer.clone()),
                    ..ClipboardPayload::default()
                };
                if self.paste_clipboard_payload(payload, now_secs) {
                    self.tab = RichEditorTab::Edit;
                } else {
                    self.show_toast("No valid HTML blocks found to import.", now_secs);
                }
            }
            if ui.button("Export / Refresh").clicked() {
                self.markdown_buffer = self.state.to_markdown();
                self.markdown_dirty = false;
            }
            if ui.button("Copy Markdown").clicked() {
                let md = self.state.to_markdown();
                ui.ctx().copy_text(md);
                self.show_toast("Copied Markdown to clipboard.", now_secs);
            }
            if ui.button("Copy HTML").clicked() {
                let html = self.export_html().html;
                ui.ctx().copy_text(html);
                self.show_toast("Copied sanitized HTML to clipboard.", now_secs);
            }
            if ui.button(tr::lng_context_save_html()).clicked() {
                let target_dir = if let Some(dialog) = self.export_dialog.as_ref() {
                    dialog.pick_folder(self.export_dir.as_deref())
                } else {
                    self.export_dir
                        .clone()
                        .or_else(|| NativeExportFolderDialog.pick_folder(None))
                };
                if let Some(dir) = target_dir {
                    self.export_dir = Some(dir.clone());
                    match self.export_html_to_dir::<tg_iv::MemoryMediaStore>(&dir, None) {
                        Ok(res) => {
                            let folder_name = res
                                .folder
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("export");
                            self.show_toast(tr::lng_export_html_saved_to(folder_name), now_secs);
                        }
                        Err(_) => {
                            self.show_toast(tr::lng_export_html_failed(), now_secs);
                        }
                    }
                }
            }
            ui.label(
                RichText::new("Edit or paste GFM + $math$ Markdown or HTML, or export as HTML.")
                    .size(11.5)
                    .color(style.muted_fg),
            );
        });
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("tg_rich_editor_md_scroll")
            .max_height((body_h - 32.0).max(120.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let resp = ui.add(
                    TextEdit::multiline(&mut self.markdown_buffer)
                        .id(Id::unique("tg_rich_editor_md_edit"))
                        .font(FontId::monospace(13.0))
                        .desired_width(f32::INFINITY)
                        .desired_rows(14),
                );
                if resp.changed() {
                    self.markdown_dirty = true;
                }
            });
    }

    fn handle_keyboard_input(&mut self, ctx: &egui::Context, now_secs: f64) {
        let events: Vec<Event> = ctx.input(|i| i.events.clone());
        let mut mutated = false;

        for ev in events {
            match ev {
                Event::Text(text) => {
                    if text.chars().count() == 1 {
                        if let Some(ch) = text.chars().next()
                            && !ch.is_control()
                        {
                            let res = self.state.handle_auto_pair_char(ch);
                            self.check_limit_error(res, now_secs);
                            mutated |= res == ApplyResult::Changed;
                        }
                    } else if !text.is_empty() {
                        let res = self.state.insert_text(&text);
                        self.check_limit_error(res, now_secs);
                        mutated |= res == ApplyResult::Changed;
                    }
                }
                Event::Paste(text) => {
                    if !text.is_empty() {
                        let imported = if looks_like_html_fragment(&text) {
                            self.paste_clipboard_payload(
                                ClipboardPayload {
                                    html: Some(text.clone()),
                                    plain_text: Some(text.clone()),
                                    ..ClipboardPayload::default()
                                },
                                now_secs,
                            )
                        } else if text.contains('\t') && text.contains('\n') {
                            self.paste_clipboard_payload(
                                ClipboardPayload {
                                    plain_text: Some(text.clone()),
                                    spreadsheet_hint: true,
                                    ..ClipboardPayload::default()
                                },
                                now_secs,
                            )
                        } else if text_has_markdown_structure(&text, self.state.limits()) {
                            self.paste_clipboard_payload(
                                ClipboardPayload {
                                    plain_text: Some(text.clone()),
                                    ..ClipboardPayload::default()
                                },
                                now_secs,
                            )
                        } else {
                            false
                        };
                        if imported {
                            mutated = true;
                        } else {
                            let res = self.state.insert_text(&text);
                            self.check_limit_error(res, now_secs);
                            mutated |= res == ApplyResult::Changed;
                        }
                    }
                }
                Event::Copy => {
                    let cur = self.state.cursor();
                    if cur.has_selection() {
                        let text = self.state.active_raw_text();
                        let (from, till) = cur.selection_range();
                        let b0 = utf16_to_byte_offset(&text, from);
                        let b1 = utf16_to_byte_offset(&text, till);
                        ctx.copy_text(text[b0..b1].to_owned());
                    }
                }
                Event::Cut => {
                    let cur = self.state.cursor();
                    if cur.has_selection() {
                        let text = self.state.active_raw_text();
                        let (from, till) = cur.selection_range();
                        let b0 = utf16_to_byte_offset(&text, from);
                        let b1 = utf16_to_byte_offset(&text, till);
                        ctx.copy_text(text[b0..b1].to_owned());
                        let res = self.state.delete_backward();
                        self.check_limit_error(res, now_secs);
                        mutated |= res == ApplyResult::Changed;
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if modifiers.command {
                        match key {
                            Key::Z if modifiers.shift => {
                                self.state.redo();
                                mutated = true;
                            }
                            Key::Z => {
                                self.state.undo();
                                mutated = true;
                            }
                            Key::Y => {
                                self.state.redo();
                                mutated = true;
                            }
                            Key::A => {
                                self.state.select_all_in_leaf();
                            }
                            Key::B => {
                                let res = self.state.toggle_formatting(TextFormattingAction::Bold);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::I => {
                                let res =
                                    self.state.toggle_formatting(TextFormattingAction::Italic);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::U => {
                                let res = self
                                    .state
                                    .toggle_formatting(TextFormattingAction::Underline);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::X if modifiers.shift => {
                                let res = self
                                    .state
                                    .toggle_formatting(TextFormattingAction::StrikeOut);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::M if modifiers.shift => {
                                let res = self.state.toggle_formatting(TextFormattingAction::Code);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::P if modifiers.shift => {
                                let res =
                                    self.state.toggle_formatting(TextFormattingAction::Spoiler);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::N if modifiers.shift => {
                                let res = self
                                    .state
                                    .toggle_formatting(TextFormattingAction::PlainText);
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            }
                            Key::K if self.state.toolbar_state().link.enabled => {
                                self.link_prompt_open = true;
                                self.close_math_box();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match key {
                        Key::Backspace => {
                            let res = self.state.handle_auto_pair_backspace();
                            self.check_limit_error(res, now_secs);
                            mutated |= res == ApplyResult::Changed;
                        }
                        Key::Delete => {
                            let res = self.state.delete_forward();
                            self.check_limit_error(res, now_secs);
                            mutated |= res == ApplyResult::Changed;
                        }
                        Key::Enter => {
                            let res = self.state.handle_enter();
                            self.check_limit_error(res, now_secs);
                            mutated |= res == ApplyResult::Changed;
                        }
                        Key::ArrowLeft => {
                            self.state.move_cursor_left(modifiers.shift);
                        }
                        Key::ArrowRight => {
                            self.state.move_cursor_right(modifiers.shift);
                        }
                        Key::ArrowUp => {
                            if let Some(prev) = self.state.previous_editable_ordinal() {
                                let head = self.state.cursor().head;
                                self.state.set_cursor(prev, head);
                            }
                        }
                        Key::ArrowDown => {
                            if let Some(next) = self.state.next_editable_ordinal() {
                                let head = self.state.cursor().head;
                                self.state.set_cursor(next, head);
                            }
                        }
                        Key::Home => {
                            let cur = self.state.cursor();
                            if modifiers.shift {
                                self.state.set_selection(cur.leaf_ordinal, cur.anchor, 0);
                            } else {
                                self.state.set_cursor(cur.leaf_ordinal, 0);
                            }
                        }
                        Key::End => {
                            let cur = self.state.cursor();
                            let len = self.state.active_text_len();
                            if modifiers.shift {
                                self.state.set_selection(cur.leaf_ordinal, cur.anchor, len);
                            } else {
                                self.state.set_cursor(cur.leaf_ordinal, len);
                            }
                        }
                        Key::Tab => {
                            let is_list = self
                                .state
                                .active_leaf_path()
                                .is_some_and(|l| l.kind == LeafKind::ListItemText);
                            if is_list {
                                let res = if modifiers.shift {
                                    self.state.lift_active_list_item()
                                } else {
                                    self.state.sink_active_list_item()
                                };
                                self.check_limit_error(res, now_secs);
                                mutated |= res == ApplyResult::Changed;
                            } else if modifiers.shift {
                                if let Some(prev) = self.state.previous_editable_ordinal() {
                                    self.state.set_active_text_by_ordinal(prev);
                                }
                            } else if let Some(next) = self.state.next_editable_ordinal() {
                                self.state.set_cursor(next, 0);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if mutated {
            self.markdown_dirty = false;
        }
    }

    fn render_editor_blocks(
        &mut self,
        ui: &mut Ui,
        container: &BlockContainerPath,
        max_width: f32,
        style: &RichPageViewStyle,
        depth: usize,
        now_secs: f64,
    ) {
        if depth > 12 {
            return;
        }
        let blocks: Vec<RichBlock> =
            tg_iv::editor::resolve_container(self.state.rich_page(), container)
                .map(<[RichBlock]>::to_vec)
                .unwrap_or_default();

        for (idx, block) in blocks.iter().enumerate() {
            let bp = BlockPath {
                container: container.clone(),
                index: idx,
            };
            self.render_editor_block(ui, &bp, block, max_width, style, depth, now_secs);
            ui.add_space(style.block_spacing);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_editor_block(
        &mut self,
        ui: &mut Ui,
        bp: &BlockPath,
        block: &RichBlock,
        max_width: f32,
        style: &RichPageViewStyle,
        depth: usize,
        now_secs: f64,
    ) {
        match block.kind {
            RichBlockKind::Paragraph => {
                let leaf = LeafPath::block_text(bp.clone());
                self.render_editable_leaf(
                    ui,
                    &leaf,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    max_width,
                    false,
                    false,
                    false,
                    "Type something...",
                );
            }
            RichBlockKind::Heading => {
                let leaf = LeafPath::block_text(bp.clone());
                let fsz = heading_font_size(block.heading_level, style.base_font_size);
                self.render_editable_leaf(
                    ui,
                    &leaf,
                    &block.text.text,
                    fsz,
                    style.text_fg,
                    style,
                    max_width,
                    true,
                    false,
                    false,
                    &format!("Heading {}", block.heading_level.clamp(1, 6)),
                );
            }
            RichBlockKind::Footer => {
                let leaf = LeafPath::block_text(bp.clone());
                self.render_editable_leaf(
                    ui,
                    &leaf,
                    &block.text.text,
                    style.base_font_size * 0.88,
                    style.muted_fg,
                    style,
                    max_width,
                    false,
                    false,
                    false,
                    "Footer text...",
                );
            }
            RichBlockKind::Code => {
                let leaf = LeafPath::block_text(bp.clone());
                let bp_lang = bp.clone();
                let mut lang_val = block.language.clone();
                egui::Frame::NONE
                    .fill(style.code_bg)
                    .stroke(Stroke::new(1.0, style.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Code language:")
                                    .size(11.0)
                                    .color(style.muted_fg),
                            );
                            let resp = ui.add(
                                TextEdit::singleline(&mut lang_val)
                                    .desired_width(100.0)
                                    .font(FontId::monospace(11.5))
                                    .hint_text("plain"),
                            );
                            if resp.changed() {
                                let res = self.state.set_code_block_language(&bp_lang, lang_val);
                                self.check_limit_error(res, now_secs);
                            }
                        });
                        ui.add_space(3.0);
                        self.render_editable_leaf(
                            ui,
                            &leaf,
                            &block.text.text,
                            (style.base_font_size * 0.92).max(10.0),
                            style.code_fg,
                            style,
                            (max_width - 20.0).max(80.0),
                            false,
                            false,
                            true,
                            "// code...",
                        );
                    });
            }
            RichBlockKind::Math => {
                let leaf = LeafPath::math_formula(bp.clone());
                let raw_rich = TextWithEntities::simple(block.formula.clone());
                egui::Frame::NONE
                    .fill(style.code_bg)
                    .stroke(Stroke::new(1.0, style.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Display Math (TeX)")
                                .size(11.0)
                                .color(style.muted_fg),
                        );
                        self.render_editable_leaf(
                            ui,
                            &leaf,
                            &raw_rich,
                            (style.base_font_size * 0.92).max(10.0),
                            style.code_fg,
                            style,
                            (max_width - 20.0).max(80.0),
                            false,
                            false,
                            true,
                            r"\int_0^\infty e^{-x^2}\,dx",
                        );
                        if !block.formula.trim().is_empty() {
                            ui.add_space(4.0);
                            let mut fstyle = FormulaWidgetStyle::display(
                                style.base_font_size * 1.1,
                                style.text_fg,
                            );
                            fstyle.max_width = (max_width - 24.0).max(80.0).round() as i32;
                            let (rendered, metrics) = layout_formula_for_style(
                                &mut self.math_engine,
                                &block.formula,
                                &fstyle,
                            );
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new((max_width - 24.0).max(metrics.size.x), metrics.size.y),
                                Sense::hover(),
                            );
                            let x = rect.left() + ((rect.width() - metrics.size.x) * 0.5).max(0.0);
                            let baseline_y = rect.top() + metrics.baseline_y;
                            paint_formula_at_baseline(
                                ui.painter(),
                                Pos2::new(x, baseline_y),
                                &rendered,
                                &fstyle,
                            );
                        }
                    });
            }
            RichBlockKind::Divider => {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(max_width, 12.0), Sense::hover());
                let y = rect.center().y;
                ui.painter().line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    Stroke::new(1.5, style.border),
                );
            }
            RichBlockKind::Quote => {
                let inner_w = (max_width - 24.0).max(80.0);
                let resp = egui::Frame::NONE
                    .fill(style.quote_bg)
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 8,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        if !block.pullquote {
                            ui.horizontal(|ui| {
                                let lbl = if block.collapsed {
                                    "Quote (collapsed)"
                                } else {
                                    "Quote"
                                };
                                if ui.small_button(lbl).clicked() {
                                    let res = self.state.toggle_quote_collapsed(bp);
                                    self.check_limit_error(res, now_secs);
                                }
                            });
                        }
                        if block.blocks.is_empty() || !block.text.is_structurally_empty() {
                            let leaf = LeafPath::block_text(bp.clone());
                            self.render_editable_leaf(
                                ui,
                                &leaf,
                                &block.text.text,
                                style.base_font_size,
                                style.text_fg,
                                style,
                                inner_w,
                                false,
                                block.pullquote,
                                false,
                                "Quoted text...",
                            );
                        }
                        if !block.blocks.is_empty() {
                            self.render_editor_blocks(
                                ui,
                                &bp.children_container(),
                                inner_w,
                                style,
                                depth + 1,
                                now_secs,
                            );
                        }
                        let cap_leaf = LeafPath::block_caption(bp.clone());
                        self.render_editable_leaf(
                            ui,
                            &cap_leaf,
                            &block.caption.text,
                            style.base_font_size * 0.86,
                            style.muted_fg,
                            style,
                            inner_w,
                            false,
                            true,
                            false,
                            "Caption / attribution...",
                        );
                    })
                    .response;
                let bar = Rect::from_min_size(resp.rect.min, Vec2::new(3.0, resp.rect.height()));
                ui.painter()
                    .rect_filled(bar, CornerRadius::same(2), style.accent);
            }
            RichBlockKind::Details => {
                let inner_w = (max_width - 24.0).max(80.0);
                egui::Frame::NONE
                    .stroke(Stroke::new(1.0, style.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let dir = if block.open {
                                tg_app_core::ui_icons::ArrowDir::Down
                            } else {
                                tg_app_core::ui_icons::ArrowDir::Right
                            };
                            if tg_app_core::ui_icons::small_icon_button(
                                ui,
                                if block.open {
                                    "Collapse details"
                                } else {
                                    "Expand details"
                                },
                                true,
                                move |p, c, s, col| {
                                    tg_app_core::ui_icons::paint_triangle(p, c, s * 0.78, dir, col);
                                },
                            )
                            .clicked()
                            {
                                let res = self.state.toggle_details_open(bp);
                                self.check_limit_error(res, now_secs);
                            }
                            let leaf = LeafPath::block_text(bp.clone());
                            self.render_editable_leaf(
                                ui,
                                &leaf,
                                &block.text.text,
                                style.base_font_size,
                                style.text_fg,
                                style,
                                (inner_w - 28.0).max(60.0),
                                true,
                                false,
                                false,
                                "Details summary...",
                            );
                        });
                        if block.open {
                            ui.indent(Id::unique(("tg_rich_details_body", bp)), |ui| {
                                self.render_editor_blocks(
                                    ui,
                                    &bp.children_container(),
                                    (inner_w - 16.0).max(60.0),
                                    style,
                                    depth + 1,
                                    now_secs,
                                );
                            });
                        }
                    });
            }
            RichBlockKind::List => {
                let inner_w = (max_width - 32.0).max(80.0);
                ui.horizontal(|ui| {
                    if ui.small_button("•").on_hover_text("Bullet list").clicked() {
                        let res = self.state.set_list_style(bp, ListStyle::Bullet);
                        self.check_limit_error(res, now_secs);
                    }
                    if ui
                        .small_button("1.")
                        .on_hover_text("Ordered list")
                        .clicked()
                    {
                        let res = self.state.set_list_style(bp, ListStyle::Ordered);
                        self.check_limit_error(res, now_secs);
                    }
                    if tg_app_core::ui_icons::small_icon_button(
                        ui,
                        "Task checklist",
                        true,
                        |p, c, s, col| {
                            tg_app_core::ui_icons::paint_checkbox_glyph(p, c, s, true, col);
                        },
                    )
                    .on_hover_text("Task checklist")
                    .clicked()
                    {
                        let res = self.state.set_list_style(bp, ListStyle::Task);
                        self.check_limit_error(res, now_secs);
                    }
                    if block.list_kind == RichListKind::Ordered {
                        if ui
                            .small_button("a/A")
                            .on_hover_text("Alphabetic markers")
                            .clicked()
                        {
                            let next = match block.ordered_list.list_type.as_deref() {
                                Some("a") => OrderedListType::UpperAlpha,
                                Some("A") => OrderedListType::LowerRoman,
                                Some("i") => OrderedListType::UpperRoman,
                                Some("I") => OrderedListType::Decimal,
                                _ => OrderedListType::LowerAlpha,
                            };
                            let res = self.state.set_list_ordered_type(bp, next);
                            self.check_limit_error(res, now_secs);
                        }
                        if ui
                            .selectable_label(block.ordered_list.reversed, "Rev")
                            .on_hover_text("Reverse numbering")
                            .clicked()
                        {
                            let res = self
                                .state
                                .set_list_ordered_reversed(bp, !block.ordered_list.reversed);
                            self.check_limit_error(res, now_secs);
                        }
                    }
                });
                for (item_idx, item) in block.list_items.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        match item.task_state {
                            RichTaskState::Checked | RichTaskState::Unchecked => {
                                let checked = item.task_state == RichTaskState::Checked;
                                if tg_app_core::ui_icons::small_icon_button(
                                    ui,
                                    if checked {
                                        "Mark task incomplete"
                                    } else {
                                        "Mark task complete"
                                    },
                                    true,
                                    move |p, c, s, col| {
                                        tg_app_core::ui_icons::paint_checkbox_glyph(
                                            p, c, s, checked, col,
                                        );
                                    },
                                )
                                .clicked()
                                {
                                    let res = self.state.toggle_task_state(bp, item_idx);
                                    self.check_limit_error(res, now_secs);
                                }
                            }
                            RichTaskState::None => {
                                let marker = match block.list_kind {
                                    RichListKind::Bullet => "•".to_owned(),
                                    RichListKind::Ordered => format_ordered_marker(block, item_idx),
                                };
                                ui.allocate_ui(Vec2::new(22.0, 20.0), |ui| {
                                    ui.label(
                                        RichText::new(marker)
                                            .size(style.base_font_size)
                                            .color(style.accent),
                                    );
                                });
                            }
                        }
                        ui.vertical(|ui| {
                            if item.blocks.is_empty() || !item.text.is_structurally_empty() {
                                let leaf = LeafPath::list_item_text(bp.clone(), item_idx);
                                self.render_editable_leaf(
                                    ui,
                                    &leaf,
                                    &item.text.text,
                                    style.base_font_size,
                                    style.text_fg,
                                    style,
                                    inner_w,
                                    false,
                                    false,
                                    false,
                                    "List item...",
                                );
                            }
                            if !item.blocks.is_empty() {
                                self.render_editor_blocks(
                                    ui,
                                    &bp.list_item_children_container(item_idx),
                                    inner_w,
                                    style,
                                    depth + 1,
                                    now_secs,
                                );
                            }
                        });
                    });
                }
            }
            RichBlockKind::Table => {
                let title_leaf = LeafPath::block_text(bp.clone());
                self.render_editable_leaf(
                    ui,
                    &title_leaf,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    max_width,
                    true,
                    false,
                    false,
                    "Table title (optional)...",
                );
                ui.add_space(3.0);
                let cols = block
                    .table_rows
                    .iter()
                    .map(|r| {
                        r.cells
                            .iter()
                            .map(|c| c.colspan.max(1) as usize)
                            .sum::<usize>()
                    })
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let col_w = ((max_width - 12.0) / cols as f32).max(56.0);
                let active_sel = self.active_table_range();

                egui::Frame::NONE
                    .stroke(Stroke::new(1.0, style.border))
                    .corner_radius(CornerRadius::same(4))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        for (r_idx, row) in block.table_rows.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::ZERO;
                                for (c_idx, cell) in row.cells.iter().enumerate() {
                                    let cw = col_w * cell.colspan.max(1) as f32;
                                    let cell_leaf =
                                        LeafPath::table_cell_text(bp.clone(), r_idx, c_idx);
                                    let in_sel = active_sel.as_ref().is_some_and(|sel| {
                                        sel.block == *bp
                                            && r_idx >= sel.row_from
                                            && r_idx < sel.row_till
                                            && c_idx >= sel.column_from
                                            && c_idx < sel.column_till
                                    });
                                    let bg = if in_sel {
                                        Color32::from_rgba_unmultiplied(
                                            style.accent.r(),
                                            style.accent.g(),
                                            style.accent.b(),
                                            30,
                                        )
                                    } else if cell.header {
                                        style.table_header_bg
                                    } else if block.striped && r_idx % 2 == 1 {
                                        style.table_stripe_bg
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    let cell_frame = egui::Frame::NONE
                                        .fill(bg)
                                        .stroke(Stroke::new(0.75, style.border))
                                        .inner_margin(egui::Margin::same(if block.compact {
                                            3
                                        } else {
                                            5
                                        }))
                                        .show(ui, |ui| {
                                            ui.set_min_width((cw - 10.0).max(36.0));
                                            ui.set_max_width((cw - 10.0).max(36.0));
                                            let clicked = self.render_editable_leaf(
                                                ui,
                                                &cell_leaf,
                                                &cell.text.text,
                                                style.base_font_size * 0.92,
                                                style.text_fg,
                                                style,
                                                (cw - 12.0).max(32.0),
                                                cell.header,
                                                false,
                                                false,
                                                "Cell",
                                            );
                                            if clicked {
                                                let shift = ui.input(|i| i.modifiers.shift);
                                                if shift
                                                    && let Some(prev) = self.active_table_range()
                                                    && prev.block == *bp
                                                {
                                                    self.table_selection =
                                                        Some(EditTableCellRange {
                                                            block: bp.clone(),
                                                            row_from: prev.row_from.min(r_idx),
                                                            row_till: prev.row_till.max(r_idx + 1),
                                                            column_from: prev
                                                                .column_from
                                                                .min(c_idx),
                                                            column_till: prev
                                                                .column_till
                                                                .max(c_idx + 1),
                                                        });
                                                } else {
                                                    self.table_selection =
                                                        Some(EditTableCellRange {
                                                            block: bp.clone(),
                                                            row_from: r_idx,
                                                            row_till: r_idx + 1,
                                                            column_from: c_idx,
                                                            column_till: c_idx + 1,
                                                        });
                                                }
                                            }
                                        });
                                    let _ = cell_frame;
                                }
                            });
                        }
                    });
            }
            RichBlockKind::Photo
            | RichBlockKind::Video
            | RichBlockKind::Audio
            | RichBlockKind::File
            | RichBlockKind::Map
            | RichBlockKind::GroupedMedia => {
                egui::Frame::NONE
                    .fill(style.code_bg)
                    .stroke(Stroke::new(1.0, style.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("[{:?} block]", block.kind))
                                    .size(12.0)
                                    .color(style.muted_fg),
                            );
                            if matches!(block.kind, RichBlockKind::Photo | RichBlockKind::Video)
                                && ui.selectable_label(block.spoiler, "Spoiler").clicked()
                            {
                                let res = self.state.toggle_spoiler_on_block(bp, None);
                                self.check_limit_error(res, now_secs);
                            }
                            if ui.small_button("Remove").clicked() {
                                let res = self.state.remove_block(bp.clone());
                                self.check_limit_error(res, now_secs);
                            }
                        });
                        let cap = LeafPath::block_caption(bp.clone());
                        self.render_editable_leaf(
                            ui,
                            &cap,
                            &block.caption.text,
                            style.base_font_size * 0.88,
                            style.muted_fg,
                            style,
                            (max_width - 20.0).max(80.0),
                            false,
                            true,
                            false,
                            "Caption...",
                        );
                    });
            }
            _ => {
                ui.label(
                    RichText::new(format!("[{:?}]", block.kind))
                        .size(12.0)
                        .color(style.muted_fg),
                );
            }
        }
    }

    /// Renders a single editable leaf with UTF-16 ↔ byte-offset caret & selection painting,
    /// inline formula painting via `formula_widget`, and click/drag selection handling.
    ///
    /// Returns `true` if the user clicked inside this leaf.
    #[allow(clippy::too_many_arguments)]
    fn render_editable_leaf(
        &mut self,
        ui: &mut Ui,
        leaf: &LeafPath,
        rich: &TextWithEntities,
        font_size: f32,
        base_color: Color32,
        style: &RichPageViewStyle,
        max_width: f32,
        force_bold: bool,
        force_italic: bool,
        raw_mono: bool,
        placeholder: &str,
    ) -> bool {
        let ordinal = self.state.text_ordinal_for_leaf(leaf);
        let cursor = self.state.cursor();
        let is_active = ordinal == Some(cursor.leaf_ordinal);
        let leaf_id = Id::unique(("tg_rich_editor_leaf", leaf));

        let mut clicked_here = false;
        let has_objects = !raw_mono && has_inline_objects(rich);

        // Build galley for standard text leaves (and caret/selection hit-testing).
        let job = if raw_mono {
            let mut j = egui::text::LayoutJob::default();
            j.wrap.max_width = max_width.max(32.0);
            j.append(
                &rich.text,
                0.0,
                egui::TextFormat {
                    font_id: FontId::monospace(font_size),
                    color: base_color,
                    ..egui::TextFormat::default()
                },
            );
            j
        } else {
            build_rich_leaf_layout_job(
                rich,
                font_size,
                base_color,
                style,
                max_width,
                force_bold,
                force_italic,
            )
        };
        let galley = ui.painter().layout_job(job);
        let min_h = (font_size * 1.45).max(20.0);
        let desired_h = galley.size().y.max(min_h);
        let box_size = Vec2::new(max_width.max(36.0), desired_h + 6.0);

        if has_objects {
            // Paint baseline-aligned inline formulas + styled text inside a clickable frame.
            let frame_resp = egui::Frame::NONE
                .stroke(if is_active {
                    Stroke::new(1.0, style.accent)
                } else {
                    Stroke::NONE
                })
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(4, 2))
                .show(ui, |ui| {
                    let (sz, _) = paint_inline_rich_text(
                        ui,
                        leaf_id,
                        &mut self.math_engine,
                        rich,
                        font_size,
                        base_color,
                        style,
                        (max_width - 10.0).max(32.0),
                        force_bold,
                        force_italic,
                    );
                    if is_active && self.canvas_focused {
                        // Draw caret at the right edge or proportional UTF-16 position.
                        let total_u16 = rich.utf16_len().max(1) as f32;
                        let frac = (cursor.head as f32 / total_u16).clamp(0.0, 1.0);
                        let min = ui.min_rect().min;
                        let caret_x = min.x + sz.x * frac;
                        ui.painter().line_segment(
                            [
                                Pos2::new(caret_x, min.y + 1.0),
                                Pos2::new(caret_x, min.y + sz.y.min(font_size * 1.5)),
                            ],
                            Stroke::new(1.5, style.accent),
                        );
                    }
                })
                .response;
            let hit = ui.interact(frame_resp.rect, leaf_id.with("hit"), Sense::click());
            if hit.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
            if hit.clicked()
                && let Some(ord) = ordinal
            {
                self.state.set_active_text_by_ordinal(ord);
                self.canvas_focused = true;
                clicked_here = true;
            }
            return clicked_here;
        }

        let (rect, resp) = ui.allocate_exact_size(box_size, Sense::click_and_drag());
        resp.widget_info(|| WidgetInfo::labeled(Role::MultilineTextInput, true, placeholder));

        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }

        // Subtle active leaf outline.
        if is_active {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(
                        style.accent.r(),
                        style.accent.g(),
                        style.accent.b(),
                        120,
                    ),
                ),
                StrokeKind::Inside,
            );
        }

        let text_origin = Pos2::new(rect.left() + 4.0, rect.top() + 3.0);

        // Paint selection highlight rects on the active leaf (UTF-16 -> byte -> char -> galley rect).
        if is_active && cursor.has_selection() {
            let (sel_u16_start, sel_u16_end) = cursor.selection_range();
            let byte_start = utf16_to_byte_offset(&rich.text, sel_u16_start);
            let byte_end = utf16_to_byte_offset(&rich.text, sel_u16_end);
            let char_start = rich.text[..byte_start].chars().count();
            let char_end = rich.text[..byte_end].chars().count();
            let sel_bg = Color32::from_rgba_unmultiplied(
                style.accent.r(),
                style.accent.g(),
                style.accent.b(),
                68,
            );
            paint_galley_selection_rects(
                ui.painter(),
                &galley,
                text_origin,
                char_start,
                char_end,
                sel_bg,
            );
        }

        // Paint placeholder or formatted galley.
        if rich.text.is_empty() {
            ui.painter().text(
                text_origin,
                Align2::LEFT_TOP,
                placeholder,
                if raw_mono {
                    FontId::monospace(font_size)
                } else {
                    FontId::proportional(font_size)
                },
                Color32::from_rgba_unmultiplied(
                    style.muted_fg.r(),
                    style.muted_fg.g(),
                    style.muted_fg.b(),
                    140,
                ),
            );
        } else {
            ui.painter().galley(text_origin, galley.clone(), base_color);
        }

        // Paint blinking caret at `cursor.head` (UTF-16 -> byte -> char -> galley pos).
        if is_active && self.canvas_focused {
            let head_byte = utf16_to_byte_offset(&rich.text, cursor.head);
            let head_char = rich.text[..head_byte].chars().count();
            let ccursor = egui::text::CCursor::new(head_char);
            let caret_local = galley.pos_from_cursor(ccursor);
            let caret_top = text_origin + caret_local.min.to_vec2();
            let caret_bot = Pos2::new(
                caret_top.x,
                (text_origin.y + caret_local.max.y).max(caret_top.y + font_size * 1.1),
            );
            ui.painter()
                .line_segment([caret_top, caret_bot], Stroke::new(1.5, style.accent));
        }

        // Pointer click / drag -> UTF-16 cursor placement via `byte_to_utf16_offset`.
        if let Some(ord) = ordinal {
            if resp.clicked() {
                clicked_here = true;
                self.canvas_focused = true;
                if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                    let u16_off = pointer_to_utf16_offset(&galley, &rich.text, text_origin, ptr);
                    if ui.input(|i| i.modifiers.shift) && is_active {
                        self.state.set_selection(ord, cursor.anchor, u16_off);
                    } else {
                        self.state.set_cursor(ord, u16_off);
                    }
                } else {
                    self.state.set_active_text_by_ordinal(ord);
                }
            } else if resp.drag_started() {
                self.canvas_focused = true;
                if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                    let u16_off = pointer_to_utf16_offset(&galley, &rich.text, text_origin, ptr);
                    self.state.set_cursor(ord, u16_off);
                }
            } else if resp.dragged()
                && let Some(ptr) = ui.ctx().pointer_interact_pos()
            {
                let u16_off = pointer_to_utf16_offset(&galley, &rich.text, text_origin, ptr);
                let anchor = if self.state.cursor().leaf_ordinal == ord {
                    self.state.cursor().anchor
                } else {
                    u16_off
                };
                self.state.set_selection(ord, anchor, u16_off);
            }
        }

        clicked_here
    }
}

fn toolbar_button(
    ui: &mut Ui,
    label: &str,
    tooltip: &str,
    state: ToolbarActionState,
    style: &RichPageViewStyle,
) -> bool {
    let fill = if state.active {
        style.accent
    } else {
        Color32::TRANSPARENT
    };
    let text_color = if !state.enabled {
        style.muted_fg
    } else if state.active {
        Color32::WHITE
    } else {
        style.text_fg
    };
    let btn = egui::Button::new(RichText::new(label).size(12.0).color(text_color))
        .fill(fill)
        .corner_radius(CornerRadius::same(4));
    ui.add_enabled(state.enabled, btn)
        .on_hover_text(tooltip)
        .clicked()
}

/// Maps a pointer position on `galley` to a UTF-16 offset in `text` using [`byte_to_utf16_offset`].
fn pointer_to_utf16_offset(
    galley: &egui::Galley,
    text: &str,
    text_origin: Pos2,
    pointer: Pos2,
) -> usize {
    let local = pointer - text_origin;
    let ccursor = galley.cursor_from_pos(local);
    let char_idx: usize = ccursor.index.into();
    let byte_off = text
        .char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b);
    byte_to_utf16_offset(text, byte_off)
}

/// Paints selection background rectangles across `[char_start, char_end)` in `galley`.
fn paint_galley_selection_rects(
    painter: &egui::Painter,
    galley: &egui::Galley,
    origin: Pos2,
    char_start: usize,
    char_end: usize,
    fill: Color32,
) {
    if char_start >= char_end {
        return;
    }
    let start_pos = galley.pos_from_cursor(egui::text::CCursor::new(char_start));
    let end_pos = galley.pos_from_cursor(egui::text::CCursor::new(char_end));
    if (start_pos.min.y - end_pos.min.y).abs() < 2.0 {
        let r = Rect::from_min_max(
            origin + start_pos.min.to_vec2(),
            origin + Pos2::new(end_pos.max.x.max(start_pos.min.x + 2.0), end_pos.max.y).to_vec2(),
        );
        painter.rect_filled(r, CornerRadius::same(2), fill);
    } else {
        // Multi-line selection: paint line-by-line across covered rows.
        let mut cur_char = 0usize;
        for row in &galley.rows {
            let row_chars: usize = row.char_count_excluding_newline().into();
            let row_end = cur_char + row_chars;
            if row_end >= char_start && cur_char <= char_end {
                let lo = char_start.max(cur_char);
                let hi = char_end.min(row_end);
                let p0 = galley.pos_from_cursor(egui::text::CCursor::new(lo));
                let p1 = galley.pos_from_cursor(egui::text::CCursor::new(hi));
                let r = Rect::from_min_max(
                    origin + p0.min.to_vec2(),
                    origin + Pos2::new(p1.max.x.max(p0.min.x + 2.0), p1.max.y).to_vec2(),
                );
                painter.rect_filled(r, CornerRadius::same(2), fill);
            }
            cur_char = row_end + usize::from(row.ends_with_newline);
        }
    }
}

fn top_level_block_index(path: &BlockPath) -> usize {
    path.container
        .steps
        .first()
        .map_or(path.index, |step| step.block_index)
}

fn looks_like_html_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.contains('<') || !trimmed.contains('>') {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    const HTML_MARKERS: &[&str] = &[
        "<!doctype",
        "<html",
        "<body",
        "<article",
        "<p>",
        "<p ",
        "<h1>",
        "<h2>",
        "<h3>",
        "<h4>",
        "<ul>",
        "<ol>",
        "<li>",
        "<b>",
        "<strong>",
        "<i>",
        "<em>",
        "<a ",
        "<table",
        "<blockquote",
        "<pre",
        "<code>",
        "<div",
        "<span",
        "<br",
    ];
    HTML_MARKERS.iter().any(|tag| lower.contains(tag))
}
