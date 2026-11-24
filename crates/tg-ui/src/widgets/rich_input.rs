//! Rich entity-aware text input model, formatting shortcuts, live styled `TextEdit` layouter,
//! formatting context menu, and link dialog.
//!
//! Port of `Telegram/lib_ui/ui/widgets/fields/input_field.cpp` (markdown tag editing, shortcuts,
//! rich paste, custom emoji & mention tags, live char-format rendering) and
//! `Telegram/SourceFiles/chat_helpers/message_field.cpp` (`EditLinkBox`, URL validation,
//! `PrepareMentionTag`, markdown conversion on send).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::{
    Area, Color32, CornerRadius, Frame, Galley, Id, Key, Margin, Order, Pos2, Stroke, TextBuffer,
    TextEdit, Ui,
};
use tg_style::FontFlags;
use tg_text::{
    EntityInText, EntityType, MentionNameFields, OffsetMapper, TextParseFlags, TextWithEntities,
    mention_name_data_from_fields, parse_entities, parse_markdown, utf8_to_utf16_offset, utf16_len,
    utf16_to_utf8_offset,
};

use crate::fonts::font_id_for_flags;

/// Formatting shortcut / context-menu action supported by [`RichEditState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatAction {
    /// Toggle bold (`Ctrl+B`).
    Bold,
    /// Toggle italic (`Ctrl+I`).
    Italic,
    /// Toggle underline (`Ctrl+U`).
    Underline,
    /// Toggle strikethrough (`Ctrl+Shift+X`).
    StrikeOut,
    /// Toggle monospace inline code or multi-line code block (`Ctrl+Shift+M`).
    Monospace,
    /// Toggle spoiler (`Ctrl+Shift+P`).
    Spoiler,
    /// Toggle blockquote (`Ctrl+Shift+.`).
    Blockquote,
    /// Open or edit hyperlink dialog (`Ctrl+K`).
    CreateLink,
    /// Clear all formatting entities in selection or reset active typing style (`Ctrl+Shift+N`).
    ClearFormatting,
}

impl FormatAction {
    /// Human-readable menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::Underline => "Underline",
            Self::StrikeOut => "Strikethrough",
            Self::Monospace => "Monospace",
            Self::Spoiler => "Spoiler",
            Self::Blockquote => "Quote",
            Self::CreateLink => "Create Link...",
            Self::ClearFormatting => "Clear Formatting",
        }
    }

    /// Shortcut hint string displayed in the formatting context menu.
    #[must_use]
    pub const fn shortcut_hint(self) -> &'static str {
        match self {
            Self::Bold => "Ctrl+B",
            Self::Italic => "Ctrl+I",
            Self::Underline => "Ctrl+U",
            Self::StrikeOut => "Ctrl+Shift+X",
            Self::Monospace => "Ctrl+Shift+M",
            Self::Spoiler => "Ctrl+Shift+P",
            Self::Blockquote => "Ctrl+Shift+.",
            Self::CreateLink => "Ctrl+K",
            Self::ClearFormatting => "Ctrl+Shift+N",
        }
    }

    /// All formatting menu items in Telegram Desktop order.
    pub const MENU_ITEMS: [Self; 9] = [
        Self::Bold,
        Self::Italic,
        Self::Underline,
        Self::StrikeOut,
        Self::Monospace,
        Self::Spoiler,
        Self::Blockquote,
        Self::CreateLink,
        Self::ClearFormatting,
    ];
}

/// Active inline typing format flags when the caret is collapsed (no selection).
///
/// Mirrors `InputField::_defaultCharFormat` toggled by `toggleCurrentMarkdownTag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActiveFormatFlags {
    /// Bold active for newly typed characters.
    pub bold: bool,
    /// Italic active for newly typed characters.
    pub italic: bool,
    /// Underline active for newly typed characters.
    pub underline: bool,
    /// Strikethrough active for newly typed characters.
    pub strike_out: bool,
    /// Monospace inline code active for newly typed characters (exclusive with inline styles).
    pub monospace: bool,
    /// Spoiler active for newly typed characters.
    pub spoiler: bool,
    /// Blockquote active for newly typed characters.
    pub blockquote: bool,
}

impl ActiveFormatFlags {
    /// Clears all active typing format flags.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns `true` if no typing flags are enabled.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.bold
            && !self.italic
            && !self.underline
            && !self.strike_out
            && !self.monospace
            && !self.spoiler
            && !self.blockquote
    }

    /// Toggles one [`FormatAction`] flag, respecting tdesktop's separate-tag rule
    /// (`Code` clears inline bold/italic/underline/strike/spoiler, and vice versa).
    pub fn toggle(&mut self, action: FormatAction) {
        match action {
            FormatAction::Bold => {
                self.bold = !self.bold;
                if self.bold {
                    self.monospace = false;
                }
            }
            FormatAction::Italic => {
                self.italic = !self.italic;
                if self.italic {
                    self.monospace = false;
                }
            }
            FormatAction::Underline => {
                self.underline = !self.underline;
                if self.underline {
                    self.monospace = false;
                }
            }
            FormatAction::StrikeOut => {
                self.strike_out = !self.strike_out;
                if self.strike_out {
                    self.monospace = false;
                }
            }
            FormatAction::Spoiler => {
                self.spoiler = !self.spoiler;
                if self.spoiler {
                    self.monospace = false;
                }
            }
            FormatAction::Monospace => {
                self.monospace = !self.monospace;
                if self.monospace {
                    self.bold = false;
                    self.italic = false;
                    self.underline = false;
                    self.strike_out = false;
                    self.spoiler = false;
                }
            }
            FormatAction::Blockquote => {
                self.blockquote = !self.blockquote;
            }
            FormatAction::ClearFormatting => {
                self.clear();
            }
            FormatAction::CreateLink => {}
        }
    }

    /// Returns the list of `EntityType`s represented by currently enabled flags.
    #[must_use]
    pub fn entity_types(self) -> Vec<EntityType> {
        let mut out = Vec::new();
        if self.blockquote {
            out.push(EntityType::Blockquote);
        }
        if self.monospace {
            out.push(EntityType::Code);
            return out;
        }
        if self.bold {
            out.push(EntityType::Bold);
        }
        if self.italic {
            out.push(EntityType::Italic);
        }
        if self.underline {
            out.push(EntityType::Underline);
        }
        if self.strike_out {
            out.push(EntityType::StrikeOut);
        }
        if self.spoiler {
            out.push(EntityType::Spoiler);
        }
        out
    }
}

/// Colors used by [`build_rich_edit_layout_job`] to style live entities inside the compose field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichInputPalette {
    /// Normal body text color.
    pub text_fg: Color32,
    /// Link / mention / hashtag text color.
    pub link_fg: Color32,
    /// Inline monospace code foreground color.
    pub mono_fg: Color32,
    /// Inline monospace code pill background tint.
    pub mono_bg: Color32,
    /// Spoiler preview background tint (text stays editable underneath).
    pub spoiler_bg: Color32,
    /// Spoiler preview foreground tint.
    pub spoiler_fg: Color32,
    /// Blockquote background tint.
    pub quote_bg: Color32,
    /// Custom emoji pill highlight background.
    pub custom_emoji_bg: Color32,
}

