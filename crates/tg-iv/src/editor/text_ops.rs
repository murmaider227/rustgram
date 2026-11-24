//! Port of `iv/editor/iv_editor_text_entities.h` and `iv/editor/iv_editor_text_entities.cpp`.
//!
//! Provides UTF-16-safe text/entity splicing, span splitting, inline formatting toggling
//! (bold, italic, underline, strike, code, spoiler, subscript, superscript, marked, link,
//! plain text), and inline math formula insertion.

use serde::{Deserialize, Serialize};

use crate::model::trim_math_source;
use crate::rich_page::{
    EntityInText, EntityType, InlineTextObjectEntity, InlineTextObjectFormulaData,
    OBJECT_REPLACEMENT_CHAR, TextWithEntities, inline_formula_copy_source,
    serialize_inline_text_object_entity, slice_text_with_entities, utf16_to_byte_offset,
};

/// Maximum single rich-text leaf length (`kMaxRichTextNodeLength` in `iv_editor_text_entities.h`).
pub const MAX_RICH_TEXT_NODE_LENGTH: usize = 16_000;

/// Maximum committed field length (`kMaxCommittedFieldLength` in `iv_editor_text_entities.h`).
pub const MAX_COMMITTED_FIELD_LENGTH: usize = 256 * 1024;

/// Inline formatting command (`Iv::Editor::State::TextFormattingAction`, extended with `Code` & `Link`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextFormattingAction {
    /// Toggle or set bold (`**...**`).
    Bold,
    /// Toggle or set italic (`*...*`).
    Italic,
    /// Toggle or set underline (`<u>...</u>`).
    Underline,
    /// Toggle or set strikethrough (`~~...~~`).
    StrikeOut,
    /// Toggle or set inline code (`` `...` ``).
    Code,
    /// Toggle or set spoiler (`||...||`).
    Spoiler,
    /// Toggle or set subscript (`<sub>...</sub>`).
    Subscript,
    /// Toggle or set superscript (`<sup>...</sup>`).
    Superscript,
    /// Toggle or set highlight (`<mark>...</mark>`).
    Marked,
    /// Set a custom link URL (empty URL removes existing links in range).
    Link(String),
    /// Strip all inline formatting entities from the selection.
    PlainText,
}

impl TextFormattingAction {
    /// Returns the corresponding [`EntityType`], if this is a single-entity formatting toggle.
    #[must_use]
    pub const fn entity_type(&self) -> Option<EntityType> {
        match self {
            Self::Bold => Some(EntityType::Bold),
            Self::Italic => Some(EntityType::Italic),
            Self::Underline => Some(EntityType::Underline),
            Self::StrikeOut => Some(EntityType::StrikeOut),
            Self::Code => Some(EntityType::Code),
            Self::Spoiler => Some(EntityType::Spoiler),
            Self::Subscript => Some(EntityType::Subscript),
            Self::Superscript => Some(EntityType::Superscript),
            Self::Marked => Some(EntityType::Marked),
            Self::Link(_) => Some(EntityType::CustomUrl),
            Self::PlainText => None,
        }
    }
}

/// Splits `text` into `(before, selected, after)` at UTF-16 range `[from, till)` (`SplitTextSpan`).
#[must_use]
pub fn split_text_span(
    text: &TextWithEntities,
    from: usize,
    till: usize,
) -> Option<(TextWithEntities, TextWithEntities, TextWithEntities)> {
    let total = text.utf16_len();
    if from > till || till > total {
        return None;
    }
    Some((
        slice_text_with_entities(text, 0, from),
        slice_text_with_entities(text, from, till),
        slice_text_with_entities(text, till, total),
    ))
}

