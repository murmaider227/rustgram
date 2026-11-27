//! Live theme editor panel, searchable palette rows, fallback links, import/export,
//! and live cloud theme save (`account.uploadTheme` + `account.createTheme` / `updateTheme`).
//!
//! Port of:
//! - `Telegram/SourceFiles/window/themes/window_theme_editor.h` & `.cpp`
//! - `Telegram/SourceFiles/window/themes/window_theme_editor_block.h` & `.cpp`
//! - `Telegram/SourceFiles/window/themes/window_theme_editor_box.h` & `.cpp`

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Frame, Key, Margin, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use image::{ImageFormat, RgbaImage};
use tg_theme::{
    CloudTheme, Color, Colorizer, Palette, ParsedTheme, ThemeError, ThemeInstance,
    export_palette_for_editor, generate_default_palette, generate_theme_name, is_good_slug,
    load_color_scheme, pack_theme, parse_theme, read_palette_values, replace_adjustable_colors,
    replace_value_in_palette_content, save_theme_to_file, strip_cloud_text_fields,
    write_cloud_to_text,
};
use tg_ui::theme::{Colors, ThemeManager, theme_color_to_egui};

use crate::themes::backend::{
    CloudThemeCreateRequest, CloudThemeRecord, CloudThemeUpdateRequest, ThemesBackend,
};
use crate::themes::chat_background::ChatBackground;
use crate::themes::color_editor::{ColorEditorMode, ColorEditorState, draw_checkerboard};

/// Which list section a palette row belongs to (`EditorBlock::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorBlockKind {
    /// Defined explicitly in the active `.tdesktop-palette` content.
    Existing,
    /// Present in `colors.palette` (`Palette::ROWS`) but not yet overridden in the file.
    New,
}

/// One searchable color entry in the live theme editor (`EditorBlock::Row`).
#[derive(Debug, Clone)]
pub struct EditorRow {
    /// Original camelCase palette key name (e.g. `"windowBgActive"`).
    pub name: String,
    /// Parent key if defined as `"name: copy_of;"` (`= same as`), otherwise empty.
    pub copy_of: String,
    /// Resolved RGBA value.
    pub value: Color,
    /// Formatted lowercase `#rrggbb` or `#rrggbbaa`.
    pub value_hex: String,
    /// Human-readable description from `colors.palette` comments.
    pub description: String,
    /// Lowercase searchable token set (`name`, `copy_of`, `description`, `value_hex`).
    search_words: Vec<String>,
}

impl EditorRow {
    /// Constructs an `EditorRow` and builds its search index.
    #[must_use]
    pub fn new(name: String, copy_of: String, value: Color, description: String) -> Self {
        let mut row = Self {
            name,
            copy_of,
            value,
            value_hex: value.to_hex_string(),
            description,
            search_words: Vec::new(),
        };
        row.rebuild_search_words();
        row
    }

    /// Updates `value` and refreshes cached hex & search index.
    pub fn set_value(&mut self, value: Color) {
        self.value = value;
        self.value_hex = value.to_hex_string();
        self.rebuild_search_words();
    }

    /// Updates `copy_of` (`= same as`) alias and refreshes the search index.
    pub fn set_copy_of(&mut self, copy_of: String) {
        self.copy_of = copy_of;
        self.rebuild_search_words();
    }

    /// Checks whether all query tokens appear as substrings in this row's search words
    /// (`EditorBlock::Row::searchWordsContain`).
    #[must_use]
    pub fn matches_tokens(&self, tokens: &[String]) -> bool {
        tokens.iter().all(|needle| {
            self.search_words
                .iter()
                .any(|word| word.contains(needle.as_str()))
        })
    }

    fn rebuild_search_words(&mut self) {
        let combined = format!(
            "{} {} {} {}",
            self.name, self.copy_of, self.description, self.value_hex
        );
        self.search_words = split_search_words(&combined);
    }
}

/// Splits text using the exact `SearchSplitter` character class from `window_theme_editor_block.cpp`.
#[must_use]
pub fn split_search_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in input.chars() {
        let is_sep = ch.is_whitespace()
            || matches!(
                ch,
                '@' | '-'
                    | '+'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | '.'
                    | ':'
                    | '!'
                    | '_'
                    | ';'
                    | '"'
                    | '\''
                    | '\0'
                    | '#'
            );
        if is_sep {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current.to_ascii_lowercase());
    }
    words
}

/// State of the "Save Cloud Theme" dialog (`window_theme_editor_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct SaveCloudThemeBoxState {
    /// Whether the modal is currently open.
    pub open: bool,
    /// Theme display title (seeded via `tg_theme::generate_theme_name`).
    pub title: String,
    /// Theme share slug (`t.me/addtheme/<slug>`), validated with `tg_theme::is_good_slug`.
    pub slug: String,
    /// Include active chat background as tiled in the packed theme.
    pub tiled_background: bool,
    /// Encode packed background as PNG instead of JPEG.
    pub export_as_png: bool,
    /// Whether a live [`crate::themes::ThemesBackend`] is connected.
    pub cloud_connected: bool,
    /// Inline validation error or status notice.
    pub status_message: Option<String>,
}

/// Active color editing session inside [`ThemeEditorState`].
pub struct ActiveColorEdit {
    /// Which section (`Existing` or `New`) the edited key currently lives in.
    pub block: EditorBlockKind,
    /// Palette key name being edited.
    pub key_name: String,
    /// Candidate `"same as"` key chosen by clicking another existing row while editing.
    pub possible_copy_of: Option<String>,
    /// Embedded RGBA color picker widget state.
    pub picker: ColorEditorState,
}

/// Side-effect events emitted by [`ThemeEditorState::show`].
#[derive(Debug, Clone, Default)]
pub struct ThemeEditorOutput {
    /// Live palette changed and was pushed into `ThemeManager`.
    pub palette_changed: bool,
    /// The palette came from a theme file ("Load file"): the app tests it with "Keep this
    /// theme?" before keeping it (TGB-2984).
    pub imported: bool,
    /// User clicked "Save Cloud Theme" while connected to a live [`crate::themes::ThemesBackend`]
    /// without a backend reference passed directly to [`ThemeEditorState::show_with_backend`].
    pub save_cloud_requested: bool,
    /// Newly created or updated cloud theme record when saved directly via [`ThemesBackend`].
    pub saved_cloud_theme: Option<CloudThemeRecord>,
    /// Transient toast notification to display.
    pub toast: Option<String>,
    /// User clicked Close on the editor panel.
    pub close_requested: bool,
}

