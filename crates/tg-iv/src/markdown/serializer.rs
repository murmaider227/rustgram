//! Markdown serializer for [`RichPage`] and [`TextWithEntities`].
//!
//! Produces canonical GFM + Math + Telegram Markdown that round-trips cleanly through
//! [`crate::markdown::parser::markdown_to_rich_page`].

use crate::model::trim_math_source;
use crate::rich_page::{
    EntityInText, EntityType, InlineTextObjectEntity, RichBlock, RichBlockKind, RichListKind,
    RichPage, RichTableAlignment, RichTaskState, TextWithEntities, parse_inline_text_object_entity,
    utf16_to_byte_offset,
};

/// Serializes a [`RichPage`] to canonical Markdown text.
#[must_use]
pub fn rich_page_to_markdown(page: &RichPage) -> String {
    serialize_blocks_to_markdown(&page.blocks)
}

/// Serializes a slice of [`RichBlock`]s into canonical Markdown separated by blank lines.
#[must_use]
pub fn serialize_blocks_to_markdown(blocks: &[RichBlock]) -> String {
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        let rendered = serialize_block(block);
        if !rendered.is_empty() {
            parts.push(rendered);
        }
    }
    parts.join("\n\n")
}

fn serialize_block(block: &RichBlock) -> String {
    match block.kind {
        RichBlockKind::Heading => {
            let level = block.heading_level.clamp(1, 6) as usize;
            let hashes = "#".repeat(level);
            let text = serialize_inline_text(&block.text.text);
            format!("{hashes} {text}")
        }
        RichBlockKind::Paragraph => serialize_inline_text(&block.text.text),
        RichBlockKind::Footer => serialize_inline_text(&block.text.text),
        RichBlockKind::Thinking => {
            let body = serialize_inline_text(&block.text.text);
            prefix_lines(&body, "> ")
        }
        RichBlockKind::AuthorDate => serialize_inline_text(&block.text.text),
        RichBlockKind::Code => {
            let code = &block.text.text.text;
            let fence_len = longest_backtick_run(code).max(2) + 1;
            let fence = "`".repeat(fence_len);
            if block.language.is_empty() {
                format!("{fence}\n{code}\n{fence}")
            } else {
                format!("{fence}{}\n{code}\n{fence}", block.language)
            }
        }
        RichBlockKind::Divider => "---".to_string(),
        RichBlockKind::Anchor => String::new(),
        RichBlockKind::Math => {
            let tex = trim_math_source(&block.formula);
            format!("$$\n{tex}\n$$")
        }
        RichBlockKind::Quote => {
            let mut inner_parts = Vec::new();
            if !block.text.text.is_empty() {
                inner_parts.push(serialize_inline_text(&block.text.text));
            }
            if !block.blocks.is_empty() {
                let nested = serialize_blocks_to_markdown(&block.blocks);
                if !nested.is_empty() {
                    inner_parts.push(nested);
                }
            }
            if !block.caption.text.is_empty() {
                inner_parts.push(format!("— {}", serialize_inline_text(&block.caption.text)));
            }
            prefix_lines(&inner_parts.join("\n\n"), "> ")
        }
        RichBlockKind::List => serialize_list_block(block),
        RichBlockKind::Table => serialize_table_block(block),
        RichBlockKind::Details => {
            let open_attr = if block.open { " open" } else { "" };
            let summary = serialize_inline_text(&block.text.text);
            let body = serialize_blocks_to_markdown(&block.blocks);
            if body.is_empty() {
                format!("<details{open_attr}>\n<summary>{summary}</summary>\n</details>")
            } else {
                format!("<details{open_attr}>\n<summary>{summary}</summary>\n\n{body}\n</details>")
            }
        }
        RichBlockKind::Photo => {
            let cap = serialize_inline_text(&block.caption.text);
            format!("![{cap}](tg://photo?id={})", block.photo_id)
        }
        RichBlockKind::Video => {
            let cap = serialize_inline_text(&block.caption.text);
            format!("[video: {cap}](tg://video?id={})", block.document_id)
        }
        RichBlockKind::Audio => {
            let label = if !block.audio_title.is_empty() {
                block.audio_title.clone()
            } else {
                serialize_inline_text(&block.caption.text)
            };
            format!("[audio: {label}](tg://audio?id={})", block.document_id)
        }
        RichBlockKind::File => {
            let label = if !block.file_name.is_empty() {
                block.file_name.clone()
            } else {
                serialize_inline_text(&block.caption.text)
            };
            format!("[file: {label}](tg://document?id={})", block.document_id)
        }
        RichBlockKind::Map => {
            let cap = serialize_inline_text(&block.caption.text);
            format!(
                "[map: {cap}](geo:{},{};z={})",
                block.latitude, block.longitude, block.zoom
            )
        }
        RichBlockKind::ButtonRow => block
            .buttons
            .iter()
            .map(|b| format!("[{}]", serialize_inline_text(&b.text.text)))
            .collect::<Vec<_>>()
            .join(" "),
        RichBlockKind::GroupedMedia => block
            .media_items
            .iter()
            .map(|m| match m.kind {
                RichBlockKind::Photo => format!("![](tg://photo?id={})", m.photo_id),
                RichBlockKind::Video => format!("[video](tg://video?id={})", m.document_id),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        RichBlockKind::Channel => format!("[@{}](https://t.me/{})", block.username, block.username),
        RichBlockKind::RelatedArticles => block
            .related_articles
            .iter()
            .map(|a| format!("- [{}]({})", a.title, a.url))
            .collect::<Vec<_>>()
            .join("\n"),
        RichBlockKind::Embed | RichBlockKind::EmbedPost => {
            if block.url.is_empty() {
                String::new()
            } else {
                format!("<{}>", block.url)
            }
        }
        RichBlockKind::Unsupported => String::new(),
    }
}

fn serialize_list_block(block: &RichBlock) -> String {
    let mut lines = Vec::with_capacity(block.list_items.len());
    let mut next_num = block
        .ordered_list
        .start
        .unwrap_or(if block.ordered_list.reversed {
            block.list_items.len() as i32
        } else {
            1
        });
    let step = if block.ordered_list.reversed { -1 } else { 1 };

    for item in &block.list_items {
        let marker = if block.list_kind == RichListKind::Ordered {
            let val = item.number.value.unwrap_or(next_num);
            next_num = val + step;
            format!("{val}. ")
        } else {
            "- ".to_string()
        };
        let task_prefix = match item.task_state {
            RichTaskState::None => "",
            RichTaskState::Unchecked => "[ ] ",
            RichTaskState::Checked => "[x] ",
        };
        let indent = " ".repeat(marker.len());
        if item.blocks.is_empty() {
            let body = serialize_inline_text(&item.text.text);
            let mut body_lines = body.lines();
            let first = body_lines.next().unwrap_or("");
            let mut rendered = format!("{marker}{task_prefix}{first}");
            for cont in body_lines {
                rendered.push('\n');
                rendered.push_str(&indent);
                rendered.push_str(cont);
            }
            lines.push(rendered);
        } else {
            let mut block_parts = Vec::new();
            if !item.text.text.is_empty() {
                block_parts.push(serialize_inline_text(&item.text.text));
            }
            let nested = serialize_blocks_to_markdown(&item.blocks);
            if !nested.is_empty() {
                block_parts.push(nested);
            }
            let joined = block_parts.join("\n\n");
            let mut iter = joined.lines();
            let first = iter.next().unwrap_or("");
            let mut rendered = format!("{marker}{task_prefix}{first}");
            for cont in iter {
                rendered.push('\n');
                if !cont.is_empty() {
                    rendered.push_str(&indent);
                    rendered.push_str(cont);
                }
            }
            lines.push(rendered);
        }
    }
    lines.join("\n")
}

fn serialize_table_block(block: &RichBlock) -> String {
    if block.table_rows.is_empty() {
        return String::new();
    }
    let col_count = block
        .table_rows
        .iter()
        .map(|r| r.cells.len())
        .max()
        .unwrap_or(0);
    if col_count == 0 {
        return String::new();
    }
    let mut out_lines = Vec::with_capacity(block.table_rows.len() + 1);
    for (row_idx, row) in block.table_rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(col_count);
        for col in 0..col_count {
            let rendered = row
                .cells
                .get(col)
                .map(|c| escape_table_cell(&serialize_inline_text(&c.text.text)))
                .unwrap_or_default();
            cells.push(rendered);
        }
        out_lines.push(format!("| {} |", cells.join(" | ")));
        if row_idx == 0 {
            let mut seps = Vec::with_capacity(col_count);
            for col in 0..col_count {
                let align = row
                    .cells
                    .get(col)
                    .map(|c| c.alignment)
                    .unwrap_or(RichTableAlignment::Left);
                seps.push(match align {
                    RichTableAlignment::Left => "---",
                    RichTableAlignment::Center => ":---:",
                    RichTableAlignment::Right => "---:",
                });
            }
            out_lines.push(format!("| {} |", seps.join(" | ")));
        }
    }
    out_lines.join("\n")
}

fn escape_table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

fn prefix_lines(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|line| {
            if line.is_empty() {
                prefix.trim_end().to_string()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn longest_backtick_run(s: &str) -> usize {
    let mut max_run = 0usize;
    let mut cur = 0usize;
    for b in s.bytes() {
        if b == b'`' {
            cur += 1;
            max_run = max_run.max(cur);
        } else {
            cur = 0;
        }
    }
    max_run
}

/// Serializes a [`TextWithEntities`] leaf to canonical inline Markdown.
#[must_use]
pub fn serialize_inline_text(text: &TextWithEntities) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut sorted = text.clone();
    sorted.sort_entities();
    let total = sorted.utf16_len();
    serialize_inline_range(&sorted.text, &sorted.entities, 0, total, None)
}

fn skip_entity(entities: &[EntityInText], idx: usize, skip: Option<usize>) -> bool {
    let Some(s) = skip else {
        return false;
    };
    idx == s
        || (idx < s
            && entities[idx].offset == entities[s].offset
            && entities[idx].length == entities[s].length)
}

fn serialize_inline_range(
    text: &str,
    entities: &[EntityInText],
    from: usize,
    till: usize,
    skip: Option<usize>,
) -> String {
    let mut out = String::new();
    let mut pos = from;
    while pos < till {
        let mut next_start = till;
        let mut chosen = None;
        for (idx, entity) in entities.iter().enumerate() {
            if skip_entity(entities, idx, skip) {
                continue;
            }
            if entity.length > 0 && entity.offset >= pos && entity.end() <= till {
                if entity.offset < next_start {
                    next_start = entity.offset;
                    chosen = Some(idx);
                }
                if entity.offset == pos {
                    chosen = Some(idx);
                    break;
                }
            }
        }
        if next_start > pos {
            let b0 = utf16_to_byte_offset(text, pos);
            let b1 = utf16_to_byte_offset(text, next_start);
            out.push_str(&text[b0..b1]);
            pos = next_start;
            continue;
        }
        let Some(idx) = chosen else {
            let b0 = utf16_to_byte_offset(text, pos);
            let b1 = utf16_to_byte_offset(text, till);
            out.push_str(&text[b0..b1]);
            break;
        };
        let entity = &entities[idx];
        let e_end = entity.end();
        let inner = serialize_inline_range(text, entities, entity.offset, e_end, Some(idx));
        match entity.kind {
            EntityType::Bold => {
                out.push_str("**");
                out.push_str(&inner);
                out.push_str("**");
            }
            EntityType::Italic => {
                out.push('*');
                out.push_str(&inner);
                out.push('*');
            }
            EntityType::Underline => {
                out.push_str("<u>");
                out.push_str(&inner);
                out.push_str("</u>");
            }
            EntityType::StrikeOut => {
                out.push_str("~~");
                out.push_str(&inner);
                out.push_str("~~");
            }
            EntityType::Spoiler => {
                out.push_str("||");
                out.push_str(&inner);
                out.push_str("||");
            }
            EntityType::Subscript => {
                out.push_str("<sub>");
                out.push_str(&inner);
                out.push_str("</sub>");
            }
            EntityType::Superscript => {
                out.push_str("<sup>");
                out.push_str(&inner);
                out.push_str("</sup>");
            }
            EntityType::Marked => {
                out.push_str("<mark>");
                out.push_str(&inner);
                out.push_str("</mark>");
            }
            EntityType::Code => {
                let ticks = longest_backtick_run(&inner) + 1;
                let fence = "`".repeat(ticks);
                let pad = inner.starts_with('`') || inner.ends_with('`');
                out.push_str(&fence);
                if pad {
                    out.push(' ');
                }
                out.push_str(&inner);
                if pad {
                    out.push(' ');
                }
                out.push_str(&fence);
            }
            EntityType::CustomUrl => {
                out.push('[');
                out.push_str(&inner);
                out.push_str("](");
                out.push_str(&entity.data);
                out.push(')');
            }
            EntityType::CustomEmoji => {
                if let Some(obj) = parse_inline_text_object_entity(&entity.data) {
                    match obj {
                        InlineTextObjectEntity::Formula(f) => {
                            let tex = if !f.trimmed_tex.is_empty() {
                                f.trimmed_tex
                            } else {
                                trim_math_source(&f.copy_source).to_string()
                            };
                            out.push('$');
                            out.push_str(&tex);
                            out.push('$');
                        }
                        InlineTextObjectEntity::IvImage(img) => {
                            out.push_str(&img.replacement_text);
                        }
                        InlineTextObjectEntity::Button(btn) => {
                            out.push('[');
                            out.push_str(&serialize_inline_text(&btn.label));
                            out.push(']');
                        }
                    }
                } else {
                    out.push_str(&inner);
                }
            }
            _ => out.push_str(&inner),
        }
        pos = e_end;
    }
    out
}