/// Adjusts a single entity after replacing `[from, from + old_len)` with `new_len` UTF-16 code units
/// (`AdjustEntityForReplacement`).
#[must_use]
pub fn adjust_entity_for_replacement(
    entity: &EntityInText,
    from: usize,
    old_len: usize,
    new_len: usize,
) -> Option<EntityInText> {
    let till = from + old_len;
    let delta = new_len as isize - old_len as isize;
    let begin = entity.offset;
    let end = entity.end();

    if end <= from {
        Some(entity.clone())
    } else if begin >= till {
        Some(EntityInText {
            kind: entity.kind,
            offset: (begin as isize + delta).max(0) as usize,
            length: entity.length,
            data: entity.data.clone(),
        })
    } else if begin <= from && end >= till {
        let len = (entity.length as isize + delta).max(0) as usize;
        if len == 0 {
            None
        } else {
            Some(EntityInText {
                kind: entity.kind,
                offset: begin,
                length: len,
                data: entity.data.clone(),
            })
        }
    } else if begin < from && end > from {
        // Overlapping left tail: clip at `from`
        let len = from - begin;
        (len > 0).then(|| EntityInText {
            kind: entity.kind,
            offset: begin,
            length: len,
            data: entity.data.clone(),
        })
    } else if begin < till && end > till {
        // Overlapping right head: keep suffix shifted by `delta`
        let new_begin = (till as isize + delta).max(0) as usize;
        let len = end - till;
        (len > 0).then(|| EntityInText {
            kind: entity.kind,
            offset: new_begin,
            length: len,
            data: entity.data.clone(),
        })
    } else {
        None
    }
}

/// Replaces UTF-16 range `[from, till)` in `target` with plain `replacement`, adjusting entities.
pub fn replace_plain_range(
    target: &mut TextWithEntities,
    from: usize,
    till: usize,
    replacement: &str,
) -> usize {
    let total = target.utf16_len();
    let start = from.min(total);
    let end = till.min(total).max(start);
    let old_len = end - start;
    let new_len = replacement.encode_utf16().count();

    let b0 = utf16_to_byte_offset(&target.text, start);
    let b1 = utf16_to_byte_offset(&target.text, end);
    target.text.replace_range(b0..b1, replacement);

    let mut adjusted = Vec::with_capacity(target.entities.len());
    for e in &target.entities {
        if let Some(a) = adjust_entity_for_replacement(e, start, old_len, new_len) {
            adjusted.push(a);
        }
    }
    target.entities = adjusted;
    target.sort_entities();
    start + new_len
}

/// Replaces UTF-16 range `[from, till)` in `target` with rich `replacement`, preserving entities.
pub fn replace_rich_range(
    target: &mut TextWithEntities,
    from: usize,
    till: usize,
    replacement: &TextWithEntities,
) -> usize {
    let start = from.min(target.utf16_len());
    let new_end = replace_plain_range(target, from, till, &replacement.text);
    for e in &replacement.entities {
        target.entities.push(EntityInText {
            kind: e.kind,
            offset: e.offset + start,
            length: e.length,
            data: e.data.clone(),
        });
    }
    target.sort_entities();
    new_end
}

/// Returns true if the entire UTF-16 range `[from, till)` is covered by `kind`.
#[must_use]
pub fn range_has_entity(
    text: &TextWithEntities,
    from: usize,
    till: usize,
    kind: EntityType,
) -> bool {
    if from >= till {
        return text
            .entities
            .iter()
            .any(|e| e.kind == kind && e.offset <= from && e.end() >= from && e.length > 0);
    }
    let mut intervals: Vec<(usize, usize)> = text
        .entities
        .iter()
        .filter(|e| e.kind == kind && e.offset < till && e.end() > from)
        .map(|e| (e.offset.max(from), e.end().min(till)))
        .collect();
    intervals.sort_unstable();
    let mut covered = from;
    for (s, e) in intervals {
        if s > covered {
            return false;
        }
        covered = covered.max(e);
        if covered >= till {
            return true;
        }
    }
    covered >= till
}