/// Live theme editor view-model and egui panel controller (`Window::Theme::Editor`).
pub struct ThemeEditorState {
    /// Raw `.tdesktop-palette` text currently being edited (preserves comments and ordering).
    palette_content: Vec<u8>,
    /// Snapshot of palette content before editing started (for Revert / dirty check).
    original_content: Vec<u8>,
    /// Snapshot of the `Palette` before editing started.
    original_palette: Palette,
    /// Cloud theme metadata associated with the current editing session.
    cloud: CloudTheme,
    /// Keys explicitly defined in `palette_content`.
    existing_rows: Vec<EditorRow>,
    /// Keys from `Palette::ROWS` not yet defined in `palette_content`.
    new_rows: Vec<EditorRow>,
    /// Search filter query string.
    pub search_query: String,
    /// Whether rows are sorted by HSL distance to `windowBgActive` (`sortByAccentDistance`).
    pub sort_by_accent: bool,
    /// Currently highlighted keyboard/click row index in the filtered list (`(block, row_idx)`).
    selected_row: Option<(EditorBlockKind, usize)>,
    /// Currently open color editor for a palette key.
    active_edit: Option<ActiveColorEdit>,
    /// Export path draft for `.tdesktop-theme` export bar.
    pub export_path: String,
    /// Import path draft for `.tdesktop-theme` / `.tdesktop-palette` import bar.
    pub import_path: String,
    /// Whether the inline import/export file bar is expanded.
    pub io_bar_open: bool,
    /// Cloud theme save dialog state.
    pub cloud_save: SaveCloudThemeBoxState,
    /// History stack of previous `palette_content` buffers for Undo.
    undo_stack: Vec<Vec<u8>>,
}

impl ThemeEditorState {
    /// Creates a new live theme editor initialized from the active `ThemeManager` palette.
    #[must_use]
    pub fn from_theme_manager(theme_manager: &ThemeManager) -> Self {
        let palette = theme_manager.palette().clone();
        let content = if palette == Palette::default() {
            generate_default_palette()
        } else {
            export_palette_for_editor(&palette)
        };
        Self::from_palette_content(content, palette, CloudTheme::default())
    }

    /// Creates a theme editor from explicit `.tdesktop-palette` bytes and resolved `Palette`.
    #[must_use]
    pub fn from_palette_content(
        palette_content: Vec<u8>,
        original_palette: Palette,
        cloud: CloudTheme,
    ) -> Self {
        let mut state = Self {
            original_content: palette_content.clone(),
            palette_content,
            original_palette,
            cloud,
            existing_rows: Vec::new(),
            new_rows: Vec::new(),
            search_query: String::new(),
            sort_by_accent: false,
            selected_row: None,
            active_edit: None,
            export_path: "custom_theme.tdesktop-theme".to_owned(),
            import_path: String::new(),
            io_bar_open: false,
            cloud_save: SaveCloudThemeBoxState::default(),
            undo_stack: Vec::new(),
        };
        state.rebuild_rows();
        state
    }

    /// Returns the raw `.tdesktop-palette` text currently maintained by the editor.
    #[must_use]
    pub fn palette_content(&self) -> &[u8] {
        &self.palette_content
    }

    /// Returns the associated [`CloudTheme`] metadata (non-zero `id` when editing an existing cloud theme).
    #[must_use]
    pub const fn cloud(&self) -> &CloudTheme {
        &self.cloud
    }

    /// Updates the associated [`CloudTheme`] metadata (e.g. after `account.createTheme`/`updateTheme` or loading a cloud theme).
    pub fn set_cloud(&mut self, cloud: CloudTheme) {
        self.cloud = cloud;
    }

