//! Copying selected message text (TGB-3016), ported from tdesktop's
//! `HistoryInner::getSelectedTextParts` / `copySelectedText` (`history_inner_widget.cpp`) and
//! `HistorySelectedItemsTextParts` (`history_item_text.cpp`).
//!
//! - A text selection inside one message copies just the selected text.
//! - One whole selected message copies its text.
//! - Several messages (selected items, or a drag that crossed into other messages) copy one
//!   line per message: `"[date time] Author: text"`, with the reply and forward context
//!   tdesktop adds, separated by `"\n"`.
//!
//! The plain-text clipboard gets tdesktop's `TextForMimeData::expanded` (text links become
//! `"text (url)"`); the entities stay in [`CopiedRichText`] for pasting back into the compose
//! field. tdesktop shows no toast for any of these copies.

use tg_text::{TextForMimeData, TextWithEntities};
use tg_ui::widgets::menu::MenuItem;

use crate::history::backend::HistoryMessage;
use crate::history::sections_model::CopiedRichText;

/// Context menu id of "Copy Selected Text" / "Copy Selected as Text".
pub const MENU_ID_COPY_SELECTED: &str = "copy_selected";

/// `kSelectedCopyReplyPreviewLimit` (`history_item_text.cpp`).
const REPLY_PREVIEW_LIMIT: usize = 64;

/// Whether the context menu was opened over the selection (`isUponSelected` in
/// `HistoryInner::showContextMenu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UponSelected {
    /// Not over a selection: the menu offers "Copy Text" for the whole message.
    No,
    /// Over a text selection inside one message (`isUponSelected == 1`).
    Text,
    /// Over selected messages (`isUponSelected > 1`).
    Items,
}

/// What a copy of the current selection puts on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionCopy {
    /// Plain clipboard text (`TextForMimeData::expanded`).
    pub clipboard: String,
    /// The same text with its entities, kept for a formatted paste into the compose field.
    pub rich: CopiedRichText,
}

impl SelectionCopy {
    fn from_text(mut text: TextWithEntities) -> Option<Self> {
        if text.text.is_empty() {
            return None;
        }
        text.sort_entities();
        let clipboard = clipboard_text(&text);
        Some(Self {
            clipboard,
            rich: CopiedRichText { content: text },
        })
    }
}

/// tdesktop's plain clipboard text for `text`: text links expanded to `"text (url)"`.
#[must_use]
pub fn clipboard_text(text: &TextWithEntities) -> String {
    let mut sorted;
    let text = if text.entities.is_sorted_by_key(|e| e.offset()) {
        text
    } else {
        sorted = text.clone();
        sorted.sort_entities();
        &sorted
    };
    TextForMimeData::with_expanded_links(text).expanded
}

/// A text selection inside one message (`view->selectedText(selection)`): the selected piece.
#[must_use]
pub fn text_selection_copy(piece: TextWithEntities) -> Option<SelectionCopy> {
    SelectionCopy::from_text(piece)
}

/// Whole messages in list order (`HistorySelectedItemsTextParts`). Service messages are skipped.
#[must_use]
pub fn items_copy(items: &[&HistoryMessage]) -> Option<SelectionCopy> {
    let items: Vec<&HistoryMessage> = items.iter().copied().filter(|m| !m.is_service()).collect();
    match items.as_slice() {
        [] => None,
        [single] => SelectionCopy::from_text(item_text(single)),
        many => {
            let mut out = TextWithEntities::default();
            for (i, item) in many.iter().enumerate() {
                if i > 0 {
                    out.append_char('\n');
                }
                out.append(wrapped_item_text(item));
            }
            SelectionCopy::from_text(out)
        }
    }
}

/// `HistoryItemText`: the message text, or its media summary when it has no text.
fn item_text(msg: &HistoryMessage) -> TextWithEntities {
    if msg.text.is_empty() {
        TextWithEntities::simple(msg.preview_snippet())
    } else {
        msg.text.clone()
    }
}

/// `HistorySelectedItemWrappedText` with `richContext` (several messages).
fn wrapped_item_text(msg: &HistoryMessage) -> TextWithEntities {
    let body = item_text(msg);
    let forwarded = forwarded_line(msg);
    let reply = reply_context(msg);

    let mut head = format!("[{}] ", short_date_time(msg.date_unix));
    head.push_str(&selected_copy_author_name(msg, forwarded.is_some()));
    if let Some((name, _)) = &reply {
        head.push(' ');
        head.push_str(&tg_lang::tr::lng_context_copy_in_reply_to(name));
    }
    head.push(':');
    if let Some(line) = &forwarded {
        head.push('\n');
        head.push_str(line);
    }
    match &reply {
        Some((_, quote)) if !quote.is_empty() => {
            head.push_str("\n> ");
            head.push_str(quote);
            if !body.text.is_empty() {
                head.push('\n');
            }
        }
        _ if forwarded.is_none() => head.push(' '),
        _ if !body.text.is_empty() => head.push('\n'),
        _ => {}
    }
    let mut out = TextWithEntities::simple(head);
    out.append(body);
    out
}