/// Applies or toggles an inline formatting action over UTF-16 range `[from, till)`.
///
/// Returns `true` if `text` was modified.
pub fn apply_formatting_to_range(
    text: &mut TextWithEntities,
    from: usize,
    till: usize,
    action: &TextFormattingAction,
    enabled: Option<bool>,
) -> bool {
    let total = text.utf16_len();
    let start = from.min(total);
    let end = till.min(total).max(start);
    if start >= end {
        return false;
    }

    match action {
        TextFormattingAction::PlainText => {
            let before = text.entities.clone();
            let mut updated = Vec::with_capacity(text.entities.len());
            for e in std::mem::take(&mut text.entities) {
                if e.end() <= start || e.offset >= end {
                    updated.push(e);
                } else {
                    if e.offset < start {
                        updated.push(EntityInText {
                            kind: e.kind,
                            offset: e.offset,
                            length: start - e.offset,
                            data: e.data.clone(),
                        });
                    }
                    if e.end() > end {
                        updated.push(EntityInText {
                            kind: e.kind,
                            offset: end,
                            length: e.end() - end,
                            data: e.data,
                        });
                    }
                }
            }
            text.entities = updated;
            text.sort_entities();
            text.entities != before
        }
        TextFormattingAction::Link(url) => {
            let before = text.entities.clone();
            // Remove existing CustomUrl overlapping [start, end)
            remove_entity_range(&mut text.entities, start, end, EntityType::CustomUrl);
            let should_enable = enabled.unwrap_or(!url.trim().is_empty());
            if should_enable && !url.trim().is_empty() {
                text.entities.push(EntityInText::with_data(
                    EntityType::CustomUrl,
                    start,
                    end - start,
                    url.trim().to_string(),
                ));
            }
            text.sort_entities();
            text.entities != before
        }
        other => {
            let Some(kind) = other.entity_type() else {
                return false;
            };
            let before = text.entities.clone();
            let currently_on = range_has_entity(text, start, end, kind);
            let target_on = enabled.unwrap_or(!currently_on);
            if target_on {
                // Subscript and Superscript are mutually exclusive (matching tdesktop).
                if kind == EntityType::Subscript {
                    remove_entity_range(&mut text.entities, start, end, EntityType::Superscript);
                } else if kind == EntityType::Superscript {
                    remove_entity_range(&mut text.entities, start, end, EntityType::Subscript);
                }
                add_and_coalesce_entity(&mut text.entities, start, end, kind);
            } else {
                remove_entity_range(&mut text.entities, start, end, kind);
            }
            text.sort_entities();
            text.entities != before
        }
    }
}

fn remove_entity_range(
    entities: &mut Vec<EntityInText>,
    start: usize,
    end: usize,
    kind: EntityType,
) {
    let mut updated = Vec::with_capacity(entities.len());
    for e in std::mem::take(entities) {
        if e.kind != kind || e.end() <= start || e.offset >= end {
            updated.push(e);
        } else {
            if e.offset < start {
                updated.push(EntityInText {
                    kind: e.kind,
                    offset: e.offset,
                    length: start - e.offset,
                    data: e.data.clone(),
                });
            }
            if e.end() > end {
                updated.push(EntityInText {
                    kind: e.kind,
                    offset: end,
                    length: e.end() - end,
                    data: e.data,
                });
            }
        }
    }
    *entities = updated;
}

fn add_and_coalesce_entity(
    entities: &mut Vec<EntityInText>,
    start: usize,
    end: usize,
    kind: EntityType,
) {
    let mut merged_start = start;
    let mut merged_end = end;
    let mut kept = Vec::with_capacity(entities.len() + 1);
    for e in std::mem::take(entities) {
        if e.kind == kind && e.data.is_empty() && e.offset <= merged_end && e.end() >= merged_start
        {
            merged_start = merged_start.min(e.offset);
            merged_end = merged_end.max(e.end());
        } else {
            kept.push(e);
        }
    }
    kept.push(EntityInText::new(
        kind,
        merged_start,
        merged_end - merged_start,
    ));
    *entities = kept;
}

/// Converts a TeX formula source into a 1-character `\u{FFFC}` inline formula [`TextWithEntities`]
/// (`FormulaSourceToRichText` in `iv_editor_text_entities.cpp`).
#[must_use]
pub fn formula_source_to_rich_text(source: &str) -> TextWithEntities {
    let trimmed = trim_math_source(source).to_string();
    if trimmed.is_empty() {
        return TextWithEntities::default();
    }
    let payload = serialize_inline_text_object_entity(&InlineTextObjectEntity::Formula(
        InlineTextObjectFormulaData {
            copy_source: inline_formula_copy_source(&trimmed),
            trimmed_tex: trimmed,
        },
    ));
    TextWithEntities {
        text: OBJECT_REPLACEMENT_CHAR.to_string(),
        entities: vec![EntityInText::with_data(
            EntityType::CustomEmoji,
            0,
            1,
            payload,
        )],
    }
}