impl RichInputPalette {
    /// Sensible light/dark palette derived from foreground, link accent, and theme polarity.
    #[must_use]
    pub fn from_colors(text_fg: Color32, link_fg: Color32, is_dark: bool) -> Self {
        if is_dark {
            Self {
                text_fg,
                link_fg,
                mono_fg: Color32::from_rgb(0x9C, 0xD4, 0xFF),
                mono_bg: Color32::from_rgba_unmultiplied(110, 170, 230, 34),
                spoiler_bg: Color32::from_rgba_unmultiplied(180, 196, 214, 58),
                spoiler_fg: Color32::from_rgb(0xC8, 0xD6, 0xE5),
                quote_bg: Color32::from_rgba_unmultiplied(100, 165, 225, 28),
                custom_emoji_bg: Color32::from_rgba_unmultiplied(245, 198, 72, 42),
            }
        } else {
            Self {
                text_fg,
                link_fg,
                mono_fg: Color32::from_rgb(0x16, 0x62, 0x99),
                mono_bg: Color32::from_rgba_unmultiplied(24, 118, 200, 26),
                spoiler_bg: Color32::from_rgba_unmultiplied(120, 134, 150, 56),
                spoiler_fg: Color32::from_rgb(0x4A, 0x55, 0x62),
                quote_bg: Color32::from_rgba_unmultiplied(64, 167, 227, 26),
                custom_emoji_bg: Color32::from_rgba_unmultiplied(245, 180, 36, 48),
            }
        }
    }
}

/// Core entity-preserving rich text editing model for Telegram Desktop compose fields.
///
/// Tracks plain UTF-8 text alongside normalized UTF-16 [`EntityInText`] spans so formatting
/// shortcuts, live typing, rich paste, custom emoji, mention-name insertions, and draft
/// save/restore all keep entity offsets exact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichEditState {
    /// Current editable text buffer.
    pub text: String,
    /// Active UTF-16 entities over [`Self::text`].
    pub entities: Vec<EntityInText>,
    /// Active inline format flags applied to newly typed characters when caret is collapsed.
    pub active_formats: ActiveFormatFlags,
    /// Last observed selection range in Unicode scalar (char) indices `[start, end)`.
    pub selection_chars: (usize, usize),
    /// Last observed caret position in Unicode scalar (char) index.
    pub cursor_char: usize,
    /// Pending caret placement to push back into egui `TextEditState` on next frame.
    pub pending_cursor_char: Option<usize>,
    /// Optional friendly labels / glyph overrides for custom emoji document IDs.
    pub custom_emoji_labels: HashMap<u64, String>,
}

impl RichEditState {
    /// Creates an empty [`RichEditState`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Initializes from an existing [`TextWithEntities`] (e.g. draft restore or editing a message).
    #[must_use]
    pub fn from_text_with_entities(mut src: TextWithEntities) -> Self {
        let len_u16 = src.utf16_len();
        src.entities.retain(|e| e.valid_for_text(len_u16));
        src.sort_entities();
        let char_len = src.text.chars().count();
        Self {
            text: src.text,
            entities: src.entities,
            active_formats: ActiveFormatFlags::default(),
            selection_chars: (char_len, char_len),
            cursor_char: char_len,
            pending_cursor_char: Some(char_len),
            custom_emoji_labels: HashMap::new(),
        }
    }

    /// Replaces the entire buffer with `src` (preserving all entities).
    pub fn set_text_with_entities(&mut self, src: TextWithEntities) {
        let labels = std::mem::take(&mut self.custom_emoji_labels);
        *self = Self::from_text_with_entities(src);
        self.custom_emoji_labels = labels;
    }

    /// Returns a snapshot of the live text and entities (used for draft saving & rich copy).
    #[must_use]
    pub fn to_text_with_entities(&self) -> TextWithEntities {
        let mut out = TextWithEntities {
            text: self.text.clone(),
            entities: self.entities.clone(),
        };
        out.sort_entities();
        out
    }

    /// Clears all text, entities, and active typing flags.
    pub fn clear(&mut self) {
        self.text.clear();
        self.entities.clear();
        self.active_formats.clear();
        self.selection_chars = (0, 0);
        self.cursor_char = 0;
        self.pending_cursor_char = Some(0);
    }

    /// Returns `true` when the text buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns the normalized `(start, end)` char selection range (`start <= end`).
    #[must_use]
    pub fn normalized_selection(&self) -> (usize, usize) {
        let max_c = self.text.chars().count();
        let a = self.selection_chars.0.min(max_c);
        let b = self.selection_chars.1.min(max_c);
        (a.min(b), a.max(b))
    }

    /// Updates cursor & selection from an egui [`CCursorRange`].
    pub fn sync_cursor_range(&mut self, range: CCursorRange) {
        let [lo, hi] = range.sorted_cursors();
        self.selection_chars = (lo.index.into(), hi.index.into());
        self.cursor_char = range.primary.index.into();
    }

    /// Synchronizes entity spans after `self.text` changed from `old_text` to `self.text`.
    ///
    /// Computes the minimal common prefix and common suffix in Unicode scalars, then maps the
    /// changed middle slice to UTF-16 offsets so every surviving entity shifts, shrinks, or
    /// expands accurately. Newly typed characters inherit [`Self::active_formats`] (or the
    /// enclosing formatting span).
    pub fn apply_text_diff(&mut self, old_text: &str) {
        if old_text == self.text {
            return;
        }
        let old_chars: Vec<char> = old_text.chars().collect();
        let new_chars: Vec<char> = self.text.chars().collect();

        let mut prefix = 0usize;
        let min_len = old_chars.len().min(new_chars.len());
        while prefix < min_len && old_chars[prefix] == new_chars[prefix] {
            prefix += 1;
        }

        let mut suffix = 0usize;
        while suffix < (old_chars.len() - prefix)
            && suffix < (new_chars.len() - prefix)
            && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
        {
            suffix += 1;
        }

        let edit_start_u16: i32 = old_chars[..prefix]
            .iter()
            .map(|c| c.len_utf16() as i32)
            .sum();
        let old_removed_u16: i32 = old_chars[prefix..(old_chars.len() - suffix)]
            .iter()
            .map(|c| c.len_utf16() as i32)
            .sum();
        let new_inserted_u16: i32 = new_chars[prefix..(new_chars.len() - suffix)]
            .iter()
            .map(|c| c.len_utf16() as i32)
            .sum();
        let old_edit_end_u16 = edit_start_u16 + old_removed_u16;
        let delta = new_inserted_u16 - old_removed_u16;

        let mut updated = Vec::with_capacity(self.entities.len() + 4);
        for ent in &self.entities {
            let e_start = ent.offset();
            let e_end = e_start + ent.length();
            let ty = ent.entity_type();
            let atomic = matches!(ty, EntityType::CustomEmoji | EntityType::MentionName);

            if e_end <= edit_start_u16 {
                // Entirely before edit.
                // Expand inline formatting if typing directly at the trailing edge of a non-atomic
                // span when no explicit active_formats override is set.
                if !atomic
                    && old_removed_u16 == 0
                    && new_inserted_u16 > 0
                    && e_end == edit_start_u16
                    && self.active_formats.is_empty()
                    && is_inheritable_inline_entity(ty)
                    && !new_chars[prefix..(new_chars.len() - suffix)]
                        .iter()
                        .all(|c| c.is_whitespace())
                {
                    updated.push(EntityInText::with_data(
                        ty,
                        e_start,
                        ent.length() + new_inserted_u16,
                        ent.data(),
                    ));
                } else {
                    updated.push(ent.clone());
                }
            } else if e_start >= old_edit_end_u16 {
                // Entirely after edit: shift by delta.
                let new_off = (e_start + delta).max(0);
                updated.push(EntityInText::with_data(
                    ty,
                    new_off,
                    ent.length(),
                    ent.data(),
                ));
            } else if atomic {
                // Atomic entities (CustomEmoji, MentionName) drop if their characters are mutated.
                continue;
            } else {
                // Overlaps the edited range.
                let new_start = if e_start < edit_start_u16 {
                    e_start
                } else {
                    edit_start_u16
                };
                let new_end = if e_end > old_edit_end_u16 {
                    e_end + delta
                } else if e_start < edit_start_u16 {
                    edit_start_u16 + new_inserted_u16
                } else {
                    edit_start_u16
                };
                let new_len = new_end - new_start;
                if new_len > 0 {
                    updated.push(EntityInText::with_data(ty, new_start, new_len, ent.data()));
                }
            }
        }

        // Apply active typing formats to newly inserted characters.
        if new_inserted_u16 > 0 && !self.active_formats.is_empty() {
            for ty in self.active_formats.entity_types() {
                if ty == EntityType::Code {
                    // Drop conflicting inline styles on the inserted slice.
                    remove_entities_in_range(
                        &mut updated,
                        edit_start_u16,
                        edit_start_u16 + new_inserted_u16,
                        is_inline_conflict_with_code,
                    );
                }
                updated.push(EntityInText::new(ty, edit_start_u16, new_inserted_u16));
            }
        }

        self.entities = updated;
        self.normalize_entities();
    }