    /// Returns whether any edits have been made compared to the opening snapshot.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.palette_content != self.original_content
    }

    /// Returns whether at least one Undo step is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns all rows currently in the `Existing` block.
    #[must_use]
    pub fn existing_rows(&self) -> &[EditorRow] {
        &self.existing_rows
    }

    /// Returns all rows currently in the `New` ("not in palette yet") block.
    #[must_use]
    pub fn new_rows(&self) -> &[EditorRow] {
        &self.new_rows
    }

    /// Returns the currently active color editor session, if any.
    #[must_use]
    pub fn active_edit(&self) -> Option<&ActiveColorEdit> {
        self.active_edit.as_ref()
    }

    /// Returns a mutable reference to the currently active color editor session, if any.
    pub fn active_edit_mut(&mut self) -> Option<&mut ActiveColorEdit> {
        self.active_edit.as_mut()
    }

    /// Looks up the current value of `key_name` across both blocks.
    #[must_use]
    pub fn find_color(&self, key_name: &str) -> Option<Color> {
        self.existing_rows
            .iter()
            .chain(self.new_rows.iter())
            .find(|r| r.name == key_name)
            .map(|r| r.value)
    }

    /// Returns indices of rows matching [`Self::search_query`] for `(existing, new)`.
    #[must_use]
    pub fn filtered_indices(&self) -> (Vec<usize>, Vec<usize>) {
        let tokens = split_search_words(&self.search_query);
        if tokens.is_empty() {
            return (
                (0..self.existing_rows.len()).collect(),
                (0..self.new_rows.len()).collect(),
            );
        }
        let ex = self
            .existing_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.matches_tokens(&tokens))
            .map(|(i, _)| i)
            .collect();
        let nw = self
            .new_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.matches_tokens(&tokens))
            .map(|(i, _)| i)
            .collect();
        (ex, nw)
    }

    /// Re-parses `palette_content` and `Palette::ROWS` into `existing_rows` and `new_rows`,
    /// preserving `colors.palette` comments and fallback relationships (`Editor::Inner::readData`).
    pub fn rebuild_rows(&mut self) {
        self.existing_rows.clear();
        self.new_rows.clear();

        let descriptions: BTreeMap<&'static str, &'static str> = Palette::ROWS
            .iter()
            .map(|row| (row.name, row.description))
            .collect();

        // 1. Parse existing definitions from palette_content
        let mut existing_index: BTreeMap<String, usize> = BTreeMap::new();
        let _ = read_palette_values(&self.palette_content, |name, raw_val| {
            let desc = descriptions.get(name).copied().unwrap_or("").to_owned();
            if raw_val.starts_with('#') {
                if let Some(color) = Color::from_hex(raw_val) {
                    Self::upsert_row(
                        &mut self.existing_rows,
                        &mut existing_index,
                        name.to_owned(),
                        String::new(),
                        color,
                        desc,
                    );
                }
            } else if raw_val != name
                && let Some(&src_idx) = existing_index.get(raw_val)
            {
                let src_color = self.existing_rows[src_idx].value;
                Self::upsert_row(
                    &mut self.existing_rows,
                    &mut existing_index,
                    name.to_owned(),
                    raw_val.to_owned(),
                    src_color,
                    desc,
                );
            }
            true
        });

        // 2. Add any Palette::ROWS keys missing from palette_content into `new_rows`
        let mut new_index: BTreeMap<String, usize> = BTreeMap::new();
        for row in Palette::ROWS {
            if existing_index.contains_key(row.name) {
                continue;
            }
            if row.value.starts_with('#') {
                let color = Color::from_hex(row.value).unwrap_or(row.default_color);
                let idx = self.new_rows.len();
                self.new_rows.push(EditorRow::new(
                    row.name.to_owned(),
                    row.fallback.to_owned(),
                    color,
                    row.description.to_owned(),
                ));
                new_index.insert(row.name.to_owned(), idx);
            } else {
                let copy_of = row.value.to_owned();
                let resolved = existing_index
                    .get(&copy_of)
                    .map(|&i| self.existing_rows[i].value)
                    .or_else(|| new_index.get(&copy_of).map(|&i| self.new_rows[i].value))
                    .unwrap_or(row.default_color);
                let idx = self.new_rows.len();
                self.new_rows.push(EditorRow::new(
                    row.name.to_owned(),
                    copy_of,
                    resolved,
                    row.description.to_owned(),
                ));
                new_index.insert(row.name.to_owned(), idx);
            }
        }

        if self.sort_by_accent {
            self.apply_accent_sort();
        }
    }

    fn upsert_row(
        rows: &mut Vec<EditorRow>,
        index: &mut BTreeMap<String, usize>,
        name: String,
        copy_of: String,
        value: Color,
        description: String,
    ) {
        if let Some(old_idx) = index.remove(&name) {
            rows.remove(old_idx);
            // Detach references to removed key and reindex subsequent rows.
            for row in rows.iter_mut().skip(old_idx) {
                if row.copy_of == name {
                    row.set_copy_of(String::new());
                }
            }
            index.clear();
            for (i, r) in rows.iter().enumerate() {
                index.insert(r.name.clone(), i);
            }
        }
        let idx = rows.len();
        rows.push(EditorRow::new(name.clone(), copy_of, value, description));
        index.insert(name, idx);
    }

    /// Toggles sorting by HSL hue/saturation distance from `windowBgActive`
    /// (`EditorBlock::sortByDistance`).
    pub fn set_sort_by_accent(&mut self, enabled: bool) {
        if self.sort_by_accent != enabled {
            self.sort_by_accent = enabled;
            self.rebuild_rows();
        }
    }

    fn apply_accent_sort(&mut self) {
        let accent = self
            .find_color("windowBgActive")
            .unwrap_or(Color::rgb(0x40, 0xA7, 0xE3));
        let to_hue = accent.to_hsl().h;
        let sort_key = |row: &EditorRow| -> i32 {
            if !row.copy_of.is_empty() {
                return 365;
            }
            let hsl = row.value.to_hsl();
            if hsl.h < 0 || to_hue < 0 {
                return 363;
            }
            let a = (hsl.h - to_hue).abs();
            let b = 360 + hsl.h - to_hue;
            let c = 360 + to_hue - hsl.h;
            if a.min(b).min(c) > 15 {
                return 363;
            }
            255 - hsl.s
        };
        self.existing_rows.sort_by_key(sort_key);
        self.new_rows.sort_by_key(sort_key);
    }

    /// Begins editing `key_name` in the embedded ColorEditor.
    ///
    /// If another key is ALREADY being edited and `key_name` is an earlier `Existing` key,
    /// clicking it instead links `"= same as"` (`possible_copy_of`) and loads its color into
    /// the open ColorEditor (exact port of `EditorBlock::activateRow`).
    pub fn activate_key(&mut self, key_name: &str) -> bool {
        if let Some((edit_key, edit_block)) = self
            .active_edit
            .as_ref()
            .map(|e| (e.key_name.clone(), e.block))
        {
            if edit_key != key_name
                && self.can_copy_from(&edit_key, edit_block, key_name)
                && let Some(src_color) = self.find_color(key_name)
                && let Some(ref mut edit) = self.active_edit
            {
                edit.possible_copy_of = Some(key_name.to_owned());
                edit.picker.show_color(src_color);
                return true;
            }
            return false;
        }

        if let Some(row) = self.existing_rows.iter().find(|r| r.name == key_name) {
            self.active_edit = Some(ActiveColorEdit {
                block: EditorBlockKind::Existing,
                key_name: row.name.clone(),
                possible_copy_of: if row.copy_of.is_empty() {
                    None
                } else {
                    Some(row.copy_of.clone())
                },
                picker: ColorEditorState::new(ColorEditorMode::Rgba, row.value),
            });
            return true;
        }
        if let Some(row) = self.new_rows.iter().find(|r| r.name == key_name) {
            self.active_edit = Some(ActiveColorEdit {
                block: EditorBlockKind::New,
                key_name: row.name.clone(),
                possible_copy_of: if row.copy_of.is_empty() {
                    None
                } else {
                    Some(row.copy_of.clone())
                },
                picker: ColorEditorState::new(ColorEditorMode::Rgba, row.value),
            });
            return true;
        }
        false
    }

    /// Returns `true` if `target_key` can legally reference `source_key` as `"= source_key"`
    /// (`EditorBlock::checkCopyOf`: `source_key` must appear earlier in `existing_rows`).
    #[must_use]
    pub fn can_copy_from(
        &self,
        target_key: &str,
        target_block: EditorBlockKind,
        source_key: &str,
    ) -> bool {
        if target_key == source_key {
            return false;
        }
        let Some(src_idx) = self.existing_rows.iter().position(|r| r.name == source_key) else {
            return false;
        };
        match target_block {
            EditorBlockKind::New => true,
            EditorBlockKind::Existing => self
                .existing_rows
                .iter()
                .position(|r| r.name == target_key)
                .is_some_and(|target_idx| target_idx > src_idx),
        }
    }

    /// Commits a color or `"same as"` reference edit for `key_name`, propagates to all
    /// dependent alias rows, updates `palette_content`, and applies the updated palette
    /// live to `theme_manager`.
    pub fn apply_color_edit(
        &mut self,
        key_name: &str,
        value: Color,
        copy_of: Option<&str>,
        theme_manager: &mut ThemeManager,
    ) -> Result<(), ThemeError> {
        let valid_copy = copy_of.filter(|src| {
            let block = if self.existing_rows.iter().any(|r| r.name == key_name) {
                EditorBlockKind::Existing
            } else {
                EditorBlockKind::New
            };
            self.can_copy_from(key_name, block, src)
                && self.find_color(src).is_some_and(|c| c == value)
        });

        let literal = valid_copy
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_hex_string());

        let updated_content =
            replace_value_in_palette_content(&self.palette_content, key_name, &literal)?;

        self.undo_stack.push(self.palette_content.clone());
        if self.undo_stack.len() > 64 {
            self.undo_stack.remove(0);
        }
        self.palette_content = updated_content;
        self.apply_content_to_theme_manager(theme_manager)?;

        // Update rows in place and cascade copies (`EditorBlock::checkCopiesChanged`)
        let copy_str = valid_copy.unwrap_or("").to_owned();
        if let Some(pos) = self.existing_rows.iter().position(|r| r.name == key_name) {
            self.existing_rows[pos].set_value(value);
            self.existing_rows[pos].set_copy_of(copy_str);
            self.propagate_copies(pos + 1, key_name, value);
        } else if let Some(new_pos) = self.new_rows.iter().position(|r| r.name == key_name) {
            let mut moved = self.new_rows.remove(new_pos);
            moved.set_value(value);
            moved.set_copy_of(copy_str);
            self.existing_rows.push(moved);
            self.propagate_copies(self.existing_rows.len(), key_name, value);
        } else {
            self.rebuild_rows();
        }
        Ok(())
    }

    fn propagate_copies(&mut self, start_existing: usize, root_name: &str, value: Color) {
        let mut changed_names: BTreeSet<String> = BTreeSet::new();
        changed_names.insert(root_name.to_owned());

        for row in self.existing_rows.iter_mut().skip(start_existing) {
            if !row.copy_of.is_empty() && changed_names.contains(&row.copy_of) {
                row.set_value(value);
                changed_names.insert(row.name.clone());
            }
        }
        for row in &mut self.new_rows {
            if !row.copy_of.is_empty() && changed_names.contains(&row.copy_of) {
                row.set_value(value);
                changed_names.insert(row.name.clone());
            }
        }
    }

    /// Commits the active ColorEditor dialog into the palette and closes it.
    pub fn commit_active_edit(
        &mut self,
        theme_manager: &mut ThemeManager,
    ) -> Result<bool, ThemeError> {
        let Some(edit) = self.active_edit.take() else {
            return Ok(false);
        };
        let color = edit.picker.color();
        self.apply_color_edit(
            &edit.key_name,
            color,
            edit.possible_copy_of.as_deref(),
            theme_manager,
        )?;
        Ok(true)
    }

    /// Cancels the active ColorEditor and restores live preview to the last committed palette.
    pub fn cancel_active_edit(&mut self, theme_manager: &mut ThemeManager) {
        if self.active_edit.take().is_some() {
            let _ = self.apply_content_to_theme_manager(theme_manager);
        }
    }

    /// Undoes the last committed palette edit and re-applies live preview.
    pub fn undo(&mut self, theme_manager: &mut ThemeManager) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        self.palette_content = prev;
        self.active_edit = None;
        let _ = self.apply_content_to_theme_manager(theme_manager);
        self.rebuild_rows();
        true
    }

    /// Reverts all edits back to the original snapshot taken when the editor opened.
    pub fn revert_all(&mut self, theme_manager: &mut ThemeManager) {
        self.palette_content = self.original_content.clone();
        self.undo_stack.clear();
        self.active_edit = None;
        theme_manager.set_palette(self.original_palette.clone());
        self.rebuild_rows();
    }

    /// Applies an ephemeral live preview of `key_name = color` onto `theme_manager` while
    /// dragging the ColorEditor (before clicking Save).
    pub fn preview_live_color(
        &self,
        key_name: &str,
        color: Color,
        theme_manager: &mut ThemeManager,
    ) {
        if let Ok(temp_content) = replace_value_in_palette_content(
            &self.palette_content,
            key_name,
            &color.to_hex_string(),
        ) {
            let mut pal = Palette::new_uninitialized();
            if load_color_scheme(&temp_content, &Colorizer::default(), &mut pal).is_ok() {
                pal.finalize();
                theme_manager.set_palette(pal);
            }
        }
    }

    fn apply_content_to_theme_manager(
        &self,
        theme_manager: &mut ThemeManager,
    ) -> Result<(), ThemeError> {
        let mut pal = Palette::new_uninitialized();
        load_color_scheme(&self.palette_content, &Colorizer::default(), &mut pal)?;
        pal.finalize();
        theme_manager.set_palette(pal);
        Ok(())
    }

    /// Builds a [`ParsedTheme`] containing the current palette (with cloud service header
    /// stripped and adjustable background colors updated) plus the active chat background
    /// encoded as PNG or JPEG (`CollectForExport` in `window_theme_editor_box.cpp`).
    pub fn collect_for_export(
        &self,
        theme_manager: &ThemeManager,
        chat_bg: &mut ChatBackground,
        as_png: bool,
        tiled: bool,
    ) -> Result<ParsedTheme, ThemeError> {
        let stripped = strip_cloud_text_fields(&self.palette_content);
        let adjusted_palette = replace_adjustable_colors(&stripped, theme_manager.palette())?;
        let bg_img = chat_bg.create_current_image(None, 512, 512);
        let bg_bytes = encode_background_bytes(&bg_img, as_png)?;
        Ok(ParsedTheme {
            palette: adjusted_palette,
            background: bg_bytes,
            is_png: as_png,
            tiled,
        })
    }

    /// Exports the current theme + background as a `.tdesktop-theme` ZIP archive on disk.
    pub fn export_to_file(
        &self,
        path: &Path,
        theme_manager: &ThemeManager,
        chat_bg: &mut ChatBackground,
        as_png: bool,
        tiled: bool,
    ) -> Result<usize, ThemeError> {
        let parsed = self.collect_for_export(theme_manager, chat_bg, as_png, tiled)?;
        let bytes = pack_theme(&parsed)?;
        let len = bytes.len();
        save_theme_to_file(path, &parsed)?;
        Ok(len)
    }

    /// Imports a `.tdesktop-theme` or `.tdesktop-palette` file from disk, updates editor rows,
    /// pushes the palette into `theme_manager`, and loads any bundled background into `chat_bg`.
    pub fn import_from_file(
        &mut self,
        path: &Path,
        theme_manager: &mut ThemeManager,
        chat_bg: &mut ChatBackground,
    ) -> Result<bool, ThemeError> {
        let bytes = std::fs::read(path)?;
        self.import_from_bytes(&bytes, theme_manager, chat_bg)
    }

    /// Imports a `.tdesktop-theme` or `.tdesktop-palette` from raw bytes (`Editor::importTheme`).
    pub fn import_from_bytes(
        &mut self,
        bytes: &[u8],
        theme_manager: &mut ThemeManager,
        chat_bg: &mut ChatBackground,
    ) -> Result<bool, ThemeError> {
        let parsed = parse_theme(bytes, false, None)?;
        // Validate syntax before mutating editor state
        let mut pal = Palette::new_uninitialized();
        load_color_scheme(&parsed.palette, &Colorizer::default(), &mut pal)?;
        pal.finalize();

        self.undo_stack.push(self.palette_content.clone());
        self.palette_content = parsed.palette;
        self.search_query.clear();
        self.active_edit = None;
        theme_manager.set_palette(pal);
        self.rebuild_rows();

        let mut loaded_bg = false;
        if !parsed.background.is_empty() {
            if ChatBackground::is_svg_or_tgv_pattern_bytes(&parsed.background) {
                if let Ok(mut cfg) = ChatBackground::load_pattern_from_bytes(
                    &parsed.background,
                    tg_theme::WallPaper::construct_default(),
                ) {
                    cfg.tiled = parsed.tiled || cfg.tiled;
                    chat_bg.set_global(cfg);
                    loaded_bg = true;
                }
            } else if let Ok(mut cfg) = ChatBackground::load_config_from_bytes(&parsed.background) {
                cfg.tiled = parsed.tiled;
                chat_bg.set_global(cfg);
                loaded_bg = true;
            }
        }
        Ok(loaded_bg)
    }

    /// Opens the "Save as Cloud Theme" dialog with an auto-generated theme name
    /// and slug matching `window_theme_editor_box.cpp`.
    pub fn open_cloud_save_dialog(&mut self, theme_manager: &ThemeManager, seed: u64) {
        let accent = theme_manager.palette().window_bg_active;
        let title = if self.cloud.title.is_empty() {
            generate_theme_name(accent, seed)
        } else {
            self.cloud.title.clone()
        };
        let slug = if self.cloud.slug.is_empty() {
            title
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
                + "_rustgram"
        } else {
            self.cloud.slug.clone()
        };
        let cloud_connected = self.cloud_save.cloud_connected;
        self.cloud_save = SaveCloudThemeBoxState {
            open: true,
            title,
            slug,
            tiled_background: false,
            export_as_png: false,
            cloud_connected,
            status_message: None,
        };
    }

    /// Validates `cloud_save` (`title` + `slug` via `tg_theme::is_good_slug`) and packs
    /// a cloud-headered `.tdesktop-theme` ZIP payload ready for `account.uploadTheme` +
    /// `account.createTheme` / `account.updateTheme`.
    pub fn build_cloud_theme_package(
        &mut self,
        theme_manager: &ThemeManager,
        chat_bg: &mut ChatBackground,
    ) -> Result<(String, String, Vec<u8>), String> {
        let title = self.cloud_save.title.trim().to_owned();
        if title.is_empty() {
            let err = "Theme title cannot be empty.".to_owned();
            self.cloud_save.status_message = Some(err.clone());
            return Err(err);
        }
        let slug = self.cloud_save.slug.trim().to_owned();
        if !is_good_slug(&slug) {
            let err = "Link slug must be 5-64 ASCII letters, digits, or underscores.".to_owned();
            self.cloud_save.status_message = Some(err.clone());
            return Err(err);
        }
        self.cloud.title = title.clone();
        self.cloud.slug = slug.clone();
        let mut parsed = self
            .collect_for_export(
                theme_manager,
                chat_bg,
                self.cloud_save.export_as_png,
                self.cloud_save.tiled_background,
            )
            .map_err(|e| e.to_string())?;
        // Prepend tdesktop cloud metadata header for round-trip compatibility
        let mut with_header = write_cloud_to_text(&self.cloud);
        with_header.extend_from_slice(&parsed.palette);
        parsed.palette = with_header;
        let zip_bytes = pack_theme(&parsed).map_err(|e| e.to_string())?;
        Ok((title, slug, zip_bytes))
    }

    /// Validates the cloud save form (`title` + `slug` via `tg_theme::is_good_slug`),
    /// packs a `.tdesktop-theme` archive, uploads it via `account.uploadTheme`, and publishes
    /// via `account.createTheme` (for a new cloud theme) or `account.updateTheme` (for an
    /// existing cloud theme).
    pub fn submit_cloud_save(
        &mut self,
        theme_manager: &ThemeManager,
        chat_bg: &mut ChatBackground,
        backend: &dyn ThemesBackend,
    ) -> Result<CloudThemeRecord, String> {
        let (title, slug, zip_bytes) = self.build_cloud_theme_package(theme_manager, chat_bg)?;
        let file_name = format!("{slug}.tdesktop-theme");

        let saved_res = if self.cloud.id != 0 && self.cloud.access_hash != 0 {
            backend.update_theme(&CloudThemeUpdateRequest {
                theme_id: self.cloud.id,
                access_hash: self.cloud.access_hash,
                slug: Some(slug),
                title: Some(title),
                theme_bytes: Some(zip_bytes),
                thumb_bytes: None,
                file_name,
                settings: Vec::new(),
            })
        } else {
            backend.create_theme(&CloudThemeCreateRequest {
                slug,
                title,
                theme_bytes: Some(zip_bytes),
                thumb_bytes: None,
                file_name,
                settings: Vec::new(),
            })
        };

        match saved_res {
            Ok(saved) => {
                self.cloud = saved.theme.clone();
                self.cloud_save.status_message = Some(format!(
                    "Saved cloud theme '{}' (t.me/addtheme/{})",
                    saved.theme.title, saved.theme.slug
                ));
                Ok(saved)
            }
            Err(err) => {
                self.cloud_save.status_message = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds a [`ThemeInstance`] snapshot of the current editor state (for live preview cards).
    #[must_use]
    pub fn snapshot_instance(
        &self,
        theme_manager: &ThemeManager,
        chat_bg: &mut ChatBackground,
    ) -> ThemeInstance {
        ThemeInstance {
            palette: theme_manager.palette().clone(),
            background: Some(chat_bg.create_current_image(None, 320, 240)),
            cached: Default::default(),
            tiled: chat_bg.global_config().tiled,
        }
    }

    /// Renders the complete live theme editor UI panel (`Window::Theme::Editor`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        theme_manager: &mut ThemeManager,
        chat_bg: &mut ChatBackground,
    ) -> ThemeEditorOutput {
        self.show_with_backend(ui, theme_manager, chat_bg, None)
    }

    /// Renders the complete live theme editor UI panel (`Window::Theme::Editor`) with an optional
    /// live [`ThemesBackend`] for direct `account.uploadTheme` + `createTheme`/`updateTheme` saves.
    pub fn show_with_backend(
        &mut self,
        ui: &mut Ui,
        theme_manager: &mut ThemeManager,
        chat_bg: &mut ChatBackground,
        backend: Option<&dyn ThemesBackend>,
    ) -> ThemeEditorOutput {
        let mut out = ThemeEditorOutput::default();
        let palette_snap = theme_manager.palette().clone();
        let colors: &dyn Colors = &palette_snap;

        let bg = colors.color("dialogsBg");
        let bg_over = colors.color("dialogsBgOver");
        let bg_active = colors.color("dialogsBgActive");
        let fg = colors.color("windowFg");
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_text = colors.color("windowActiveTextFg");
        let divider = colors.color("shadowFg");
        let accent_bg = colors.color("activeButtonBg");
        let accent_fg = colors.color("activeButtonFg");

        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);

        // Keyboard navigation (Up/Down/Enter/Escape)
        if self.active_edit.is_none() {
            if ui.input(|i| i.key_pressed(Key::Escape)) && !self.search_query.is_empty() {
                self.search_query.clear();
            }
            if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
                self.step_selection(1);
            }
            if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
                self.step_selection(-1);
            }
        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.cancel_active_edit(theme_manager);
            out.palette_changed = true;
        }

        Frame::NONE
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                // Header bar
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Theme Editor")
                            .size(16.0)
                            .strong()
                            .color(bold_fg),
                    );
                    if self.is_dirty() {
                        ui.label(
                            egui::RichText::new("• Modified")
                                .size(11.5)
                                .color(active_text),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            out.close_requested = true;
                        }
                        if ui
                            .add_enabled(self.can_undo(), egui::Button::new("Undo"))
                            .clicked()
                            && self.undo(theme_manager)
                        {
                            out.palette_changed = true;
                            out.toast = Some("Undid last palette edit".to_owned());
                        }
                        if ui
                            .add_enabled(self.is_dirty(), egui::Button::new("Revert"))
                            .clicked()
                        {
                            self.revert_all(theme_manager);
                            out.palette_changed = true;
                            out.toast = Some("Reverted theme changes".to_owned());
                        }
                    });
                });

                ui.add_space(6.0);

                // Search bar + Sort + Export/Import toggles
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .desired_width((ui.available_width() - 210.0).max(140.0))
                            .hint_text("Search 600+ keys, descriptions, #hex, = alias..."),
                    );
                    let mut sort_accent = self.sort_by_accent;
                    if ui.checkbox(&mut sort_accent, "By accent").changed() {
                        self.set_sort_by_accent(sort_accent);
                    }
                    if ui.button("Import / Export").clicked() {
                        self.io_bar_open = !self.io_bar_open;
                    }
                });

                // Optional Import / Export bar
                if self.io_bar_open {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(bg_over)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Export:").size(12.0).color(sub_fg));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.export_path)
                                        .desired_width(210.0),
                                );
                                if ui.button("Save .tdesktop-theme").clicked() {
                                    let p = Path::new(self.export_path.trim());
                                    match self.export_to_file(
                                        p,
                                        theme_manager,
                                        chat_bg,
                                        self.cloud_save.export_as_png,
                                        chat_bg.global_config().tiled,
                                    ) {
                                        Ok(n) => {
                                            out.toast = Some(format!(
                                                "Exported {} ({n} bytes)",
                                                p.display()
                                            ));
                                        }
                                        Err(e) => {
                                            out.toast = Some(format!("Export failed: {e}"));
                                        }
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Import:").size(12.0).color(sub_fg));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.import_path)
                                        .desired_width(210.0)
                                        .hint_text("Path to .tdesktop-theme or .tdesktop-palette"),
                                );
                                if ui.button("Load file").clicked() {
                                    let p = Path::new(self.import_path.trim()).to_path_buf();
                                    match self.import_from_file(&p, theme_manager, chat_bg) {
                                        Ok(has_bg) => {
                                            out.palette_changed = true;
                                            out.imported = true;
                                            out.toast = Some(if has_bg {
                                                "Imported theme palette + background".to_owned()
                                            } else {
                                                "Imported theme palette".to_owned()
                                            });
                                        }
                                        Err(e) => {
                                            out.toast = Some(format!("Import failed: {e}"));
                                        }
                                    }
                                }
                            });
                        });
                }

                // Active inline ColorEditor card (stays docked at top while user can also click
                // any earlier existing row below to pick "= same as" fallback!)
                if self.active_edit.is_some() {
                    ui.add_space(8.0);
                    let mut live_preview: Option<(String, Color)> = None;
                    let mut do_save = false;
                    let mut do_cancel = false;
                    let mut clear_copy_of = false;

                    Frame::NONE
                        .fill(bg_over)
                        .stroke(Stroke::new(1.0, divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            if let Some(ref mut edit) = self.active_edit {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(&edit.key_name)
                                            .size(14.0)
                                            .strong()
                                            .color(bold_fg),
                                    );
                                    if let Some(ref src) = edit.possible_copy_of {
                                        ui.label(
                                            egui::RichText::new(format!("= {src}"))
                                                .size(12.0)
                                                .color(active_text),
                                        );
                                        if ui.small_button("Unlink").clicked() {
                                            clear_copy_of = true;
                                        }
                                    } else {
                                        ui.label(
                                            egui::RichText::new(
                                                "(click an earlier row below to link '= same as')",
                                            )
                                            .size(11.0)
                                            .color(sub_fg),
                                        );
                                    }
                                });
                                ui.add_space(6.0);
                                if edit.picker.show(ui, colors) {
                                    // Direct slider/field change breaks "= same as" link
                                    edit.possible_copy_of = None;
                                    live_preview =
                                        Some((edit.key_name.clone(), edit.picker.color()));
                                }
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    let save_btn = egui::Button::new(
                                        egui::RichText::new("Save Color").color(accent_fg).strong(),
                                    )
                                    .fill(accent_bg);
                                    if ui.add(save_btn).clicked() {
                                        do_save = true;
                                    }
                                    if ui.button("Cancel").clicked() {
                                        do_cancel = true;
                                    }
                                    ui.label(
                                        egui::RichText::new(edit.picker.color().to_hex_string())
                                            .font(FontId::monospace(12.0))
                                            .color(sub_fg),
                                    );
                                });
                            }
                        });

                    if clear_copy_of && let Some(ref mut edit) = self.active_edit {
                        edit.possible_copy_of = None;
                    }
                    if let Some((key, col)) = live_preview {
                        self.preview_live_color(&key, col, theme_manager);
                        out.palette_changed = true;
                    }
                    if do_save {
                        match self.commit_active_edit(theme_manager) {
                            Ok(true) => {
                                out.palette_changed = true;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                out.toast = Some(format!("Failed to save color: {e}"));
                            }
                        }
                    } else if do_cancel {
                        self.cancel_active_edit(theme_manager);
                        out.palette_changed = true;
                    }
                }

                // Cloud save dialog (`account.uploadTheme` + `createTheme`/`updateTheme` when live)
                if self.cloud_save.open {
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(bg_over)
                        .stroke(Stroke::new(1.0, active_text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            let is_live = backend.is_some() || self.cloud_save.cloud_connected;
                            let heading = if is_live {
                                if self.cloud.id != 0 {
                                    "Update Cloud Theme"
                                } else {
                                    "Save Cloud Theme"
                                }
                            } else {
                                "Save Cloud Theme (Offline)"
                            };
                            ui.label(
                                egui::RichText::new(heading)
                                    .size(14.0)
                                    .strong()
                                    .color(bold_fg),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Title:").size(12.0).color(sub_fg));
                                ui.text_edit_singleline(&mut self.cloud_save.title);
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("t.me/addtheme/")
                                        .size(12.0)
                                        .color(sub_fg),
                                );
                                ui.text_edit_singleline(&mut self.cloud_save.slug);
                            });
                            ui.horizontal(|ui| {
                                ui.checkbox(
                                    &mut self.cloud_save.tiled_background,
                                    "Tile background",
                                );
                                ui.checkbox(&mut self.cloud_save.export_as_png, "Encode PNG");
                            });
                            if let Some(ref msg) = self.cloud_save.status_message {
                                ui.label(egui::RichText::new(msg).size(11.5).color(active_text));
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if is_live {
                                    let btn_lbl = if self.cloud.id != 0 {
                                        "Update Cloud Theme"
                                    } else {
                                        "Save Cloud Theme"
                                    };
                                    if ui.button(btn_lbl).clicked() {
                                        if let Some(be) = backend {
                                            match self.submit_cloud_save(theme_manager, chat_bg, be)
                                            {
                                                Ok(saved) => {
                                                    out.toast = Some(format!(
                                                        "Saved cloud theme '{}' (t.me/addtheme/{})",
                                                        saved.theme.title, saved.theme.slug
                                                    ));
                                                    out.saved_cloud_theme = Some(saved);
                                                }
                                                Err(err) => {
                                                    out.toast = tg_app_core::user_error::save_cloud_theme(&err).into_text();
                                                }
                                            }
                                        } else {
                                            out.save_cloud_requested = true;
                                        }
                                    }
                                } else if ui.button("Prepare Local Package").clicked() {
                                    match self.build_cloud_theme_package(theme_manager, chat_bg) {
                                        Ok((_title, slug, zip_bytes)) => {
                                            self.cloud_save.status_message = Some(format!(
                                                "Theme package ready ({} bytes, t.me/addtheme/{slug}). Sign in to publish to Telegram Cloud, or use Export to save locally.",
                                                zip_bytes.len()
                                            ));
                                            out.toast =
                                                Some("Local theme package ready".to_owned());
                                        }
                                        Err(err) => {
                                            out.toast = Some(err);
                                        }
                                    }
                                }
                                if ui.button("Close").clicked() {
                                    self.cloud_save.open = false;
                                }
                            });
                        });
                }

                ui.add_space(8.0);
                let (existing_indices, new_indices) = self.filtered_indices();
                ui.label(
                    egui::RichText::new(format!(
                        "{} palette keys ({} defined, {} defaults)",
                        existing_indices.len() + new_indices.len(),
                        existing_indices.len(),
                        new_indices.len()
                    ))
                    .size(11.5)
                    .color(sub_fg),
                );
                ui.add_space(4.0);

                // Scrollable palette key list
                let list_h = (ui.available_height() - 44.0).max(160.0);
                let mut clicked_key: Option<String> = None;
                let mut jump_to_key: Option<String> = None;
                let editing_key = self.active_edit.as_ref().map(|e| e.key_name.clone());
                let editing_block = self.active_edit.as_ref().map(|e| e.block);

                ScrollArea::vertical()
                    .id_salt("tg_theme_editor_rows")
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if existing_indices.is_empty() && new_indices.is_empty() {
                            ui.add_space(24.0);
                            ui.label(
                                egui::RichText::new("No matching palette colors found.")
                                    .size(13.0)
                                    .color(sub_fg),
                            );
                        }

                        for &idx in &existing_indices {
                            let row = &self.existing_rows[idx];
                            let is_active = editing_key.as_deref() == Some(row.name.as_str());
                            let is_dimmed = match (&editing_key, editing_block) {
                                (Some(ek), Some(eb)) if !is_active => {
                                    !self.can_copy_from(ek, eb, &row.name)
                                }
                                _ => false,
                            };
                            let (row_clicked, link_clicked) = paint_editor_row(
                                ui,
                                row,
                                is_active,
                                is_dimmed,
                                bg,
                                bg_over,
                                bg_active,
                                fg,
                                sub_fg,
                                active_text,
                                divider,
                            );
                            if let Some(target) = link_clicked {
                                jump_to_key = Some(target);
                            } else if row_clicked && !is_dimmed {
                                clicked_key = Some(row.name.clone());
                            }
                        }

                        if !new_indices.is_empty() {
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("Not in palette yet (defaults)")
                                    .size(13.0)
                                    .strong()
                                    .color(bold_fg),
                            );
                            ui.add_space(4.0);
                            for &idx in &new_indices {
                                let row = &self.new_rows[idx];
                                let is_active = editing_key.as_deref() == Some(row.name.as_str());
                                let is_dimmed = editing_key.is_some() && !is_active;
                                let (row_clicked, link_clicked) = paint_editor_row(
                                    ui,
                                    row,
                                    is_active,
                                    is_dimmed,
                                    bg,
                                    bg_over,
                                    bg_active,
                                    fg,
                                    sub_fg,
                                    active_text,
                                    divider,
                                );
                                if let Some(target) = link_clicked {
                                    jump_to_key = Some(target);
                                } else if row_clicked && !is_dimmed {
                                    clicked_key = Some(row.name.clone());
                                }
                            }
                        }
                    });

                if let Some(target) = jump_to_key {
                    self.search_query = target;
                } else if let Some(key) = clicked_key {
                    let was_editing = self.active_edit.is_some();
                    if self.activate_key(&key) && was_editing {
                        if let Some(ref edit) = self.active_edit {
                            let preview_col = edit.picker.color();
                            let edit_key = edit.key_name.clone();
                            self.preview_live_color(&edit_key, preview_col, theme_manager);
                            out.palette_changed = true;
                        }
                        out.toast = Some(format!("Linked '= {key}'"));
                    }
                }

                // Bottom Save Theme footer bar (`_save` button in `window_theme_editor.cpp`)
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let save_cloud = egui::Button::new(
                        egui::RichText::new("Save Cloud Theme...")
                            .size(13.0)
                            .strong()
                            .color(accent_fg),
                    )
                    .fill(accent_bg)
                    .min_size(Vec2::new(160.0, 32.0));
                    if ui.add(save_cloud).clicked() {
                        self.open_cloud_save_dialog(theme_manager, 42);
                    }
                    if ui
                        .add(
                            egui::Button::new("Export .tdesktop-theme")
                                .min_size(Vec2::new(150.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.io_bar_open = true;
                    }
                });
            });

        out
    }

    fn step_selection(&mut self, delta: isize) {
        let (ex, nw) = self.filtered_indices();
        let total = ex.len() + nw.len();
        if total == 0 {
            self.selected_row = None;
            return;
        }
        let flat: Vec<(EditorBlockKind, usize)> = ex
            .into_iter()
            .map(|i| (EditorBlockKind::Existing, i))
            .chain(nw.into_iter().map(|i| (EditorBlockKind::New, i)))
            .collect();
        let cur = self
            .selected_row
            .and_then(|sel| flat.iter().position(|&item| item == sel))
            .map_or(0isize, |i| i as isize);
        let next = (cur + delta).clamp(0, (flat.len() - 1) as isize) as usize;
        self.selected_row = Some(flat[next]);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_editor_row(
    ui: &mut Ui,
    row: &EditorRow,
    is_active: bool,
    is_dimmed: bool,
    bg: Color32,
    bg_over: Color32,
    bg_active: Color32,
    fg: Color32,
    sub_fg: Color32,
    active_text: Color32,
    divider: Color32,
) -> (bool, Option<String>) {
    let width = ui.available_width();
    let has_desc = !row.description.is_empty();
    let row_h = if has_desc { 56.0 } else { 38.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
    if resp.hovered() && !is_dimmed {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let fill = if is_active {
        bg_active
    } else if resp.hovered() && !is_dimmed {
        bg_over
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);

    let name_col = if is_active { Color32::WHITE } else { fg };
    let desc_col = if is_active {
        Color32::from_rgba_unmultiplied(255, 255, 255, 210)
    } else {
        sub_fg
    };
    let alias_col = if is_active {
        Color32::WHITE
    } else {
        active_text
    };

    // Right-aligned color swatch with checkerboard behind alpha colors
    let swatch_size = Vec2::new(34.0, 22.0);
    let swatch_rect = Rect::from_min_size(
        Pos2::new(rect.right() - swatch_size.x - 10.0, rect.top() + 8.0),
        swatch_size,
    );
    if row.value.a < 255 {
        draw_checkerboard(ui, swatch_rect, 4.0);
    }
    ui.painter().rect_filled(
        swatch_rect,
        CornerRadius::same(4),
        theme_color_to_egui(row.value),
    );
    ui.painter().rect_stroke(
        swatch_rect,
        CornerRadius::same(4),
        Stroke::new(1.0, divider),
        StrokeKind::Inside,
    );

    // Key name + hex badge
    let left_x = rect.left() + 10.0;
    ui.painter().text(
        Pos2::new(left_x, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        &row.name,
        FontId::proportional(13.0),
        name_col,
    );

    // Hex label next to swatch
    ui.painter().text(
        Pos2::new(swatch_rect.left() - 8.0, rect.top() + 11.0),
        egui::Align2::RIGHT_TOP,
        &row.value_hex,
        FontId::monospace(11.0),
        desc_col,
    );

    // Fallback / "= copyOf" link
    let mut link_clicked = None;
    if !row.copy_of.is_empty() {
        let alias_label = format!("= {}", row.copy_of);
        let alias_pos = Pos2::new(left_x, rect.top() + 23.0);
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(alias_label.clone(), FontId::proportional(11.5), alias_col)
        });
        let link_rect = Rect::from_min_size(alias_pos, galley.size());
        ui.painter().galley(alias_pos, galley, alias_col);
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && link_rect.expand(2.0).contains(pos)
        {
            link_clicked = Some(row.copy_of.clone());
        }
    }

    // Description from `colors.palette` comment
    if has_desc {
        let desc_y = if row.copy_of.is_empty() {
            rect.top() + 26.0
        } else {
            rect.top() + 38.0
        };
        let max_desc_w = (width - 24.0).max(80.0);
        let galley = ui.fonts_mut(|f| {
            f.layout(
                row.description.clone(),
                FontId::proportional(11.0),
                desc_col,
                max_desc_w,
            )
        });
        ui.painter()
            .galley(Pos2::new(left_x, desc_y), galley, desc_col);
    }

    if is_dimmed {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 150),
        );
    }

    ui.painter().line_segment(
        [
            Pos2::new(rect.left() + 10.0, rect.bottom()),
            Pos2::new(rect.right() - 10.0, rect.bottom()),
        ],
        Stroke::new(0.5, divider),
    );

    (resp.clicked(), link_clicked)
}

fn encode_background_bytes(img: &RgbaImage, as_png: bool) -> Result<Vec<u8>, ThemeError> {
    let mut cursor = Cursor::new(Vec::new());
    if as_png {
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut cursor, ImageFormat::Png)
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
    } else {
        let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut cursor, ImageFormat::Jpeg)
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
    }
    Ok(cursor.into_inner())
}