/// `SelectedCopyAuthorName`: the original sender of a forward, else the author. With a
/// forwarded line shown, the line names the original sender and the head names the author.
fn selected_copy_author_name(msg: &HistoryMessage, has_forwarded_line: bool) -> String {
    if !has_forwarded_line
        && let Some(fwd) = &msg.forward
        && !fwd.from_name.is_empty()
    {
        return fwd.from_name.clone();
    }
    msg.sender_name.clone()
}

/// `ForwardedLineForSelectedCopy`: `"Forwarded from {user}"` on one line.
fn forwarded_line(msg: &HistoryMessage) -> Option<String> {
    let fwd = msg.forward.as_ref()?;
    if fwd.from_name.is_empty() {
        return None;
    }
    Some(single_line(&tg_lang::tr::lng_forwarded(&fwd.from_name)))
}

/// `ReplyContextForSelectedCopy`: the replied-to author and the quote (the exact manual quote,
/// else the reply preview cut to [`REPLY_PREVIEW_LIMIT`]).
fn reply_context(msg: &HistoryMessage) -> Option<(String, String)> {
    let reply = msg.reply.as_ref()?;
    if reply.author_name.is_empty() {
        return None;
    }
    let quote = match reply.quote_text.as_deref() {
        Some(q) if !q.is_empty() => single_line(q),
        _ => limit_reply_preview(&reply.preview_text),
    };
    Some((reply.author_name.clone(), quote))
}

/// `LimitNonExactReplyPreview`: one line of at most 64 UTF-16 units, `"…"` when cut.
fn limit_reply_preview(text: &str) -> String {
    let line = single_line(text);
    let line = line.trim();
    if line.encode_utf16().count() <= REPLY_PREVIEW_LIMIT {
        return line.to_owned();
    }
    let mut units = 0;
    let mut cut = String::new();
    for ch in line.chars() {
        units += ch.len_utf16();
        if units > REPLY_PREVIEW_LIMIT {
            break;
        }
        cut.push(ch);
    }
    let mut cut = cut.trim_end().to_owned();
    cut.push('…');
    cut
}

/// `TextUtilities::SingleLine`: line and paragraph breaks become spaces.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => ' ',
            c => c,
        })
        .collect()
}

/// `QLocale::toString(ItemDateTime(item), QLocale::ShortFormat)` in the client's short date
/// and 24-hour time convention: `"DD.MM.YY H:mm"`.
fn short_date_time(date_unix: i32) -> String {
    format_short_date_time(tg_history::datetime::LocalDateTime::from_unix_local(
        i64::from(date_unix),
    ))
}

fn format_short_date_time(dt: tg_history::datetime::LocalDateTime) -> String {
    format!(
        "{:02}.{:02}.{:02} {}",
        dt.date.day,
        dt.date.month,
        dt.date.year.rem_euclid(100),
        tg_history::datetime::format_time_short(dt)
    )
}

/// Puts "Copy Selected Text" / "Copy Selected as Text" where tdesktop does (after the reply
/// actions) when the menu opens over the selection, and drops the whole-message "Copy Text",
/// which tdesktop offers only away from the selection.
pub fn apply_upon_selected(items: &mut Vec<MenuItem>, upon: UponSelected) {
    if upon == UponSelected::No {
        return;
    }
    items.retain(|item| !matches!(item, MenuItem::Action { id, .. } if id == "copy_text"));
    let label = if upon == UponSelected::Items {
        tg_lang::tr::lng_context_copy_selected_items()
    } else {
        tg_lang::tr::lng_context_copy_selected()
    };
    let after_reply = items
        .iter()
        .rposition(|item| {
            matches!(item, MenuItem::Action { id, .. } if id == "reply" || id == "reply_quote")
        })
        .map_or(0, |i| i + 1);
    items.insert(after_reply, MenuItem::action(MENU_ID_COPY_SELECTED, label));
}

#[cfg(test)]
mod tests {
    use tg_text::{EntityInText, EntityType};

    use super::*;
    use crate::history::backend::{ForwardHeader, ReplyHeader};
    use crate::history::tests_w4_text_snap::{group_peer, message};

    fn msg(id: i32, name: &str, text: &str) -> HistoryMessage {
        message(group_peer(), id, 1000 + id as u64, name, text)
    }