    /// Applies or toggles a [`FormatAction`] over `char_range` (`None` uses [`Self::selection_chars`]).
    ///
    /// Returns `true` if entities or active typing flags changed.
    pub fn toggle_format(
        &mut self,
        action: FormatAction,
        char_range: Option<(usize, usize)>,
    ) -> bool {
        let (c_start, c_end) = char_range.unwrap_or_else(|| self.normalized_selection());
        let (c_start, c_end) = (c_start.min(c_end), c_start.max(c_end));

        if c_start == c_end {
            if action == FormatAction::CreateLink {
                return false;
            }
            self.active_formats.toggle(action);
            return true;
        }

        let (u16_start, u16_end) = char_range_to_utf16(&self.text, c_start, c_end);
        if u16_end <= u16_start {
            return false;
        }

        match action {
            FormatAction::ClearFormatting => {
                remove_entities_in_range(&mut self.entities, u16_start, u16_end, |_| true);
                self.normalize_entities();
                true
            }
            FormatAction::CreateLink => false,
            _ => {
                let target_type = match action {
                    FormatAction::Bold => EntityType::Bold,
                    FormatAction::Italic => EntityType::Italic,
                    FormatAction::Underline => EntityType::Underline,
                    FormatAction::StrikeOut => EntityType::StrikeOut,
                    FormatAction::Spoiler => EntityType::Spoiler,
                    FormatAction::Blockquote => EntityType::Blockquote,
                    FormatAction::Monospace => {
                        let selected = slice_chars(&self.text, c_start, c_end);
                        if selected.contains('\n') {
                            EntityType::Pre
                        } else {
                            EntityType::Code
                        }
                    }
                    FormatAction::CreateLink | FormatAction::ClearFormatting => return false,
                };

                let is_mono = matches!(target_type, EntityType::Code | EntityType::Pre);
                let covered = if is_mono {
                    is_range_covered(&self.entities, u16_start, u16_end, |t| {
                        matches!(t, EntityType::Code | EntityType::Pre)
                    })
                } else {
                    is_range_covered(&self.entities, u16_start, u16_end, |t| t == target_type)
                };

                if covered {
                    if is_mono {
                        remove_entities_in_range(&mut self.entities, u16_start, u16_end, |t| {
                            matches!(t, EntityType::Code | EntityType::Pre)
                        });
                    } else {
                        remove_entities_in_range(&mut self.entities, u16_start, u16_end, |t| {
                            t == target_type
                        });
                    }
                } else {
                    if is_mono {
                        // Separate tag rule (`IsSeparateTag` in tdesktop): monospace strips inline tags.
                        remove_entities_in_range(&mut self.entities, u16_start, u16_end, |t| {
                            is_inline_conflict_with_code(t)
                                || matches!(t, EntityType::Code | EntityType::Pre)
                        });
                    } else if is_inline_conflict_with_code(target_type) {
                        // Applying bold/italic/etc. removes conflicting Code/Pre in the selection.
                        remove_entities_in_range(&mut self.entities, u16_start, u16_end, |t| {
                            matches!(t, EntityType::Code | EntityType::Pre)
                        });
                    }
                    self.entities.push(EntityInText::new(
                        target_type,
                        u16_start,
                        u16_end - u16_start,
                    ));
                }

                self.normalize_entities();
                true
            }
        }
    }

    /// Creates or edits a [`EntityType::CustomUrl`] link over `char_range` (replacing text if changed
    /// or inserting `link_text` at caret if collapsed).
    pub fn set_link(
        &mut self,
        char_range: Option<(usize, usize)>,
        link_text: &str,
        raw_url: &str,
    ) -> bool {
        let normalized_url = normalize_link_url(raw_url);
        let text_to_insert = link_text.trim();
        if normalized_url.is_empty() || text_to_insert.is_empty() {
            return false;
        }
        let (c_start, c_end) = char_range.unwrap_or_else(|| self.normalized_selection());
        let (c_start, c_end) = (c_start.min(c_end), c_start.max(c_end));

        let (start_u16, inserted_u16, end_char) =
            self.replace_char_range_preserving_outer(c_start, c_end, text_to_insert);

        remove_entities_in_range(
            &mut self.entities,
            start_u16,
            start_u16 + inserted_u16,
            |t| matches!(t, EntityType::CustomUrl | EntityType::Url),
        );
        self.entities.push(EntityInText::with_data(
            EntityType::CustomUrl,
            start_u16,
            inserted_u16,
            normalized_url,
        ));
        self.normalize_entities();
        self.cursor_char = end_char;
        self.selection_chars = (end_char, end_char);
        self.pending_cursor_char = Some(end_char);
        true
    }

    /// Returns the existing custom link URL covering `char_range` (if any), for pre-filling `Ctrl+K`.
    #[must_use]
    pub fn existing_link_for_range(&self, char_range: (usize, usize)) -> Option<String> {
        let (u16_start, u16_end) = char_range_to_utf16(&self.text, char_range.0, char_range.1);
        for ent in &self.entities {
            if ent.entity_type() == EntityType::CustomUrl {
                let s = ent.offset();
                let e = s + ent.length();
                if u16_start == u16_end {
                    if s <= u16_start && u16_start <= e {
                        return Some(ent.data().to_owned());
                    }
                } else if s < u16_end && e > u16_start {
                    return Some(ent.data().to_owned());
                }
            }
        }
        None
    }

    /// Pastes a [`TextWithEntities`] payload at `replace_char_range` (or current selection),
    /// preserving all pasted entities and shifting surrounding entities cleanly.
    pub fn paste_rich(
        &mut self,
        pasted: &TextWithEntities,
        replace_char_range: Option<(usize, usize)>,
    ) {
        let (c_start, c_end) = replace_char_range.unwrap_or_else(|| self.normalized_selection());
        let (c_start, c_end) = (c_start.min(c_end), c_start.max(c_end));

        let (insert_u16, pasted_u16_len, new_cursor_char) =
            self.replace_char_range_preserving_outer(c_start, c_end, &pasted.text);

        let pasted_total = pasted.utf16_len();
        for ent in &pasted.entities {
            if !ent.valid_for_text(pasted_total) {
                continue;
            }
            let off = insert_u16 + ent.offset();
            let len = ent.length().min(pasted_u16_len - ent.offset());
            if len > 0 {
                self.entities.push(EntityInText::with_data(
                    ent.entity_type(),
                    off,
                    len,
                    ent.data(),
                ));
            }
        }
        self.normalize_entities();
        self.cursor_char = new_cursor_char;
        self.selection_chars = (new_cursor_char, new_cursor_char);
        self.pending_cursor_char = Some(new_cursor_char);
    }

    /// Inserts a Telegram custom emoji (`EntityType::CustomEmoji`) at `replace_char_range`
    /// (or current selection), storing `document_id` in entity data.
    pub fn insert_custom_emoji(
        &mut self,
        fallback_emoji: &str,
        document_id: u64,
        replace_char_range: Option<(usize, usize)>,
    ) {
        let glyph = if fallback_emoji.is_empty() {
            "⭐"
        } else {
            fallback_emoji
        };
        let (c_start, c_end) = replace_char_range.unwrap_or_else(|| self.normalized_selection());
        let (c_start, c_end) = (c_start.min(c_end), c_start.max(c_end));

        let (insert_u16, glyph_u16_len, new_cursor_char) =
            self.replace_char_range_preserving_outer(c_start, c_end, glyph);

        self.entities.push(EntityInText::with_data(
            EntityType::CustomEmoji,
            insert_u16,
            glyph_u16_len,
            document_id.to_string(),
        ));
        self.custom_emoji_labels
            .insert(document_id, glyph.to_owned());
        self.normalize_entities();
        self.cursor_char = new_cursor_char;
        self.selection_chars = (new_cursor_char, new_cursor_char);
        self.pending_cursor_char = Some(new_cursor_char);
    }

    /// Inserts a user mention creating an [`EntityType::MentionName`] entity (replacing
    /// `replace_char_range`, e.g. the active `@query` token, and appending a trailing space).
    ///
    /// Matches `FieldAutocomplete` + `PrepareMentionTag` in `chat_helpers/message_field.cpp`.
    pub fn insert_mention_name(
        &mut self,
        display_name: &str,
        user_id: u64,
        access_hash: u64,
        self_id: u64,
        replace_char_range: (usize, usize),
    ) {
        let clean_name = display_name.trim();
        if clean_name.is_empty() {
            return;
        }
        let with_space = format!("{clean_name} ");
        let (c_start, c_end) = (
            replace_char_range.0.min(replace_char_range.1),
            replace_char_range.0.max(replace_char_range.1),
        );
        let (insert_u16, _total_u16, new_cursor_char) =
            self.replace_char_range_preserving_outer(c_start, c_end, &with_space);

        let name_u16_len = utf16_len(clean_name) as i32;
        let data = mention_name_data_from_fields(MentionNameFields {
            user_id,
            access_hash,
            self_id,
        });
        remove_entities_in_range(
            &mut self.entities,
            insert_u16,
            insert_u16 + name_u16_len,
            |t| matches!(t, EntityType::MentionName | EntityType::Mention),
        );
        self.entities.push(EntityInText::with_data(
            EntityType::MentionName,
            insert_u16,
            name_u16_len,
            data,
        ));
        self.normalize_entities();
        self.cursor_char = new_cursor_char;
        self.selection_chars = (new_cursor_char, new_cursor_char);
        self.pending_cursor_char = Some(new_cursor_char);
    }

    /// Converts the live compose buffer into final sendable [`TextWithEntities`]:
    /// 1. Preserves all live-edited entities (`Bold`, `Italic`, `Underline`, `StrikeOut`,
    ///    `Code`, `Pre`, `Spoiler`, `Blockquote`, `CustomUrl`, `CustomEmoji`, `MentionName`).
    /// 2. Parses any raw Telegram markdown typed directly in plain text (`**bold**`, `__italic__`,
    ///    `` `code` ``, `~~strike~~`, `||spoiler||`, `[text](url)`) via `tg_text::parse_markdown`
    ///    while remapping existing live entity offsets cleanly when markdown delimiters are stripped.
    /// 3. Runs `tg_text::parse_entities` to auto-detect links, `@mentions`, `#hashtags`, and
    ///    `/bot_commands`.
    /// 4. Trims leading/trailing whitespace and adjusts entity offsets accordingly.
    #[must_use]
    pub fn to_send_text_with_entities(&self) -> TextWithEntities {
        // Trim leading and trailing whitespace while rebasing existing live entities.
        let trimmed_start_bytes = self.text.len() - self.text.trim_start().len();
        let trimmed_end_bytes = self.text.trim_end().len();
        if trimmed_end_bytes <= trimmed_start_bytes {
            return TextWithEntities::new();
        }
        let trimmed_str = &self.text[trimmed_start_bytes..trimmed_end_bytes];
        let trim_start_u16 = utf16_len(&self.text[..trimmed_start_bytes]) as i32;
        let trimmed_u16_len = utf16_len(trimmed_str) as i32;

        let mut base_entities = Vec::new();
        for ent in &self.entities {
            let s = (ent.offset() - trim_start_u16).max(0);
            let e = (ent.offset() + ent.length() - trim_start_u16).min(trimmed_u16_len);
            if e > s {
                base_entities.push(EntityInText::with_data(
                    ent.entity_type(),
                    s,
                    e - s,
                    ent.data(),
                ));
            }
        }

        // Parse inline markdown on `trimmed_str` via `tg-text`.
        let md_parsed = parse_markdown(trimmed_str);
        let final_text = md_parsed.text.clone();
        let mut final_entities = md_parsed.entities;

        if final_text == trimmed_str {
            // No markdown markers were stripped: live entities keep their exact UTF-16 offsets!
            final_entities.extend(base_entities);
        } else {
            // Markdown markers were stripped: map each live entity offset through the stripped diff.
            let mapping = build_subsequence_u16_map(trimmed_str, &final_text);
            for ent in base_entities {
                let s = (ent.offset() as usize).min(mapping.len().saturating_sub(1));
                let e =
                    ((ent.offset() + ent.length()) as usize).min(mapping.len().saturating_sub(1));
                let new_s = mapping[s];
                let new_e = mapping[e];
                if new_e > new_s {
                    final_entities.push(EntityInText::with_data(
                        ent.entity_type(),
                        new_s as i32,
                        (new_e - new_s) as i32,
                        ent.data(),
                    ));
                }
            }
        }

        // Auto-detect URLs, @mentions, #hashtags, /bot_commands via `tg-text`.
        let auto = parse_entities(
            &final_text,
            TextParseFlags::LINKS
                | TextParseFlags::MENTIONS
                | TextParseFlags::HASHTAGS
                | TextParseFlags::BOT_COMMANDS,
        );
        for ae in auto.entities {
            // Do not overwrite an existing CustomUrl, Code/Pre, CustomEmoji, or MentionName.
            let ae_s = ae.offset();
            let ae_e = ae_s + ae.length();
            let conflicts = final_entities.iter().any(|existing| {
                let ex_s = existing.offset();
                let ex_e = ex_s + existing.length();
                ex_s < ae_e
                    && ex_e > ae_s
                    && matches!(
                        existing.entity_type(),
                        EntityType::CustomUrl
                            | EntityType::Code
                            | EntityType::Pre
                            | EntityType::CustomEmoji
                            | EntityType::MentionName
                    )
            });
            if !conflicts {
                final_entities.push(ae);
            }
        }

        let mut result = TextWithEntities {
            text: final_text,
            entities: final_entities,
        };
        result.sort_entities();
        result.entities.dedup();
        result
    }