    #[test]
    fn a_text_link_is_expanded_on_the_plain_clipboard() {
        let text = TextWithEntities {
            text: "see docs here".to_owned(),
            entities: vec![
                EntityInText::new(EntityType::Bold, 0, 3),
                EntityInText::with_data(EntityType::CustomUrl, 4, 4, "https://example.org/d"),
            ],
        };
        let copy = text_selection_copy(text.clone()).expect("copy");
        assert_eq!(copy.clipboard, "see docs (https://example.org/d) here");
        assert_eq!(
            copy.rich.content, text,
            "entities kept for the compose paste"
        );
    }

    #[test]
    fn one_whole_message_copies_its_text_only() {
        let a = msg(1, "Mira Vance", "hello");
        let copy = items_copy(&[&a]).expect("copy");
        assert_eq!(copy.clipboard, "hello");
    }

    #[test]
    fn the_time_is_the_short_date_and_24_hour_time() {
        let dt = tg_history::datetime::LocalDateTime::from_unix_with_offset(1_795_340_000, 0);
        assert_eq!(format_short_date_time(dt), "22.11.26 9:33");
    }

    #[test]
    fn several_messages_get_a_time_and_author_line_each() {
        let a = msg(1, "Mira Vance", "first");
        let mut b = msg(2, "Ivo Marten", "second");
        b.text.entities = vec![EntityInText::new(EntityType::Italic, 0, 6)];
        let copy = items_copy(&[&a, &b]).expect("copy");
        assert_eq!(
            copy.clipboard,
            format!(
                "[{}] Mira Vance: first\n[{}] Ivo Marten: second",
                short_date_time(a.date_unix),
                short_date_time(b.date_unix)
            )
        );
        let italic = &copy.rich.content.entities[0];
        assert_eq!(italic.entity_type(), EntityType::Italic);
        let start = copy.clipboard.find("second").expect("body") as i32;
        assert_eq!(italic.offset(), start, "entities shift past the head");
    }

    #[test]
    fn reply_and_forward_context_follow_tdesktop() {
        let mut a = msg(1, "Mira Vance", "sure");
        a.reply = Some(ReplyHeader {
            reply_to_msg_id: tg_app_core::stand_in::types::MsgId(7),
            author_name: "Ivo Marten".to_owned(),
            author_color_id: 0,
            author_accent: None,
            preview_text: "lunch\nat noon?".to_owned(),
            preview_thumb_rgb: None,
            preview_thumb: None,
            preview_colored_len: 0,
            quote_text: None,
        });
        let mut b = msg(2, "Noor Ilves", "news");
        b.forward = Some(ForwardHeader {
            from_name: "Harbor Daily".to_owned(),
            from_peer: None,
            original_date: String::new(),
        });
        let copy = items_copy(&[&a, &b]).expect("copy");
        assert_eq!(
            copy.clipboard,
            format!(
                "[{}] Mira Vance in reply to Ivo Marten:\n> lunch at noon?\nsure\n\
                 [{}] Noor Ilves:\nForwarded from Harbor Daily\nnews",
                short_date_time(a.date_unix),
                short_date_time(b.date_unix)
            )
        );
    }

    #[test]
    fn a_long_reply_preview_is_cut_with_an_ellipsis() {
        let long = "word ".repeat(30);
        let cut = limit_reply_preview(&long);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.encode_utf16().count() <= REPLY_PREVIEW_LIMIT + 1);
    }

    #[test]
    fn the_menu_offers_copy_selected_after_reply_instead_of_copy_text() {
        let mut items = vec![
            MenuItem::action("reply", "Reply"),
            MenuItem::action("reply_quote", "Reply with Quote"),
            MenuItem::action("copy_text", "Copy Text"),
            MenuItem::action("delete", "Delete"),
        ];
        let ids = |items: &[MenuItem]| -> Vec<String> {
            items
                .iter()
                .filter_map(|i| match i {
                    MenuItem::Action { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        };
        let original = items.clone();
        apply_upon_selected(&mut items, UponSelected::No);
        assert_eq!(ids(&items), ids(&original));

        apply_upon_selected(&mut items, UponSelected::Text);
        assert_eq!(
            ids(&items),
            ["reply", "reply_quote", MENU_ID_COPY_SELECTED, "delete"]
        );
        assert!(matches!(
            &items[2],
            MenuItem::Action { label, .. } if label == "Copy Selected Text"
        ));

        let mut items = original;
        apply_upon_selected(&mut items, UponSelected::Items);
        assert!(matches!(
            &items[2],
            MenuItem::Action { label, .. } if label == "Copy Selected as Text"
        ));
    }
}