    fn replace_char_range_preserving_outer(
        &mut self,
        c_start: usize,
        c_end: usize,
        replacement: &str,
    ) -> (i32, i32, usize) {
        let byte_start = char_to_byte_offset(&self.text, c_start);
        let byte_end = char_to_byte_offset(&self.text, c_end);
        let start_u16 = utf16_len(&self.text[..byte_start]) as i32;
        let removed_u16 = utf16_len(&self.text[byte_start..byte_end]) as i32;
        let old_end_u16 = start_u16 + removed_u16;
        let inserted_u16 = utf16_len(replacement) as i32;
        let delta = inserted_u16 - removed_u16;

        let mut new_text =
            String::with_capacity(self.text.len() - (byte_end - byte_start) + replacement.len());
        new_text.push_str(&self.text[..byte_start]);
        new_text.push_str(replacement);
        new_text.push_str(&self.text[byte_end..]);
        self.text = new_text;

        // Keep entities outside the replaced range intact (splitting or shifting as needed).
        let mut updated = Vec::with_capacity(self.entities.len());
        for ent in &self.entities {
            let s = ent.offset();
            let e = s + ent.length();
            if e <= start_u16 {
                updated.push(ent.clone());
            } else if s >= old_end_u16 {
                updated.push(EntityInText::with_data(
                    ent.entity_type(),
                    (s + delta).max(0),
                    ent.length(),
                    ent.data(),
                ));
            } else {
                // Split around the replaced span (outer segments survive).
                if s < start_u16 {
                    updated.push(EntityInText::with_data(
                        ent.entity_type(),
                        s,
                        start_u16 - s,
                        ent.data(),
                    ));
                }
                if e > old_end_u16 {
                    let right_start = start_u16 + inserted_u16;
                    let right_len = e - old_end_u16;
                    updated.push(EntityInText::with_data(
                        ent.entity_type(),
                        right_start,
                        right_len,
                        ent.data(),
                    ));
                }
            }
        }
        self.entities = updated;
        let new_cursor_char = c_start + replacement.chars().count();
        (start_u16, inserted_u16, new_cursor_char)
    }

    /// Clamps, sorts, and coalesces adjacent/overlapping identical entities.
    pub fn normalize_entities(&mut self) {
        let total_u16 = utf16_len(&self.text) as i32;
        self.entities.retain_mut(|e| {
            let s = e.offset().clamp(0, total_u16);
            let end = (e.offset() + e.length()).clamp(0, total_u16);
            if end <= s {
                return false;
            }
            *e = EntityInText::with_data(e.entity_type(), s, end - s, e.data());
            true
        });

        self.entities.sort_by(|a, b| {
            (a.entity_type() as u8)
                .cmp(&(b.entity_type() as u8))
                .then_with(|| a.data().cmp(b.data()))
                .then_with(|| a.offset().cmp(&b.offset()))
                .then_with(|| a.length().cmp(&b.length()))
        });

        let mut merged: Vec<EntityInText> = Vec::with_capacity(self.entities.len());
        for ent in self.entities.drain(..) {
            let can_merge = !matches!(
                ent.entity_type(),
                EntityType::CustomEmoji | EntityType::MentionName
            );
            if can_merge
                && let Some(prev) = merged.last_mut()
                && prev.entity_type() == ent.entity_type()
                && prev.data() == ent.data()
                && ent.offset() <= prev.offset() + prev.length()
            {
                let new_end = (prev.offset() + prev.length()).max(ent.offset() + ent.length());
                *prev = EntityInText::with_data(
                    prev.entity_type(),
                    prev.offset(),
                    new_end - prev.offset(),
                    prev.data(),
                );
                continue;
            }
            merged.push(ent);
        }

        self.entities = merged;
        tg_text::sort_entities(&mut self.entities);
    }
}

/// Detects Telegram Desktop formatting keyboard shortcuts when the compose field is focused.
///
/// Consumes the pressed shortcut key from `ctx` and returns the matching [`FormatAction`].
pub fn detect_format_shortcut(ctx: &egui::Context) -> Option<FormatAction> {
    ctx.input_mut(|i| {
        let ctrl = i.modifiers.ctrl || i.modifiers.command;
        if !ctrl || i.modifiers.alt {
            return None;
        }
        let shift = i.modifiers.shift;
        if !shift && i.consume_key(i.modifiers, Key::B) {
            return Some(FormatAction::Bold);
        }
        if !shift && i.consume_key(i.modifiers, Key::I) {
            return Some(FormatAction::Italic);
        }
        if !shift && i.consume_key(i.modifiers, Key::U) {
            return Some(FormatAction::Underline);
        }
        if !shift && i.consume_key(i.modifiers, Key::K) {
            return Some(FormatAction::CreateLink);
        }
        if shift && i.consume_key(i.modifiers, Key::X) {
            return Some(FormatAction::StrikeOut);
        }
        if shift && i.consume_key(i.modifiers, Key::M) {
            return Some(FormatAction::Monospace);
        }
        if shift && i.consume_key(i.modifiers, Key::P) {
            return Some(FormatAction::Spoiler);
        }
        if shift && i.consume_key(i.modifiers, Key::N) {
            return Some(FormatAction::ClearFormatting);
        }
        if shift && i.consume_key(i.modifiers, Key::Period) {
            return Some(FormatAction::Blockquote);
        }
        None
    })
}

/// Builds a live styled egui [`LayoutJob`] for `text` with `entities` so bold, italic, underline,
/// strikethrough, monospace, spoiler, links, mentions, and custom emoji render styled while typing.
///
/// Guarantees a 1:1 byte/character match with `text` so `egui::TextEdit` caret and selection
/// indices remain exact.
#[must_use]
pub fn build_rich_edit_layout_job(
    text: &str,
    entities: &[EntityInText],
    font_size: f32,
    wrap_width: f32,
    palette: &RichInputPalette,
) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width.max(40.0),
            ..Default::default()
        },
        ..Default::default()
    };

    if text.is_empty() {
        return job;
    }

    let mapper = OffsetMapper::new(text);
    let len = text.len();
    let mut cuts = BTreeSet::new();
    cuts.insert(0usize);
    cuts.insert(len);

    let mut byte_entities: Vec<(usize, usize, EntityType)> = Vec::with_capacity(entities.len());
    for ent in entities {
        if ent.offset() < 0 || ent.length() <= 0 {
            continue;
        }
        let s_u16 = ent.offset() as usize;
        let e_u16 = (ent.offset() + ent.length()) as usize;
        let Some(range) = mapper.utf16_range_to_utf8(s_u16, e_u16.saturating_sub(s_u16)) else {
            continue;
        };
        if range.end > range.start && range.end <= len {
            cuts.insert(range.start);
            cuts.insert(range.end);
            byte_entities.push((range.start, range.end, ent.entity_type()));
        }
    }

    let points: Vec<usize> = cuts.into_iter().collect();
    for win in points.windows(2) {
        let a = win[0];
        let b = win[1];
        if b <= a {
            continue;
        }
        let mut bold = false;
        let mut italic = false;
        let mut underline = false;
        let mut strike = false;
        let mut mono = false;
        let mut spoiler = false;
        let mut link = false;
        let mut quote = false;
        let mut custom_emoji = false;

        for &(s, e, ty) in &byte_entities {
            if s <= a && e >= b {
                match ty {
                    EntityType::Bold | EntityType::Semibold => bold = true,
                    EntityType::Italic => italic = true,
                    EntityType::Underline => underline = true,
                    EntityType::StrikeOut => strike = true,
                    EntityType::Code | EntityType::Pre => mono = true,
                    EntityType::Spoiler => spoiler = true,
                    EntityType::CustomUrl
                    | EntityType::Url
                    | EntityType::Mention
                    | EntityType::MentionName
                    | EntityType::Hashtag
                    | EntityType::BotCommand
                    | EntityType::Email => link = true,
                    EntityType::Blockquote => quote = true,
                    EntityType::CustomEmoji => custom_emoji = true,
                    _ => {}
                }
            }
        }

        let mut flags = FontFlags::from_bits(0);
        if mono {
            flags = flags.union(FontFlags::MONOSPACE);
        } else {
            if bold {
                flags = flags.union(FontFlags::BOLD);
            }
            if italic || quote {
                flags = flags.union(FontFlags::ITALIC);
            }
        }

        let color = if link {
            palette.link_fg
        } else if mono {
            palette.mono_fg
        } else if spoiler {
            palette.spoiler_fg
        } else {
            palette.text_fg
        };

        let background = if custom_emoji {
            palette.custom_emoji_bg
        } else if spoiler {
            palette.spoiler_bg
        } else if mono {
            palette.mono_bg
        } else if quote {
            palette.quote_bg
        } else {
            Color32::TRANSPARENT
        };

        let format = TextFormat {
            font_id: font_id_for_flags(font_size, flags),
            color,
            background,
            underline: if underline || link {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            strikethrough: if strike {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        };

        job.append(&text[a..b], 0.0, format);
    }

    job
}

/// Renders a live entity-styled multi-line `egui::TextEdit` backed by [`RichEditState`].
///
/// Updates [`RichEditState::entities`] via [`RichEditState::apply_text_diff`] whenever the text
/// changes, synchronizes selection/caret indices, and applies any pending programmatic cursor.
#[allow(clippy::too_many_arguments)]
pub fn show_rich_text_edit(
    ui: &mut Ui,
    id: Id,
    state: &mut RichEditState,
    placeholder: &str,
    font_size: f32,
    desired_width: f32,
    palette: &RichInputPalette,
    request_focus: bool,
) -> egui::text_edit::TextEditOutput {
    let prev_text = state.text.clone();
    let entities_snapshot = state.entities.clone();
    let pal = *palette;

    let mut layouter = move |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| -> Arc<Galley> {
        let job = build_rich_edit_layout_job(
            buf.as_str(),
            &entities_snapshot,
            font_size,
            wrap_width,
            &pal,
        );
        ui.painter().layout_job(job)
    };

    let edit = TextEdit::multiline(&mut state.text)
        .id(id)
        .frame(Frame::NONE)
        .desired_width(desired_width)
        .desired_rows(1)
        .hint_text(placeholder)
        .layouter(&mut layouter);

    let mut output = edit.show(ui);

    if request_focus {
        output.response.request_focus();
    }

    if state.text != prev_text {
        state.apply_text_diff(&prev_text);
    }

    if let Some(target_char) = state.pending_cursor_char.take() {
        let clamped = target_char.min(state.text.chars().count());
        let range = CCursorRange::one(CCursor::new(clamped));
        output.state.cursor.set_char_range(Some(range));
        output.state.clone().store(ui.ctx(), output.response.id);
        state.cursor_char = clamped;
        state.selection_chars = (clamped, clamped);
    } else if let Some(range) = output.cursor_range {
        state.sync_cursor_range(range);
    }

    output
}

/// Modal dialog state for `Ctrl+K` ("Create Link" / "Edit Link", `EditLinkBox` in `message_field.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditLinkDialogState {
    /// Editable visible link text.
    pub text: String,
    /// Editable target URL.
    pub url: String,
    /// Target character selection in the compose field when opened.
    pub target_range: (usize, usize),
    /// True if editing an existing link rather than creating a new one.
    pub editing_existing: bool,
    /// Validation error hint if user clicked Save with empty fields.
    pub error: Option<String>,
}

/// Result emitted by [`EditLinkDialogState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditLinkDialogAction {
    /// User confirmed link creation/update.
    Confirm {
        /// Original character selection to replace.
        range: (usize, usize),
        /// Display text for the link.
        text: String,
        /// Normalized URL.
        url: String,
    },
    /// User cancelled the dialog.
    Cancel,
}

impl EditLinkDialogState {
    /// Opens a new Create/Edit Link dialog initialized from the current selection and optional URL.
    #[must_use]
    pub fn open(
        selected_text: String,
        existing_url: Option<String>,
        target_range: (usize, usize),
    ) -> Self {
        let editing_existing = existing_url.is_some();
        Self {
            text: selected_text,
            url: existing_url.unwrap_or_else(|| "https://".to_owned()),
            target_range,
            editing_existing,
            error: None,
        }
    }

    /// Renders the modal Create/Edit Link box centered in the viewport.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        bg: Color32,
        divider: Color32,
        text_fg: Color32,
        sub_fg: Color32,
        accent: Color32,
    ) -> Option<EditLinkDialogAction> {
        let mut action = None;
        let screen = ui.ctx().content_rect();
        // Translucent backdrop.
        ui.painter().rect_filled(
            screen,
            CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(0, 0, 0, 110),
        );

        let box_w = 340.0_f32.min(screen.width() - 32.0);
        let pos = Pos2::new(
            screen.center().x - box_w * 0.5,
            (screen.center().y - 105.0).max(24.0),
        );

        Area::new(Id::unique("tg_edit_link_dialog"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(bg)
                    .stroke(Stroke::new(1.0, divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(box_w - 36.0);
                        ui.label(
                            egui::RichText::new(if self.editing_existing {
                                "Edit Link"
                            } else {
                                "Create Link"
                            })
                            .size(16.0)
                            .strong()
                            .color(text_fg),
                        );
                        ui.add_space(10.0);

                        ui.label(egui::RichText::new("Text").size(12.0).color(sub_fg));
                        let text_resp = ui.add(
                            TextEdit::singleline(&mut self.text)
                                .desired_width(f32::INFINITY)
                                .hint_text("Link display text"),
                        );
                        ui.add_space(8.0);

                        ui.label(egui::RichText::new("URL").size(12.0).color(sub_fg));
                        let url_resp = ui.add(
                            TextEdit::singleline(&mut self.url)
                                .desired_width(f32::INFINITY)
                                .hint_text("https://example.org"),
                        );

                        if let Some(ref err) = self.error {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0xE0, 0x48, 0x48)),
                            );
                        }

                        let submit_enter = (text_resp.lost_focus() || url_resp.lost_focus())
                            && ui.ctx().input(|i| i.key_pressed(Key::Enter));

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let save_label = if self.editing_existing {
                                        "Save"
                                    } else {
                                        "Create"
                                    };
                                    let save_btn = ui.add(
                                        egui::Button::new(
                                            egui::RichText::new(save_label)
                                                .strong()
                                                .color(Color32::WHITE),
                                        )
                                        .fill(accent),
                                    );
                                    if save_btn.clicked() || submit_enter {
                                        let norm = normalize_link_url(&self.url);
                                        if self.text.trim().is_empty() {
                                            self.error = Some("Enter link text".to_owned());
                                        } else if norm.is_empty() {
                                            self.error = Some("Enter a valid URL".to_owned());
                                        } else {
                                            action = Some(EditLinkDialogAction::Confirm {
                                                range: self.target_range,
                                                text: self.text.trim().to_owned(),
                                                url: norm,
                                            });
                                        }
                                    }
                                    if ui.button("Cancel").clicked() {
                                        action = Some(EditLinkDialogAction::Cancel);
                                    }
                                },
                            );
                        });
                    });
            });

        if action.is_none() && ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            action = Some(EditLinkDialogAction::Cancel);
        }
        action
    }
}

/// Right-click Formatting context menu for rich compose input (`InputField::contextMenuEvent`).
#[derive(Debug, Clone, Default)]
pub struct FormattingContextMenu {
    /// True while menu is open.
    pub open: bool,
    /// Anchor position in screen coordinates.
    pub pos: Pos2,
    /// Captured selection when menu opened (so clicking menu items preserves selection).
    pub captured_selection: (usize, usize),
}

/// Event emitted when the user clicks an item in [`FormattingContextMenu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormattingMenuEvent {
    /// Apply/toggle a formatting action over the captured selection.
    Format(FormatAction),
    /// Paste rich text keeping entities.
    PasteRich,
}

impl FormattingContextMenu {
    /// Opens the formatting menu at `pos` with `selection`.
    pub fn open_at(&mut self, pos: Pos2, selection: (usize, usize)) {
        self.open = true;
        self.pos = pos;
        self.captured_selection = selection;
    }

    /// Closes the menu.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Paints the formatting context menu above the compose bar if open.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        has_rich_clipboard: bool,
        bg: Color32,
        divider: Color32,
        text_fg: Color32,
        sub_fg: Color32,
    ) -> Option<FormattingMenuEvent> {
        if !self.open {
            return None;
        }
        let mut emitted = None;
        let mut close = false;
        let screen = ui.ctx().content_rect();
        let menu_w = 228.0_f32;
        let menu_h = 286.0_f32;
        let clamped_pos = Pos2::new(
            self.pos.x.min((screen.right() - menu_w - 8.0).max(8.0)),
            (self.pos.y - menu_h).max(8.0),
        );

        let opaque_bg = Color32::from_rgb(bg.r(), bg.g(), bg.b());
        let area_resp = Area::new(Id::unique("tg_compose_format_ctx_menu"))
            .order(Order::Foreground)
            .fixed_pos(clamped_pos)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(opaque_bg)
                    .stroke(Stroke::new(1.0, divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(6))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_rgba_unmultiplied(0, 0, 0, 36),
                    })
                    .show(ui, |ui| {
                        let inner_w = menu_w - 12.0;
                        ui.set_min_width(inner_w);
                        ui.set_max_width(inner_w);
                        ui.label(
                            egui::RichText::new("Transformations / Formatting")
                                .size(11.0)
                                .strong()
                                .color(sub_fg),
                        );
                        ui.separator();
                        for act in FormatAction::MENU_ITEMS {
                            if act == FormatAction::ClearFormatting {
                                ui.separator();
                            }
                            let row = ui.horizontal(|ui| {
                                let clicked = ui
                                    .selectable_label(
                                        false,
                                        egui::RichText::new(act.label()).color(text_fg),
                                    )
                                    .clicked();
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(act.shortcut_hint())
                                                .size(10.5)
                                                .color(sub_fg),
                                        );
                                    },
                                );
                                clicked
                            });
                            if row.inner {
                                emitted = Some(FormattingMenuEvent::Format(act));
                                close = true;
                            }
                        }
                        if has_rich_clipboard {
                            ui.separator();
                            if ui
                                .selectable_label(
                                    false,
                                    egui::RichText::new("Paste with Formatting").color(text_fg),
                                )
                                .clicked()
                            {
                                emitted = Some(FormattingMenuEvent::PasteRich);
                                close = true;
                            }
                        }
                    });
            });

        if close || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            self.open = false;
        } else if ui.ctx().input(|i| i.pointer.primary_clicked())
            && let Some(ptr) = ui.ctx().pointer_interact_pos()
            && !area_resp.response.rect.contains(ptr)
        {
            self.open = false;
        }

        emitted
    }
}

/// Validates and normalizes a user-entered URL (`ValidateUrl` in `message_field.cpp`).
#[must_use]
pub fn normalize_link_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "https://" || trimmed == "http://" {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("tg://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tonsite://")
    {
        trimmed.to_owned()
    } else if trimmed.contains('.') {
        format!("https://{trimmed}")
    } else {
        String::new()
    }
}

/// Returns `true` for entity types that conflict with `Code` / `Pre` (`IsSeparateTag`).
#[inline]
const fn is_inline_conflict_with_code(ty: EntityType) -> bool {
    matches!(
        ty,
        EntityType::Bold
            | EntityType::Semibold
            | EntityType::Italic
            | EntityType::Underline
            | EntityType::StrikeOut
            | EntityType::Spoiler
    )
}

#[inline]
const fn is_inheritable_inline_entity(ty: EntityType) -> bool {
    matches!(
        ty,
        EntityType::Bold
            | EntityType::Semibold
            | EntityType::Italic
            | EntityType::Underline
            | EntityType::StrikeOut
            | EntityType::Code
            | EntityType::Spoiler
            | EntityType::Blockquote
    )
}

fn char_to_byte_offset(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

fn char_range_to_utf16(text: &str, c_start: usize, c_end: usize) -> (i32, i32) {
    let bs = char_to_byte_offset(text, c_start);
    let be = char_to_byte_offset(text, c_end);
    let u16_s = utf8_to_utf16_offset(text, bs).unwrap_or(0) as i32;
    let u16_e = utf8_to_utf16_offset(text, be).unwrap_or(u16_s as usize) as i32;
    (u16_s, u16_e)
}

fn slice_chars(text: &str, c_start: usize, c_end: usize) -> &str {
    let bs = char_to_byte_offset(text, c_start);
    let be = char_to_byte_offset(text, c_end);
    &text[bs..be]
}

fn is_range_covered<F>(entities: &[EntityInText], start: i32, end: i32, predicate: F) -> bool
where
    F: Fn(EntityType) -> bool,
{
    let mut spans: Vec<(i32, i32)> = entities
        .iter()
        .filter(|e| predicate(e.entity_type()))
        .filter_map(|e| {
            let s = e.offset().max(start);
            let en = (e.offset() + e.length()).min(end);
            (en > s).then_some((s, en))
        })
        .collect();
    if spans.is_empty() {
        return false;
    }
    spans.sort_unstable();
    let mut covered = start;
    for (s, e) in spans {
        if s > covered {
            return false;
        }
        covered = covered.max(e);
    }
    covered >= end
}

fn remove_entities_in_range<F>(entities: &mut Vec<EntityInText>, start: i32, end: i32, predicate: F)
where
    F: Fn(EntityType) -> bool,
{
    let mut out = Vec::with_capacity(entities.len());
    for ent in entities.drain(..) {
        if !predicate(ent.entity_type()) {
            out.push(ent);
            continue;
        }
        let s = ent.offset();
        let e = s + ent.length();
        if e <= start || s >= end {
            out.push(ent);
            continue;
        }
        if s < start {
            out.push(EntityInText::with_data(
                ent.entity_type(),
                s,
                start - s,
                ent.data(),
            ));
        }
        if e > end {
            out.push(EntityInText::with_data(
                ent.entity_type(),
                end,
                e - end,
                ent.data(),
            ));
        }
    }
    *entities = out;
}

/// Builds a monotonic UTF-16 offset map from `original` to `stripped` when `stripped` is formed
/// by removing markdown delimiters from `original`.
fn build_subsequence_u16_map(original: &str, stripped: &str) -> Vec<usize> {
    let orig_u16: Vec<u16> = original.encode_utf16().collect();
    let strip_u16: Vec<u16> = stripped.encode_utf16().collect();
    let mut map = vec![0usize; orig_u16.len() + 1];
    let mut j = 0usize;
    for i in 0..orig_u16.len() {
        map[i] = j;
        if j < strip_u16.len() && orig_u16[i] == strip_u16[j] {
            j += 1;
        }
    }
    map[orig_u16.len()] = j;
    // Also ensure byte boundary sanity if utf16_to_utf8_offset is checked later.
    let _ = utf16_to_utf8_offset(stripped, j);
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_formatting_splits_merges_and_clears() {
        let mut st =
            RichEditState::from_text_with_entities(TextWithEntities::simple("Hello bold world"));
        // Bold "bold" (chars 6..10)
        assert!(st.toggle_format(FormatAction::Bold, Some((6, 10))));
        assert_eq!(st.entities.len(), 1);
        assert_eq!(st.entities[0], EntityInText::new(EntityType::Bold, 6, 4));

        // Expand bold to cover "Hello bold" (0..10) -> coalesces into single [0..10)
        assert!(st.toggle_format(FormatAction::Bold, Some((0, 10))));
        assert_eq!(st.entities.len(), 1);
        assert_eq!(st.entities[0], EntityInText::new(EntityType::Bold, 0, 10));

        // Toggle off bold on the middle "lo " (3..6) -> splits into [0..3) and [6..10)
        assert!(st.toggle_format(FormatAction::Bold, Some((3, 6))));
        assert_eq!(st.entities.len(), 2);
        assert_eq!(st.entities[0], EntityInText::new(EntityType::Bold, 0, 3));
        assert_eq!(st.entities[1], EntityInText::new(EntityType::Bold, 6, 4));

        // Clear all formatting over entire string
        assert!(st.toggle_format(FormatAction::ClearFormatting, Some((0, 16))));
        assert!(st.entities.is_empty());
    }

    #[test]
    fn monospace_drops_conflicting_inline_styles_and_multiline_uses_pre() {
        let mut st =
            RichEditState::from_text_with_entities(TextWithEntities::simple("line1\nline2"));
        st.toggle_format(FormatAction::Bold, Some((0, 11)));
        st.toggle_format(FormatAction::Italic, Some((0, 11)));
        assert_eq!(st.entities.len(), 2);

        // Monospace over multi-line selection becomes Pre and clears Bold/Italic.
        st.toggle_format(FormatAction::Monospace, Some((0, 11)));
        assert_eq!(st.entities.len(), 1);
        assert_eq!(st.entities[0].entity_type(), EntityType::Pre);

        // Single-line monospace becomes Code.
        let mut single =
            RichEditState::from_text_with_entities(TextWithEntities::simple("fn main()"));
        single.toggle_format(FormatAction::Monospace, Some((0, 9)));
        assert_eq!(single.entities[0].entity_type(), EntityType::Code);
    }

    #[test]
    fn text_diff_shifts_entities_across_utf16_emoji_surrogates() {
        // "😀" is 2 UTF-16 code units!
        let mut st = RichEditState::from_text_with_entities(TextWithEntities::simple("Hi world"));
        st.toggle_format(FormatAction::Bold, Some((3, 8))); // "world" at UTF-16 3..8
        let prev = st.text.clone();
        st.text = "Hi 😀 world".to_owned();
        st.apply_text_diff(&prev);
        // Inserted "😀 " = 3 UTF-16 code units -> "world" shifted to 6..11
        assert_eq!(st.entities.len(), 1);
        assert_eq!(st.entities[0], EntityInText::new(EntityType::Bold, 6, 5));
    }

    #[test]
    fn active_typing_format_applies_to_newly_typed_chars() {
        let mut st = RichEditState::new();
        st.toggle_format(FormatAction::Italic, Some((0, 0)));
        assert!(st.active_formats.italic);
        let prev = st.text.clone();
        st.text.push_str("abc");
        st.apply_text_diff(&prev);
        assert_eq!(
            st.entities,
            vec![EntityInText::new(EntityType::Italic, 0, 3)]
        );
    }

    #[test]
    fn paste_rich_custom_emoji_mention_name_and_links() {
        let mut st = RichEditState::from_text_with_entities(TextWithEntities::simple("Hey @sam"));
        // Replace "@sam" (chars 4..8) with MentionName "Sam Chen"
        st.insert_mention_name("Sam Chen", 1001, 42, 1000, (4, 8));
        assert_eq!(st.text, "Hey Sam Chen ");
        assert_eq!(st.entities.len(), 1);
        assert_eq!(st.entities[0].entity_type(), EntityType::MentionName);
        assert_eq!(st.entities[0].offset(), 4);
        assert_eq!(st.entities[0].length(), 8);
        assert_eq!(st.entities[0].data(), "1001.42:1000");

        // Insert custom emoji at end
        let end = st.text.chars().count();
        st.insert_custom_emoji("✨", 5368324170671202286, Some((end, end)));
        assert!(st.entities.iter().any(|e| {
            e.entity_type() == EntityType::CustomEmoji && e.data() == "5368324170671202286"
        }));

        // Paste rich bold+link snippet at end
        let pasted = TextWithEntities {
            text: " docs".to_owned(),
            entities: vec![
                EntityInText::new(EntityType::Bold, 1, 4),
                EntityInText::with_data(EntityType::CustomUrl, 1, 4, "https://rustgram.example"),
            ],
        };
        let end2 = st.text.chars().count();
        st.paste_rich(&pasted, Some((end2, end2)));
        assert!(
            st.entities
                .iter()
                .any(|e| e.entity_type() == EntityType::Bold)
        );
        assert!(st.entities.iter().any(|e| {
            e.entity_type() == EntityType::CustomUrl && e.data() == "https://rustgram.example"
        }));
    }

    #[test]
    fn send_conversion_merges_live_entities_and_typed_markdown() {
        let mut st = RichEditState::from_text_with_entities(TextWithEntities::simple(
            "Live and **md** https://core.telegram.org",
        ));
        st.toggle_format(FormatAction::Underline, Some((0, 4))); // "Live"
        let sent = st.to_send_text_with_entities();
        assert_eq!(sent.text, "Live and md https://core.telegram.org");
        assert!(sent.entities.iter().any(|e| {
            e.entity_type() == EntityType::Underline && e.offset() == 0 && e.length() == 4
        }));
        assert!(sent.entities.iter().any(|e| {
            e.entity_type() == EntityType::Bold && e.offset() == 9 && e.length() == 2
        }));
        assert!(
            sent.entities
                .iter()
                .any(|e| e.entity_type() == EntityType::Url)
        );
    }

    #[test]
    fn live_layout_job_matches_text_byte_length() {
        let mut st = RichEditState::from_text_with_entities(TextWithEntities::simple(
            "Bold italic code spoiler link",
        ));
        st.toggle_format(FormatAction::Bold, Some((0, 4)));
        st.toggle_format(FormatAction::Italic, Some((5, 11)));
        st.toggle_format(FormatAction::Monospace, Some((12, 16)));
        st.toggle_format(FormatAction::Spoiler, Some((17, 24)));
        st.set_link(Some((25, 29)), "link", "rustgram.example");
        let pal = RichInputPalette::from_colors(Color32::BLACK, Color32::BLUE, false);
        let job = build_rich_edit_layout_job(&st.text, &st.entities, 13.5, 400.0, &pal);
        assert_eq!(job.text, st.text);
        assert!(job.sections.len() >= 5);
    }
}
