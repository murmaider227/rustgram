//! Port of `export/output/export_output_json.h` and `export/output/export_output_json.cpp`.
//!
//! Produces byte-for-byte identical `result.json` formatting, key ordering, indentation
//! (1 space per nesting level), string escaping, entity `text` / `text_entities` arrays,
//! and ISO + unixtime date fields as Telegram Desktop.

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;

use crate::data::{
    ContactsList, DialogInfo, DialogType, DialogsInfo, Document, File, InlineButtonAction,
    InlineButtonActionType, MediaContent, Message, MessagesSlice, NANOS_IN_GRAM, Peer,
    PersonalInfo, PhoneCallState, Photo, ProfileMusicInfo, ProfileMusicSlice, Reaction,
    ReactionType, RichBlock, RichBlockKind, RichCaption, RichChannel, RichChannelSource,
    RichListItem, RichListItemContent, RichListKind, RichMapPoint, RichMapPointSource, RichMessage,
    RichQuoteContent, RichRelatedArticle, RichTableAlignment, RichTableCell, RichTableRow,
    RichTableVerticalAlignment, RichTaskState, RichText, RichTextType, SecureValueType,
    ServiceAction, SessionsList, SkipReason, StoriesInfo, StoriesSlice, TextPart, TextPartType,
    TopPeer, User, UserpicsInfo, UserpicsSlice, float_to_string, format_iso_date_with_offset,
    format_phone_number, peer_to_bare_id, skip_message_by_date, sorted_contacts_indices,
};
use crate::output::file::OutputFile;
use crate::output::result::OutputResult;
use crate::output::stats::Stats;
use crate::output::{AbstractWriter, OutputFormat};
use crate::settings::{Environment, Settings};
use crate::stand_in::{
    PeerId, TimeId, UserId, base64_url_no_pad, credits_is_empty, credits_is_ton,
};

/// JSON container kind for nesting stack (port of `details::JsonContext::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Object,
    Array,
}

/// JSON indentation and nesting tracker (port of `details::JsonContext`).
#[derive(Debug, Default)]
struct JsonContext {
    nesting: Vec<ContainerKind>,
    utc_offset_seconds: i32,
}

impl JsonContext {
    fn indentation(&self) -> String {
        " ".repeat(self.nesting.len())
    }

    fn indentation_plus(&self, extra: usize) -> String {
        " ".repeat(self.nesting.len() + extra)
    }
}

/// Serializes UTF-8 bytes into a JSON string literal following tdesktop's exact `SerializeString` rules.
pub fn serialize_json_string(value: &str) -> Vec<u8> {
    serialize_json_bytes(value.as_bytes())
}

/// Byte-level port of `SerializeString` in `export/output/export_output_json.cpp:33-79`.
pub fn serialize_json_bytes(value: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(2 + value.len() * 2);
    result.push(b'"');
    let mut i = 0;
    while i < value.len() {
        let ch = value[i];
        match ch {
            b'\n' => result.extend_from_slice(b"\\n"),
            b'\r' => result.extend_from_slice(b"\\r"),
            b'\t' => result.extend_from_slice(b"\\t"),
            b'"' => result.extend_from_slice(b"\\\""),
            b'\\' => result.extend_from_slice(b"\\\\"),
            0..=31 => {
                result.extend_from_slice(b"\\u00");
                result.push(b'0' + (ch >> 4));
                let left = ch & 0x0F;
                if left >= 10 {
                    result.push(b'A' + (left - 10));
                } else {
                    result.push(b'0' + left);
                }
            }
            0xE2 if i + 2 < value.len() && value[i + 1] == 0x80 => {
                if value[i + 2] == 0xA8 {
                    result.extend_from_slice(b"\\u2028");
                    i += 2;
                } else if value[i + 2] == 0xA9 {
                    result.extend_from_slice(b"\\u2029");
                    i += 2;
                } else {
                    result.push(ch);
                }
            }
            _ => result.push(ch),
        }
        i += 1;
    }
    result.push(b'"');
    result
}

fn serialize_date(ctx: &JsonContext, date: TimeId) -> Vec<u8> {
    serialize_json_string(&format_iso_date_with_offset(date, ctx.utc_offset_seconds))
}

fn serialize_date_raw(date: TimeId) -> Vec<u8> {
    serialize_json_string(&date.to_string())
}

fn string_allow_empty(data: &str) -> Vec<u8> {
    if data.is_empty() {
        Vec::new()
    } else {
        serialize_json_string(data)
    }
}

fn string_allow_null(data: &str) -> Vec<u8> {
    if data.is_empty() {
        b"null".to_vec()
    } else {
        serialize_json_string(data)
    }
}

fn float_allow_null(value: f64) -> Vec<u8> {
    if value.is_finite() {
        float_to_string(value).into_bytes()
    } else {
        b"null".to_vec()
    }
}

fn serialize_object(context: &mut JsonContext, values: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let indent = context.indentation();
    context.nesting.push(ContainerKind::Object);
    let next = format!("\n{}", context.indentation());

    let mut first = true;
    let mut result = Vec::new();
    result.push(b'{');
    for (key, value) in values {
        if value.is_empty() {
            continue;
        }
        if first {
            first = false;
        } else {
            result.push(b',');
        }
        result.extend_from_slice(next.as_bytes());
        result.extend_from_slice(&serialize_json_string(key));
        result.extend_from_slice(b": ");
        result.extend_from_slice(value);
    }
    result.push(b'\n');
    result.extend_from_slice(indent.as_bytes());
    result.push(b'}');
    context.nesting.pop();
    result
}

fn serialize_array(context: &JsonContext, values: &[Vec<u8>]) -> Vec<u8> {
    let indent = context.indentation();
    let next = format!("\n{}", context.indentation_plus(1));
    let mut first = true;
    let mut result = Vec::new();
    result.push(b'[');
    for value in values {
        if first {
            first = false;
        } else {
            result.push(b',');
        }
        result.extend_from_slice(next.as_bytes());
        result.extend_from_slice(value);
    }
    result.push(b'\n');
    result.extend_from_slice(indent.as_bytes());
    result.push(b']');
    result
}

fn serialize_text(
    context: &mut JsonContext,
    data: &[TextPart],
    serialize_to_objects: bool,
) -> Vec<u8> {
    if data.is_empty() {
        return if serialize_to_objects {
            b"[]".to_vec()
        } else {
            serialize_json_string("")
        };
    }

    context.nesting.push(ContainerKind::Array);
    let mut items = Vec::with_capacity(data.len());
    for part in data {
        if part.kind == TextPartType::Text && !serialize_to_objects {
            items.push(serialize_json_string(&part.text));
            continue;
        }
        let type_string = match part.kind {
            TextPartType::Unknown => "unknown",
            TextPartType::Mention => "mention",
            TextPartType::Hashtag => "hashtag",
            TextPartType::BotCommand => "bot_command",
            TextPartType::Url => "link",
            TextPartType::Email => "email",
            TextPartType::Bold => "bold",
            TextPartType::Italic => "italic",
            TextPartType::Code => "code",
            TextPartType::Pre => "pre",
            TextPartType::Text => "plain",
            TextPartType::TextUrl => "text_link",
            TextPartType::MentionName => "mention_name",
            TextPartType::Phone => "phone",
            TextPartType::Cashtag => "cashtag",
            TextPartType::Underline => "underline",
            TextPartType::Strike => "strikethrough",
            TextPartType::Blockquote => "blockquote",
            TextPartType::BankCard => "bank_card",
            TextPartType::TonAddress => "ton_address",
            TextPartType::Spoiler => "spoiler",
            TextPartType::CustomEmoji => "custom_emoji",
        };
        let (additional_name, additional_value) = match part.kind {
            TextPartType::MentionName => ("user_id", part.additional.as_bytes().to_vec()),
            TextPartType::CustomEmoji => ("document_id", serialize_json_string(&part.additional)),
            TextPartType::Pre => ("language", serialize_json_string(&part.additional)),
            TextPartType::TextUrl => ("href", serialize_json_string(&part.additional)),
            TextPartType::Blockquote => (
                "collapsed",
                if part.additional.is_empty() {
                    b"false".to_vec()
                } else {
                    b"true".to_vec()
                },
            ),
            _ => ("none", Vec::new()),
        };
        items.push(serialize_object(
            context,
            &[
                ("type", serialize_json_string(type_string)),
                ("text", serialize_json_string(&part.text)),
                (additional_name, additional_value),
            ],
        ));
    }
    context.nesting.pop();

    if !serialize_to_objects && data.len() == 1 && data[0].kind == TextPartType::Text {
        return items.swap_remove(0);
    }
    serialize_array(context, &items)
}

fn format_username(username: &str) -> String {
    if username.is_empty() {
        String::new()
    } else {
        format!("@{username}")
    }
}

fn serialize_bool(value: bool) -> Vec<u8> {
    if value {
        b"true".to_vec()
    } else {
        b"false".to_vec()
    }
}

fn serialize_number_string(value: impl std::fmt::Display) -> Vec<u8> {
    serialize_json_string(&value.to_string())
}

fn append_rich_file_availability(
    values: &mut Vec<(&'static str, Vec<u8>)>,
    file: Option<&File>,
    path_key: &'static str,
    reason_key: &'static str,
) {
    if let Some(f) = file {
        match f.skip_reason {
            SkipReason::Unavailable => {
                values.push((reason_key, serialize_json_string("unavailable")));
                return;
            }
            SkipReason::FileType => {
                values.push((reason_key, serialize_json_string("file_type")));
                return;
            }
            SkipReason::FileSize => {
                values.push((reason_key, serialize_json_string("file_size")));
                return;
            }
            SkipReason::DateLimits => {
                values.push((reason_key, serialize_json_string("date_limits")));
                return;
            }
            SkipReason::None => {
                if !f.relative_path.is_empty() {
                    values.push((path_key, serialize_json_string(&f.relative_path)));
                    return;
                }
            }
        }
    }
    values.push((reason_key, serialize_json_string("unavailable")));
}

fn append_rich_photo_metadata(
    values: &mut Vec<(&'static str, Vec<u8>)>,
    photo: Option<&Photo>,
    path_key: &'static str,
    reason_key: &'static str,
    size_key: &'static str,
    width_key: &'static str,
    height_key: &'static str,
) {
    append_rich_file_availability(values, photo.map(|p| &p.image.file), path_key, reason_key);
    let Some(photo) = photo else {
        return;
    };
    values.push((size_key, photo.image.file.size.to_string().into_bytes()));
    if photo.image.width != 0 && photo.image.height != 0 {
        values.push((width_key, photo.image.width.to_string().into_bytes()));
        values.push((height_key, photo.image.height.to_string().into_bytes()));
    }
}

fn append_rich_document_metadata(
    values: &mut Vec<(&'static str, Vec<u8>)>,
    document: Option<&Document>,
    include_dimensions: bool,
) {
    append_rich_file_availability(
        values,
        document.map(|d| &d.file),
        "file",
        "file_skip_reason",
    );
    let Some(doc) = document else {
        return;
    };
    if !doc.name.is_empty() {
        values.push(("file_name", serialize_json_string(&doc.name)));
    }
    values.push(("file_size", doc.file.size.to_string().into_bytes()));
    if doc.thumb.width > 0 {
        append_rich_file_availability(
            values,
            Some(&doc.thumb.file),
            "thumbnail",
            "thumbnail_skip_reason",
        );
        values.push((
            "thumbnail_file_size",
            doc.thumb.file.size.to_string().into_bytes(),
        ));
    }
    if doc.is_sticker {
        values.push(("media_type", serialize_json_string("sticker")));
        if !doc.sticker_emoji.is_empty() {
            values.push(("sticker_emoji", serialize_json_string(&doc.sticker_emoji)));
        }
    } else if doc.is_video_message {
        values.push(("media_type", serialize_json_string("video_message")));
    } else if doc.is_voice_message {
        values.push(("media_type", serialize_json_string("voice_message")));
    } else if doc.is_animated {
        values.push(("media_type", serialize_json_string("animation")));
    } else if doc.is_video_file {
        values.push(("media_type", serialize_json_string("video_file")));
    } else if doc.is_audio_file {
        values.push(("media_type", serialize_json_string("audio_file")));
        if !doc.song_performer.is_empty() {
            values.push(("performer", serialize_json_string(&doc.song_performer)));
        }
        if !doc.song_title.is_empty() {
            values.push(("title", serialize_json_string(&doc.song_title)));
        }
    }
    if !doc.mime.is_empty() {
        values.push(("mime_type", serialize_json_string(&doc.mime)));
    }
    if doc.duration != 0 {
        values.push(("duration_seconds", doc.duration.to_string().into_bytes()));
    }
    if include_dimensions && doc.width != 0 && doc.height != 0 {
        values.push(("width", doc.width.to_string().into_bytes()));
        values.push(("height", doc.height.to_string().into_bytes()));
    }
}

fn serialize_rich_array<T, F>(context: &mut JsonContext, data: &[T], mut serializer: F) -> Vec<u8>
where
    F: FnMut(&mut JsonContext, &T) -> Vec<u8>,
{
    if data.is_empty() {
        return b"[]".to_vec();
    }
    let mut values = Vec::with_capacity(data.len());
    context.nesting.push(ContainerKind::Array);
    for item in data {
        values.push(serializer(context, item));
    }
    context.nesting.pop();
    serialize_array(context, &values)
}

fn rich_text_type_str(kind: RichTextType) -> &'static str {
    match kind {
        RichTextType::Empty => "empty",
        RichTextType::Plain => "plain",
        RichTextType::Concat => "concat",
        RichTextType::Bold => "bold",
        RichTextType::Italic => "italic",
        RichTextType::Underline => "underline",
        RichTextType::Strike => "strikethrough",
        RichTextType::Fixed => "code",
        RichTextType::Url => "text_link",
        RichTextType::Email | RichTextType::AutoEmail => "email",
        RichTextType::Phone | RichTextType::AutoPhone => "phone",
        RichTextType::Subscript => "subscript",
        RichTextType::Superscript => "superscript",
        RichTextType::Marked => "marked",
        RichTextType::Anchor => "anchor",
        RichTextType::Math => "math",
        RichTextType::CustomEmoji => "custom_emoji",
        RichTextType::Spoiler => "spoiler",
        RichTextType::Mention => "mention",
        RichTextType::Hashtag => "hashtag",
        RichTextType::BotCommand => "bot_command",
        RichTextType::Cashtag => "cashtag",
        RichTextType::AutoUrl => "link",
        RichTextType::BankCard => "bank_card",
        RichTextType::TonAddress => "ton_address",
        RichTextType::MentionName => "mention_name",
        RichTextType::FormattedDate => "formatted_date",
        RichTextType::InlineImage => "inline_image",
        RichTextType::Diff => "diff",
        RichTextType::Button => "button",
    }
}

fn serialize_rich_button_action(context: &mut JsonContext, action: &InlineButtonAction) -> Vec<u8> {
    let mut values: Vec<(&str, Vec<u8>)> = vec![("type", serialize_json_string(action.type_str()))];
    context.nesting.push(ContainerKind::Object);
    match action.kind {
        InlineButtonActionType::Url | InlineButtonActionType::WebView => {
            values.push(("url", serialize_json_string(&action.url)));
        }
        InlineButtonActionType::Auth => {
            values.push(("url", serialize_json_string(&action.url)));
            if let Some(ref forward_text) = action.forward_text {
                values.push(("forward_text", serialize_json_string(forward_text)));
            }
            values.push(("button_id", action.button_id.to_string().into_bytes()));
        }
        InlineButtonActionType::Callback | InlineButtonActionType::CallbackWithPassword => {
            values.push((
                "requires_password",
                serialize_bool(action.requires_password),
            ));
            values.push((
                "dataBase64",
                serialize_json_string(&base64_url_no_pad(&action.callback_data)),
            ));
            values.push(("data", serialize_json_string("")));
        }
        InlineButtonActionType::Game
        | InlineButtonActionType::Buy
        | InlineButtonActionType::Disabled => {}
        InlineButtonActionType::SwitchInline | InlineButtonActionType::SwitchInlineSame => {
            values.push(("query", serialize_json_string(&action.query)));
            values.push(("same_peer", serialize_bool(action.same_peer)));
            if let Some(ref peer_types) = action.peer_types {
                let arr = serialize_rich_array(context, peer_types, |_, pt| {
                    serialize_json_string(pt.as_str())
                });
                values.push(("peer_types", arr));
            }
        }
        InlineButtonActionType::UserProfile => {
            values.push(("user_id", serialize_number_string(action.user_id)));
        }
        InlineButtonActionType::CopyText => {
            values.push(("copy_text", serialize_json_string(&action.copy_text)));
        }
    }
    context.nesting.pop();
    serialize_object(context, &values)
}

fn append_rich_button_fields(
    values: &mut Vec<(&'static str, Vec<u8>)>,
    context: &mut JsonContext,
    message: &RichMessage,
    button: &RichText,
) {
    let child = button.children.first().cloned().unwrap_or_default();
    values.push(("text", serialize_rich_text(context, message, &child)));
    if let Some(ref payload) = button.button {
        values.push((
            "button",
            serialize_rich_button_action(context, &payload.action),
        ));
        if let Some(style) = payload.style {
            values.push(("style", serialize_json_string(style.as_str())));
        }
    }
}

fn serialize_rich_text_child(
    context: &mut JsonContext,
    message: &RichMessage,
    children: &[RichText],
) -> Vec<u8> {
    if let Some(first) = children.first() {
        serialize_rich_text(context, message, first)
    } else {
        serialize_rich_text(context, message, &RichText::default())
    }
}

fn serialize_rich_text(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichText,
) -> Vec<u8> {
    let mut values: Vec<(&'static str, Vec<u8>)> =
        vec![("type", serialize_json_string(rich_text_type_str(data.kind)))];
    context.nesting.push(ContainerKind::Object);
    match data.kind {
        RichTextType::Empty => {}
        RichTextType::Plain => {
            values.push(("text", serialize_json_string(&data.text)));
        }
        RichTextType::Concat => {
            let arr = serialize_rich_array(context, &data.children, |ctx, t| {
                serialize_rich_text(ctx, message, t)
            });
            values.push(("text", arr));
        }
        RichTextType::Bold
        | RichTextType::Italic
        | RichTextType::Underline
        | RichTextType::Strike
        | RichTextType::Fixed
        | RichTextType::Subscript
        | RichTextType::Superscript
        | RichTextType::Marked
        | RichTextType::Spoiler
        | RichTextType::Mention
        | RichTextType::Hashtag
        | RichTextType::BotCommand
        | RichTextType::Cashtag
        | RichTextType::AutoUrl
        | RichTextType::AutoEmail
        | RichTextType::AutoPhone
        | RichTextType::BankCard
        | RichTextType::TonAddress => {
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::Url => {
            values.push(("href", serialize_json_string(&data.data)));
            if data.id != 0 {
                values.push(("webpage_id", serialize_number_string(data.id)));
            }
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::Email => {
            values.push(("email", serialize_json_string(&data.data)));
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::Phone => {
            values.push(("phone", serialize_json_string(&data.data)));
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::Anchor => {
            values.push(("name", serialize_json_string(&data.data)));
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::Math => {
            values.push(("source", serialize_json_string(&data.data)));
        }
        RichTextType::CustomEmoji => {
            values.push(("text", serialize_json_string(&data.text)));
            values.push((
                "document_id",
                serialize_json_string(&data.custom_emoji_data),
            ));
        }
        RichTextType::MentionName => {
            values.push(("user_id", serialize_number_string(data.id)));
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
        }
        RichTextType::FormattedDate => {
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
            values.push(("date", serialize_date(context, data.date)));
            values.push(("date_unixtime", serialize_date_raw(data.date)));
            values.push(("relative", serialize_bool(data.relative)));
            values.push(("short_time", serialize_bool(data.short_time)));
            values.push(("long_time", serialize_bool(data.long_time)));
            values.push(("short_date", serialize_bool(data.short_date)));
            values.push(("long_date", serialize_bool(data.long_date)));
            values.push(("day_of_week", serialize_bool(data.day_of_week)));
        }
        RichTextType::InlineImage => {
            values.push(("document_id", serialize_number_string(data.id)));
            append_rich_document_metadata(&mut values, message.documents.get(&data.id), false);
            values.push(("width", data.width.to_string().into_bytes()));
            values.push(("height", data.height.to_string().into_bytes()));
        }
        RichTextType::Diff => {
            values.push(("unsupported", b"true".to_vec()));
            let child = serialize_rich_text_child(context, message, &data.children);
            values.push(("text", child));
            let old = serialize_rich_text_child(context, message, &data.old_children);
            values.push(("old_text", old));
        }
        RichTextType::Button => {
            append_rich_button_fields(&mut values, context, message, data);
        }
    }
    context.nesting.pop();
    serialize_object(context, &values)
}

fn serialize_rich_caption(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichCaption,
) -> Vec<u8> {
    context.nesting.push(ContainerKind::Object);
    let text = serialize_rich_text(context, message, &data.text);
    let credit = serialize_rich_text(context, message, &data.credit);
    context.nesting.pop();
    serialize_object(context, &[("text", text), ("credit", credit)])
}

fn serialize_rich_list_item(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichListItem,
) -> Vec<u8> {
    let task_state = match data.task_state {
        RichTaskState::None => "none",
        RichTaskState::Unchecked => "unchecked",
        RichTaskState::Checked => "checked",
    };
    let content_str = match data.content {
        RichListItemContent::Text => "text",
        RichListItemContent::Blocks => "blocks",
    };
    let mut values: Vec<(&str, Vec<u8>)> = vec![
        ("task_state", serialize_json_string(task_state)),
        ("content", serialize_json_string(content_str)),
    ];
    context.nesting.push(ContainerKind::Object);
    match data.content {
        RichListItemContent::Text => {
            let empty = RichText::default();
            let text = data.text.as_ref().unwrap_or(&empty);
            values.push(("text", serialize_rich_text(context, message, text)));
        }
        RichListItemContent::Blocks => {
            let blocks = serialize_rich_array(context, &data.blocks, |ctx, b| {
                serialize_rich_block(ctx, message, b)
            });
            values.push(("blocks", blocks));
        }
    }
    context.nesting.pop();
    if let Some(ref num) = data.num {
        values.push(("num", serialize_json_string(num)));
    }
    if let Some(value) = data.value {
        values.push(("value", value.to_string().into_bytes()));
    }
    if let Some(ref item_type) = data.item_type {
        values.push(("item_type", serialize_json_string(item_type)));
    }
    serialize_object(context, &values)
}

fn serialize_rich_table_cell(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichTableCell,
) -> Vec<u8> {
    let mut values: Vec<(&str, Vec<u8>)> = Vec::new();
    if let Some(ref text) = data.text {
        context.nesting.push(ContainerKind::Object);
        let serialized = serialize_rich_text(context, message, text);
        context.nesting.pop();
        values.push(("text", serialized));
    }
    if let Some(colspan) = data.colspan {
        values.push(("colspan", colspan.to_string().into_bytes()));
    }
    if let Some(rowspan) = data.rowspan {
        values.push(("rowspan", rowspan.to_string().into_bytes()));
    }
    values.push(("header", serialize_bool(data.header)));
    let align = match data.alignment {
        RichTableAlignment::Left => "left",
        RichTableAlignment::Center => "center",
        RichTableAlignment::Right => "right",
    };
    let valign = match data.vertical_alignment {
        RichTableVerticalAlignment::Top => "top",
        RichTableVerticalAlignment::Middle => "middle",
        RichTableVerticalAlignment::Bottom => "bottom",
    };
    values.push(("align", serialize_json_string(align)));
    values.push(("vertical_align", serialize_json_string(valign)));
    serialize_object(context, &values)
}

fn serialize_rich_table_row(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichTableRow,
) -> Vec<u8> {
    context.nesting.push(ContainerKind::Object);
    let cells = serialize_rich_array(context, &data.cells, |ctx, cell| {
        serialize_rich_table_cell(ctx, message, cell)
    });
    context.nesting.pop();
    serialize_object(context, &[("cells", cells)])
}

fn serialize_rich_related_article(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichRelatedArticle,
) -> Vec<u8> {
    let mut values: Vec<(&'static str, Vec<u8>)> = vec![
        ("url", serialize_json_string(&data.url)),
        ("webpage_id", serialize_number_string(data.webpage_id)),
    ];
    if let Some(ref title) = data.title {
        values.push(("title", serialize_json_string(title)));
    }
    if let Some(ref description) = data.description {
        values.push(("description", serialize_json_string(description)));
    }
    if let Some(photo_id) = data.photo_id {
        values.push(("photo_id", serialize_number_string(photo_id)));
        append_rich_photo_metadata(
            &mut values,
            message.photos.get(&photo_id),
            "photo",
            "photo_skip_reason",
            "photo_file_size",
            "photo_width",
            "photo_height",
        );
    }
    if let Some(ref author) = data.author {
        values.push(("author", serialize_json_string(author)));
    }
    if let Some(published_date) = data.published_date {
        values.push(("published_date", serialize_date(context, published_date)));
        values.push((
            "published_date_unixtime",
            serialize_date_raw(published_date),
        ));
    }
    serialize_object(context, &values)
}

fn serialize_rich_channel(context: &mut JsonContext, data: &RichChannel) -> Vec<u8> {
    let source = match data.source {
        RichChannelSource::ChatEmpty => "chat_empty",
        RichChannelSource::Chat => "chat",
        RichChannelSource::ChatForbidden => "chat_forbidden",
        RichChannelSource::Channel => "channel",
        RichChannelSource::ChannelForbidden => "channel_forbidden",
        RichChannelSource::Community => "community",
        RichChannelSource::CommunityForbidden => "community_forbidden",
    };
    let mut values: Vec<(&str, Vec<u8>)> = vec![
        ("source_type", serialize_json_string(source)),
        ("id", serialize_number_string(data.id)),
    ];
    if let Some(access_hash) = data.access_hash {
        values.push(("access_hash", serialize_number_string(access_hash)));
    }
    if let Some(ref title) = data.title {
        values.push(("title", serialize_json_string(title)));
    }
    if let Some(ref username) = data.username {
        values.push(("username", serialize_json_string(username)));
    }
    if matches!(
        data.source,
        RichChannelSource::Channel | RichChannelSource::ChannelForbidden
    ) {
        values.push(("broadcast", serialize_bool(data.broadcast)));
        values.push(("megagroup", serialize_bool(data.megagroup)));
        values.push(("monoforum", serialize_bool(data.monoforum)));
    }
    serialize_object(context, &values)
}

fn serialize_rich_map_point(context: &mut JsonContext, data: &RichMapPoint) -> Vec<u8> {
    let source = match data.source {
        RichMapPointSource::GeoPointEmpty => "geo_point_empty",
        RichMapPointSource::GeoPoint => "geo_point",
        RichMapPointSource::InputGeoPointEmpty => "input_geo_point_empty",
        RichMapPointSource::InputGeoPoint => "input_geo_point",
    };
    let mut values: Vec<(&str, Vec<u8>)> = vec![("source_type", serialize_json_string(source))];
    match data.source {
        RichMapPointSource::GeoPointEmpty | RichMapPointSource::InputGeoPointEmpty => {}
        RichMapPointSource::GeoPoint | RichMapPointSource::InputGeoPoint => {
            values.push(("latitude", float_allow_null(data.latitude)));
            values.push(("longitude", float_allow_null(data.longitude)));
        }
    }
    if let Some(access_hash) = data.access_hash {
        values.push(("access_hash", serialize_number_string(access_hash)));
    }
    if let Some(accuracy_radius) = data.accuracy_radius {
        values.push(("accuracy_radius", accuracy_radius.to_string().into_bytes()));
    }
    serialize_object(context, &values)
}

fn rich_block_kind_str(kind: RichBlockKind) -> &'static str {
    match kind {
        RichBlockKind::Unsupported | RichBlockKind::Unknown => "unsupported",
        RichBlockKind::Heading => "heading",
        RichBlockKind::Paragraph => "paragraph",
        RichBlockKind::Footer => "footer",
        RichBlockKind::Thinking => "thinking",
        RichBlockKind::AuthorDate => "author_date",
        RichBlockKind::Code => "code",
        RichBlockKind::Divider => "divider",
        RichBlockKind::Anchor => "anchor",
        RichBlockKind::List => "list",
        RichBlockKind::Quote => "quote",
        RichBlockKind::Photo => "photo",
        RichBlockKind::Video => "video",
        RichBlockKind::Cover => "cover",
        RichBlockKind::Embed => "embed",
        RichBlockKind::EmbedPost => "embed_post",
        RichBlockKind::Collage => "collage",
        RichBlockKind::Slideshow => "slideshow",
        RichBlockKind::Channel => "channel",
        RichBlockKind::Audio => "audio",
        RichBlockKind::File => "file",
        RichBlockKind::Math => "math",
        RichBlockKind::Table => "table",
        RichBlockKind::Details => "details",
        RichBlockKind::RelatedArticles => "related_articles",
        RichBlockKind::Map => "map",
        RichBlockKind::InputMap => "input_map",
        RichBlockKind::ButtonRow => "button_row",
    }
}

fn serialize_rich_block(
    context: &mut JsonContext,
    message: &RichMessage,
    data: &RichBlock,
) -> Vec<u8> {
    let mut values: Vec<(&'static str, Vec<u8>)> = vec![(
        "type",
        serialize_json_string(rich_block_kind_str(data.kind)),
    )];
    context.nesting.push(ContainerKind::Object);
    match data.kind {
        RichBlockKind::Unsupported => {
            values.push((
                "source_type",
                serialize_json_string("page_block_unsupported"),
            ));
            values.push(("unsupported", b"true".to_vec()));
        }
        RichBlockKind::Heading => {
            values.push(("level", data.heading_level.to_string().into_bytes()));
            values.push(("text", serialize_rich_text(context, message, &data.text)));
        }
        RichBlockKind::Paragraph | RichBlockKind::Footer | RichBlockKind::Thinking => {
            values.push(("text", serialize_rich_text(context, message, &data.text)));
        }
        RichBlockKind::AuthorDate => {
            values.push(("author", serialize_rich_text(context, message, &data.text)));
            values.push(("date", serialize_date(context, data.date)));
            values.push(("date_unixtime", serialize_date_raw(data.date)));
        }
        RichBlockKind::Code => {
            values.push(("text", serialize_rich_text(context, message, &data.text)));
            values.push(("language", serialize_json_string(&data.language)));
        }
        RichBlockKind::Divider => {}
        RichBlockKind::Anchor => {
            values.push(("name", serialize_json_string(&data.name)));
        }
        RichBlockKind::List => {
            let list_kind = match data.list_kind {
                RichListKind::Bullet => "bullet",
                RichListKind::Ordered => "ordered",
            };
            values.push(("kind", serialize_json_string(list_kind)));
            if data.list_kind == RichListKind::Ordered {
                values.push(("reversed", serialize_bool(data.ordered_list.reversed)));
                if let Some(start) = data.ordered_list.start {
                    values.push(("start", start.to_string().into_bytes()));
                }
                if let Some(ref lt) = data.ordered_list.list_type {
                    values.push(("list_type", serialize_json_string(lt)));
                }
            }
            let items = serialize_rich_array(context, &data.list_items, |ctx, item| {
                serialize_rich_list_item(ctx, message, item)
            });
            values.push(("items", items));
        }
        RichBlockKind::Quote => {
            let content_str = match data.quote_content {
                RichQuoteContent::Text => "text",
                RichQuoteContent::Blocks => "blocks",
            };
            values.push(("content", serialize_json_string(content_str)));
            values.push(("pullquote", serialize_bool(data.pullquote)));
            match data.quote_content {
                RichQuoteContent::Text => {
                    values.push(("text", serialize_rich_text(context, message, &data.text)));
                }
                RichQuoteContent::Blocks => {
                    let blocks = serialize_rich_array(context, &data.blocks, |ctx, b| {
                        serialize_rich_block(ctx, message, b)
                    });
                    values.push(("blocks", blocks));
                }
            }
            values.push((
                "caption",
                serialize_rich_text(context, message, &data.quote_caption),
            ));
        }
        RichBlockKind::Photo => {
            values.push(("photo_id", serialize_number_string(data.photo_id)));
            append_rich_photo_metadata(
                &mut values,
                message.photos.get(&data.photo_id),
                "photo",
                "photo_skip_reason",
                "photo_file_size",
                "width",
                "height",
            );
            values.push(("spoiler", serialize_bool(data.spoiler)));
            if let Some(ref url) = data.optional_url {
                values.push(("url", serialize_json_string(url)));
            }
            if let Some(webpage_id) = data.optional_webpage_id {
                values.push(("webpage_id", serialize_number_string(webpage_id)));
            }
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::Video => {
            values.push(("document_id", serialize_number_string(data.document_id)));
            append_rich_document_metadata(
                &mut values,
                message.documents.get(&data.document_id),
                true,
            );
            values.push(("autoplay", serialize_bool(data.autoplay)));
            values.push(("loop", serialize_bool(data.is_loop)));
            values.push(("spoiler", serialize_bool(data.spoiler)));
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::Cover => {
            if let Some(first) = data.blocks.first() {
                values.push(("block", serialize_rich_block(context, message, first)));
            }
        }
        RichBlockKind::Embed => {
            if let Some(ref url) = data.optional_url {
                values.push(("url", serialize_json_string(url)));
            }
            if let Some(ref html) = data.html {
                values.push(("html", serialize_json_string(html)));
            }
            if let Some(poster_id) = data.poster_photo_id {
                values.push(("poster_photo_id", serialize_number_string(poster_id)));
                append_rich_photo_metadata(
                    &mut values,
                    message.photos.get(&poster_id),
                    "poster_photo",
                    "poster_photo_skip_reason",
                    "poster_photo_file_size",
                    "poster_photo_width",
                    "poster_photo_height",
                );
            }
            if let Some(w) = data.width {
                values.push(("width", w.to_string().into_bytes()));
            }
            if let Some(h) = data.height {
                values.push(("height", h.to_string().into_bytes()));
            }
            values.push(("full_width", serialize_bool(data.full_width)));
            values.push(("allow_scrolling", serialize_bool(data.allow_scrolling)));
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::EmbedPost => {
            values.push(("url", serialize_json_string(&data.url)));
            values.push(("webpage_id", serialize_number_string(data.webpage_id)));
            values.push((
                "author_photo_id",
                serialize_number_string(data.author_photo_id),
            ));
            append_rich_photo_metadata(
                &mut values,
                message.photos.get(&data.author_photo_id),
                "author_photo",
                "author_photo_skip_reason",
                "author_photo_file_size",
                "author_photo_width",
                "author_photo_height",
            );
            values.push(("author", serialize_json_string(&data.author)));
            values.push(("date", serialize_date(context, data.date)));
            values.push(("date_unixtime", serialize_date_raw(data.date)));
            let blocks = serialize_rich_array(context, &data.blocks, |ctx, b| {
                serialize_rich_block(ctx, message, b)
            });
            values.push(("blocks", blocks));
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::Collage | RichBlockKind::Slideshow => {
            let items = serialize_rich_array(context, &data.blocks, |ctx, b| {
                serialize_rich_block(ctx, message, b)
            });
            values.push(("items", items));
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::Channel => {
            values.push(("channel", serialize_rich_channel(context, &data.channel)));
        }
        RichBlockKind::Audio | RichBlockKind::File => {
            values.push(("document_id", serialize_number_string(data.document_id)));
            append_rich_document_metadata(
                &mut values,
                message.documents.get(&data.document_id),
                true,
            );
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::Math => {
            values.push(("formula", serialize_json_string(&data.formula)));
        }
        RichBlockKind::Table => {
            values.push(("title", serialize_rich_text(context, message, &data.text)));
            values.push(("bordered", serialize_bool(data.bordered)));
            values.push(("striped", serialize_bool(data.striped)));
            values.push(("compact", serialize_bool(data.compact)));
            let rows = serialize_rich_array(context, &data.table_rows, |ctx, row| {
                serialize_rich_table_row(ctx, message, row)
            });
            values.push(("rows", rows));
        }
        RichBlockKind::Details => {
            values.push(("title", serialize_rich_text(context, message, &data.text)));
            values.push(("open", serialize_bool(data.open)));
            let blocks = serialize_rich_array(context, &data.blocks, |ctx, b| {
                serialize_rich_block(ctx, message, b)
            });
            values.push(("blocks", blocks));
        }
        RichBlockKind::RelatedArticles => {
            values.push(("title", serialize_rich_text(context, message, &data.text)));
            let articles = serialize_rich_array(context, &data.related_articles, |ctx, a| {
                serialize_rich_related_article(ctx, message, a)
            });
            values.push(("articles", articles));
        }
        RichBlockKind::Map | RichBlockKind::InputMap => {
            values.push(("geo", serialize_rich_map_point(context, &data.map_point)));
            values.push(("zoom", data.zoom.to_string().into_bytes()));
            values.push(("width", data.map_width.to_string().into_bytes()));
            values.push(("height", data.map_height.to_string().into_bytes()));
            values.push((
                "caption",
                serialize_rich_caption(context, message, &data.caption),
            ));
        }
        RichBlockKind::ButtonRow => {
            values.push((
                "alignment",
                serialize_json_string(data.button_alignment.as_str()),
            ));
            let buttons = serialize_rich_array(context, &data.buttons, |ctx, btn| {
                let mut btn_values = Vec::new();
                ctx.nesting.push(ContainerKind::Object);
                append_rich_button_fields(&mut btn_values, ctx, message, btn);
                ctx.nesting.pop();
                serialize_object(ctx, &btn_values)
            });
            values.push(("buttons", buttons));
        }
        RichBlockKind::Unknown => {
            values.push(("source_type", serialize_json_string("unknown")));
            values.push(("unsupported", b"true".to_vec()));
        }
    }
    context.nesting.pop();
    serialize_object(context, &values)
}

fn serialize_rich_message(context: &mut JsonContext, data: &RichMessage) -> Vec<u8> {
    let mut values: Vec<(&str, Vec<u8>)> = vec![
        ("rtl", serialize_bool(data.rtl)),
        ("part", serialize_bool(data.part)),
    ];
    context.nesting.push(ContainerKind::Object);
    let blocks = serialize_rich_array(context, &data.blocks, |ctx, block| {
        serialize_rich_block(ctx, data, block)
    });
    values.push(("blocks", blocks));
    context.nesting.pop();
    serialize_object(context, &values)
}

fn wrap_peer_id(peer_id: PeerId) -> Vec<u8> {
    let chat = peer_id.to_chat_or_zero();
    if chat.0 != 0 {
        return serialize_json_string(&format!("chat{}", chat.0));
    }
    let channel = peer_id.to_channel_or_zero();
    if channel.0 != 0 {
        return serialize_json_string(&format!("channel{}", channel.0));
    }
    serialize_json_string(&format!("user{}", peer_id.to_user_or_zero().0))
}

fn format_file_path_or_skip(file: &File, name: &str) -> String {
    let pre = if name.is_empty() {
        String::new()
    } else {
        format!("{name} ")
    };
    match file.skip_reason {
        SkipReason::Unavailable => format!("{pre}(File unavailable, please try again later)"),
        SkipReason::FileSize => {
            format!("{pre}(File exceeds maximum size. Change data exporting settings to download.)")
        }
        SkipReason::FileType => {
            format!("{pre}(File not included. Change data exporting settings to download.)")
        }
        SkipReason::DateLimits => format!("{pre}(File unavailable, please try again later)"),
        SkipReason::None => file.relative_path.clone(),
    }
}

fn serialize_message(
    context: &mut JsonContext,
    message: &Message,
    peers: &BTreeMap<PeerId, Peer>,
    internal_links_domain: &str,
) -> Vec<u8> {
    if matches!(message.media.content, MediaContent::Unsupported(_))
        && message.rich_message.is_none()
    {
        return serialize_object(
            context,
            &[
                ("id", message.id.to_string().into_bytes()),
                ("type", serialize_json_string("unsupported")),
            ],
        );
    }

    let empty_peer = Peer::default();
    let empty_user = User::default();
    let get_peer = |peer_id: PeerId| -> &Peer { peers.get(&peer_id).unwrap_or(&empty_peer) };
    let get_user = |user_id: UserId| -> &User {
        get_peer(PeerId::from_user(user_id))
            .as_user()
            .unwrap_or(&empty_user)
    };

    let msg_type = if !message.action.is_none() {
        "service"
    } else {
        "message"
    };
    let mut values: Vec<(String, Vec<u8>)> = vec![
        ("id".to_string(), message.id.to_string().into_bytes()),
        ("type".to_string(), serialize_json_string(msg_type)),
        ("date".to_string(), serialize_date(context, message.date)),
        (
            "date_unixtime".to_string(),
            serialize_date_raw(message.date),
        ),
    ];
    context.nesting.push(ContainerKind::Object);

    let push_bare = |values: &mut Vec<(String, Vec<u8>)>, key: &str, value: Vec<u8>| {
        if !value.is_empty() {
            values.push((key.to_string(), value));
        }
    };
    let push_str = |values: &mut Vec<(String, Vec<u8>)>, key: &str, value: &str| {
        if !value.is_empty() {
            values.push((key.to_string(), serialize_json_string(value)));
        }
    };
    let push_bytes_as_str = |values: &mut Vec<(String, Vec<u8>)>, key: &str, value: &[u8]| {
        if !value.is_empty() {
            values.push((key.to_string(), serialize_json_bytes(value)));
        }
    };
    let push_bool = |values: &mut Vec<(String, Vec<u8>)>, key: &str, value: bool| {
        values.push((key.to_string(), serialize_bool(value)));
    };
    let push_num = |values: &mut Vec<(String, Vec<u8>)>, key: &str, value: String| {
        values.push((key.to_string(), value.into_bytes()));
    };
    let push_peer_id = |values: &mut Vec<(String, Vec<u8>)>, key: &str, peer_id: PeerId| {
        values.push((key.to_string(), wrap_peer_id(peer_id)));
    };

    if message.edited != 0 {
        push_bare(
            &mut values,
            "edited",
            serialize_date(context, message.edited),
        );
        push_bare(
            &mut values,
            "edited_unixtime",
            serialize_date_raw(message.edited),
        );
    }

    let wrap_peer_name = |peer_id: PeerId| string_allow_null(&get_peer(peer_id).name());
    let wrap_user_name = |user_id: UserId| string_allow_null(&get_user(user_id).name());

    let push_from = |values: &mut Vec<(String, Vec<u8>)>, label: &str| {
        if !message.from_id.is_zero() {
            push_bare(values, label, wrap_peer_name(message.from_id));
            push_peer_id(values, &format!("{label}_id"), message.from_id);
        }
    };
    let push_reply_to_msg_id = |values: &mut Vec<(String, Vec<u8>)>, label: &str| {
        if message.reply_to_msg_id != 0 {
            push_num(values, label, message.reply_to_msg_id.to_string());
            if !message.reply_to_peer_id.is_zero() {
                push_peer_id(values, "reply_to_peer_id", message.reply_to_peer_id);
            }
        }
    };
    let push_user_names =
        |values: &mut Vec<(String, Vec<u8>)>, ctx: &JsonContext, data: &[UserId], label: &str| {
            let list: Vec<Vec<u8>> = data.iter().map(|&uid| wrap_user_name(uid)).collect();
            push_bare(values, label, serialize_array(ctx, &list));
        };
    let push_path = |values: &mut Vec<(String, Vec<u8>)>, file: &File, label: &str| {
        let s = format_file_path_or_skip(file, "");
        push_str(values, label, &s);
    };
    let push_photo = |values: &mut Vec<(String, Vec<u8>)>, image: &crate::data::Image| {
        push_path(values, &image.file, "photo");
        push_num(values, "photo_file_size", image.file.size.to_string());
        if image.width != 0 && image.height != 0 {
            push_num(values, "width", image.width.to_string());
            push_num(values, "height", image.height.to_string());
        }
    };
    let push_ttl = |values: &mut Vec<(String, Vec<u8>)>, label: &str| {
        if message.media.ttl != 0 {
            push_num(values, label, message.media.ttl.to_string());
        }
    };

    match &message.action {
        ServiceAction::None => {}
        ServiceAction::ChatCreate { title, user_ids } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "create_group");
            push_str(&mut values, "title", title);
            push_user_names(&mut values, context, user_ids, "members");
        }
        ServiceAction::ChatEditTitle { title } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "edit_group_title");
            push_str(&mut values, "title", title);
        }
        ServiceAction::ChatEditPhoto { photo } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "edit_group_photo");
            push_photo(&mut values, &photo.image);
            if photo.spoilered {
                push_bool(&mut values, "media_spoiler", true);
            }
        }
        ServiceAction::ChatDeletePhoto => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "delete_group_photo");
        }
        ServiceAction::ChatAddUser { user_ids } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "invite_members");
            push_user_names(&mut values, context, user_ids, "members");
        }
        ServiceAction::ChatDeleteUser { user_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "remove_members");
            push_user_names(&mut values, context, &[*user_id], "members");
        }
        ServiceAction::ChatJoinedByLink { inviter_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "join_group_by_link");
            push_bare(&mut values, "inviter", wrap_user_name(*inviter_id));
        }
        ServiceAction::ChannelCreate { title } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "create_channel");
            push_str(&mut values, "title", title);
        }
        ServiceAction::ChatMigrateTo { .. } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "migrate_to_supergroup");
        }
        ServiceAction::ChannelMigrateFrom { title, .. } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "migrate_from_group");
            push_str(&mut values, "title", title);
        }
        ServiceAction::PinMessage => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "pin_message");
            push_reply_to_msg_id(&mut values, "message_id");
        }
        ServiceAction::HistoryClear => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "clear_history");
        }
        ServiceAction::GameScore { score, .. } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "score_in_game");
            push_reply_to_msg_id(&mut values, "game_message_id");
            push_num(&mut values, "score", score.to_string());
        }
        ServiceAction::PaymentSent {
            currency,
            amount,
            recurring_init,
            recurring_used,
        } => {
            push_str(&mut values, "action", "send_payment");
            push_num(&mut values, "amount", amount.to_string());
            push_str(&mut values, "currency", currency);
            push_reply_to_msg_id(&mut values, "invoice_message_id");
            if *recurring_used {
                push_str(&mut values, "recurring", "used");
            } else if *recurring_init {
                push_str(&mut values, "recurring", "init");
            }
        }
        ServiceAction::PhoneCall {
            conference_id,
            state,
            duration,
        } => {
            push_from(&mut values, "actor");
            push_str(
                &mut values,
                "action",
                if *conference_id != 0 {
                    "conference_call"
                } else {
                    "phone_call"
                },
            );
            if *duration != 0 {
                push_num(&mut values, "duration_seconds", duration.to_string());
            }
            if *conference_id != 0 {
                push_bool(&mut values, "is_active", *state == PhoneCallState::Active);
                push_bool(&mut values, "is_missed", *state == PhoneCallState::Missed);
            } else {
                let reason = match state {
                    PhoneCallState::Busy => "busy",
                    PhoneCallState::Disconnect => "disconnect",
                    PhoneCallState::Hangup => "hangup",
                    PhoneCallState::Missed => "missed",
                    // Exact match to tdesktop `export_output_json.cpp:1773`.
                    PhoneCallState::MigrateConferenceCall => "migrate_conference_all",
                    _ => "",
                };
                push_str(&mut values, "discard_reason", reason);
            }
        }
        ServiceAction::ScreenshotTaken => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "take_screenshot");
        }
        ServiceAction::CustomAction { message: text } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "information_text", text);
        }
        ServiceAction::BotAllowed {
            app_id,
            app,
            domain,
            attach_menu,
            from_request,
        } => {
            if *attach_menu {
                push_str(&mut values, "action", "attach_menu_bot_allowed");
            } else if *from_request {
                push_str(&mut values, "action", "web_app_bot_allowed");
            } else if *app_id != 0 {
                push_str(&mut values, "action", "allow_sending_messages");
                push_num(&mut values, "reason_app_id", app_id.to_string());
                push_str(&mut values, "reason_app_name", app);
            } else {
                push_str(&mut values, "action", "allow_sending_messages");
                push_str(&mut values, "reason_domain", domain);
            }
        }
        ServiceAction::SecureValuesSent { types } => {
            push_str(&mut values, "action", "send_passport_values");
            let list: Vec<Vec<u8>> = types
                .iter()
                .map(|t| {
                    let s = match t {
                        SecureValueType::PersonalDetails => "personal_details",
                        SecureValueType::Passport => "passport",
                        SecureValueType::DriverLicense => "driver_license",
                        SecureValueType::IdentityCard => "identity_card",
                        SecureValueType::InternalPassport => "internal_passport",
                        SecureValueType::Address => "address_information",
                        SecureValueType::UtilityBill => "utility_bill",
                        SecureValueType::BankStatement => "bank_statement",
                        SecureValueType::RentalAgreement => "rental_agreement",
                        SecureValueType::PassportRegistration => "passport_registration",
                        SecureValueType::TemporaryRegistration => "temporary_registration",
                        SecureValueType::Phone => "phone_number",
                        SecureValueType::Email => "email",
                    };
                    serialize_json_string(s)
                })
                .collect();
            push_bare(&mut values, "values", serialize_array(context, &list));
        }
        ServiceAction::ContactSignUp => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "joined_telegram");
        }
        ServiceAction::GeoProximityReached {
            from_id,
            to_id,
            distance,
            ..
        } => {
            push_str(&mut values, "action", "proximity_reached");
            if !from_id.is_zero() {
                push_bare(&mut values, "from", wrap_peer_name(*from_id));
                push_peer_id(&mut values, "from_id", *from_id);
            }
            if !to_id.is_zero() {
                push_bare(&mut values, "to", wrap_peer_name(*to_id));
                push_peer_id(&mut values, "to_id", *to_id);
            }
            push_num(&mut values, "distance", distance.to_string());
        }
        ServiceAction::PhoneNumberRequest => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "requested_phone_number");
        }
        ServiceAction::GroupCall { duration } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "group_call");
            if *duration != 0 {
                push_num(&mut values, "duration", duration.to_string());
            }
        }
        ServiceAction::InviteToGroupCall { user_ids } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "invite_to_group_call");
            push_user_names(&mut values, context, user_ids, "members");
        }
        ServiceAction::SetMessagesTtl { period } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "set_messages_ttl");
            push_num(&mut values, "period", period.to_string());
        }
        ServiceAction::GroupCallScheduled { date } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "group_call_scheduled");
            push_num(&mut values, "schedule_date", date.to_string());
        }
        ServiceAction::SetChatTheme { emoji } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "edit_chat_theme");
            if !emoji.is_empty() {
                push_str(&mut values, "emoticon", emoji);
            }
        }
        ServiceAction::ChatJoinedByRequest => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "join_group_by_request");
        }
        ServiceAction::ChatJoinedViaCommunity { community_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "join_group_via_community");
            push_num(&mut values, "community_id", community_id.0.to_string());
        }
        ServiceAction::WebViewDataSent { text } => {
            push_str(&mut values, "action", "send_webview_data");
            push_str(&mut values, "button_text", text);
        }
        ServiceAction::GiftPremium { cost, days } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "send_premium_gift");
            if !cost.is_empty() {
                push_str(&mut values, "cost", cost);
            }
            if *days != 0 {
                push_num(&mut values, "days", days.to_string());
            }
        }
        ServiceAction::TopicCreate { title } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "topic_created");
            push_str(&mut values, "title", title);
        }
        ServiceAction::TopicEdit {
            title,
            icon_emoji_id,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "topic_edit");
            if !title.is_empty() {
                push_str(&mut values, "new_title", title);
            }
            if let Some(icon_id) = icon_emoji_id {
                push_num(&mut values, "new_icon_emoji_id", icon_id.to_string());
            }
        }
        ServiceAction::SuggestProfilePhoto { photo } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "suggest_profile_photo");
            push_photo(&mut values, &photo.image);
            if photo.spoilered {
                push_bool(&mut values, "media_spoiler", true);
            }
        }
        ServiceAction::RequestedPeer {
            peers: peer_list,
            button_id,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "requested_peer");
            push_num(&mut values, "button_id", button_id.to_string());
            let items: Vec<Vec<u8>> = peer_list
                .iter()
                .map(|p| p.0.to_string().into_bytes())
                .collect();
            // Matches C++ `push("peers", SerializeArray(context, values))` (serialized as JSON string).
            let serialized = serialize_array(context, &items);
            push_bytes_as_str(&mut values, "peers", &serialized);
        }
        ServiceAction::GiftCode {
            code,
            boost_peer_id,
            days,
            via_giveaway,
            unclaimed,
        } => {
            push_str(&mut values, "action", "gift_code_prize");
            push_str(&mut values, "gift_code", code);
            if !boost_peer_id.is_zero() {
                push_peer_id(&mut values, "boost_peer_id", *boost_peer_id);
            }
            push_num(&mut values, "days", days.to_string());
            push_bool(&mut values, "unclaimed", *unclaimed);
            push_bool(&mut values, "via_giveaway", *via_giveaway);
        }
        ServiceAction::GiveawayLaunch => {
            push_str(&mut values, "action", "giveaway_launch");
        }
        ServiceAction::GiveawayResults {
            winners,
            unclaimed,
            credits,
        } => {
            push_str(&mut values, "action", "giveaway_results");
            push_num(&mut values, "winners", winners.to_string());
            push_num(&mut values, "unclaimed", unclaimed.to_string());
            push_bool(&mut values, "stars", *credits);
        }
        ServiceAction::SetChatWallPaper { same, .. } => {
            push_from(&mut values, "actor");
            push_str(
                &mut values,
                "action",
                if *same {
                    "set_same_chat_wallpaper"
                } else {
                    "set_chat_wallpaper"
                },
            );
            push_reply_to_msg_id(&mut values, "message_id");
        }
        ServiceAction::BoostApply { boosts } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "boost_apply");
            push_num(&mut values, "boosts", boosts.to_string());
        }
        ServiceAction::PaymentRefunded {
            peer_id,
            currency,
            amount,
            transaction_id,
        } => {
            push_str(&mut values, "action", "refunded_payment");
            push_num(&mut values, "amount", amount.to_string());
            push_str(&mut values, "currency", currency);
            push_bare(&mut values, "peer_name", wrap_peer_name(*peer_id));
            push_peer_id(&mut values, "peer_id", *peer_id);
            push_str(&mut values, "charge_id", transaction_id);
        }
        ServiceAction::GiftCredits { cost, amount } => {
            push_from(&mut values, "actor");
            push_str(
                &mut values,
                "action",
                if credits_is_ton(*amount) {
                    "send_ton_gift"
                } else {
                    "send_stars_gift"
                },
            );
            if !cost.is_empty() {
                push_str(&mut values, "cost", cost);
            }
            if !credits_is_empty(*amount) {
                push_num(&mut values, "amount_whole", amount.whole().to_string());
                push_num(&mut values, "amount_nano", amount.nano().to_string());
            }
        }
        ServiceAction::PrizeStars {
            peer_id,
            amount,
            transaction_id,
            giveaway_msg_id,
            is_unclaimed,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "stars_prize");
            push_peer_id(&mut values, "boost_peer_id", *peer_id);
            push_bare(&mut values, "boost_peer_name", wrap_peer_name(*peer_id));
            push_num(&mut values, "stars", amount.to_string());
            push_bool(&mut values, "is_unclaimed", *is_unclaimed);
            push_num(&mut values, "giveaway_msg_id", giveaway_msg_id.to_string());
            push_str(&mut values, "transaction_id", transaction_id);
        }
        ServiceAction::StarGift {
            gift_id,
            stars,
            text,
            anonymous,
            limited,
            ..
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "send_star_gift");
            push_num(&mut values, "gift_id", gift_id.to_string());
            push_num(&mut values, "stars", stars.to_string());
            push_bool(&mut values, "is_limited", *limited);
            push_bool(&mut values, "is_anonymous", *anonymous);
            let serialized_text = serialize_text(context, text, false);
            push_bare(&mut values, "gift_text", serialized_text);
        }
        ServiceAction::PaidMessagesRefunded { messages, stars } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "paid_messages_refund");
            push_num(&mut values, "messages_count", messages.to_string());
            push_num(&mut values, "stars_count", stars.to_string());
        }
        ServiceAction::PaidMessagesPrice {
            stars,
            broadcast_allowed,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "paid_messages_price_change");
            push_num(&mut values, "price_stars", stars.to_string());
            push_bool(
                &mut values,
                "is_broadcast_messages_allowed",
                *broadcast_allowed,
            );
        }
        ServiceAction::TodoCompletions {
            completed,
            incompleted,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "todo_completions");
            let comp: Vec<String> = completed.iter().map(ToString::to_string).collect();
            let incomp: Vec<String> = incompleted.iter().map(ToString::to_string).collect();
            push_bare(
                &mut values,
                "completed",
                format!("[{}]", comp.join(",")).into_bytes(),
            );
            push_bare(
                &mut values,
                "incompleted",
                format!("[{}]", incomp.join(",")).into_bytes(),
            );
        }
        ServiceAction::TodoAppendTasks { items } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "todo_append_tasks");
            let mut serialized_items = Vec::with_capacity(items.len());
            for item in items {
                context.nesting.push(ContainerKind::Array);
                let txt = serialize_text(context, &item.text, false);
                let obj = serialize_object(
                    context,
                    &[("text", txt), ("id", item.id.to_string().into_bytes())],
                );
                context.nesting.pop();
                serialized_items.push(obj);
            }
            push_bare(
                &mut values,
                "items",
                serialize_array(context, &serialized_items),
            );
        }
        ServiceAction::PollAppendAnswer { option } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "poll_append_answer");
            push_str(&mut values, "option", option);
        }
        ServiceAction::PollDeleteAnswer { option } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "poll_delete_answer");
            push_str(&mut values, "option", option);
        }
        ServiceAction::SuggestedPostApproval {
            reject_comment,
            schedule_date,
            price,
            rejected,
            ..
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "process_suggested_post");
            if *rejected {
                push_bare(&mut values, "rejected", b"true".to_vec());
                if !reject_comment.is_empty() {
                    push_str(&mut values, "comment", reject_comment);
                }
            } else {
                // Matches C++ `push("price_amount_whole", NumberToString(...))` (string-quoted).
                push_str(
                    &mut values,
                    "price_amount_whole",
                    &price.whole().to_string(),
                );
                push_str(&mut values, "price_amount_nano", &price.nano().to_string());
                push_str(
                    &mut values,
                    "price_currency",
                    if credits_is_ton(*price) {
                        "TON"
                    } else {
                        "Stars"
                    },
                );
                push_num(&mut values, "scheduled_date", schedule_date.to_string());
            }
        }
        ServiceAction::SuggestedPostSuccess { price } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "suggested_post_success");
            push_str(
                &mut values,
                "price_amount_whole",
                &price.whole().to_string(),
            );
            push_str(&mut values, "price_amount_nano", &price.nano().to_string());
            push_str(
                &mut values,
                "price_currency",
                if credits_is_ton(*price) {
                    "TON"
                } else {
                    "Stars"
                },
            );
        }
        ServiceAction::SuggestedPostRefund { payer_initiated } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "suggested_post_refund");
            push_bool(&mut values, "user_initiated", *payer_initiated);
        }
        ServiceAction::SuggestBirthday { birthday } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "suggest_birthday");
            push_num(&mut values, "day", birthday.day().to_string());
            push_num(&mut values, "month", birthday.month().to_string());
            if birthday.year() != 0 {
                push_num(&mut values, "year", birthday.year().to_string());
            }
        }
        ServiceAction::NoForwardsToggle { new_value } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "no_forwards_toggle");
            push_bool(&mut values, "new_value", *new_value);
        }
        ServiceAction::NoForwardsRequest { expired, new_value } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "no_forwards_request");
            push_bool(&mut values, "expired", *expired);
            push_bool(&mut values, "new_value", *new_value);
        }
        ServiceAction::NewCreatorPending { new_creator_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "new_creator_pending");
            push_bare(&mut values, "new_creator", wrap_user_name(*new_creator_id));
        }
        ServiceAction::ChangeCreator { new_creator_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "change_creator");
            push_bare(&mut values, "new_creator", wrap_user_name(*new_creator_id));
        }
        ServiceAction::ManagedBotCreated { bot_id } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "managed_bot_created");
            push_bare(&mut values, "bot", wrap_user_name(*bot_id));
        }
        ServiceAction::GramTransfer {
            amount,
            peer_address,
            transaction_id,
            comment,
            comment_encrypted,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "gram_transfer");
            push_num(
                &mut values,
                "amount_whole",
                (amount / NANOS_IN_GRAM).to_string(),
            );
            push_num(
                &mut values,
                "amount_nano",
                (amount % NANOS_IN_GRAM).to_string(),
            );
            push_str(&mut values, "peer_address", peer_address);
            push_str(&mut values, "transaction_id", transaction_id);
            push_bool(&mut values, "comment_encrypted", *comment_encrypted);
            if *comment_encrypted {
                push_str(&mut values, "encrypted_comment_base64", comment);
            } else {
                push_str(&mut values, "comment", comment);
            }
        }
        ServiceAction::WalletTonConnectRequest {
            session_id,
            expires,
            topic,
            trace_id,
            accepted,
            declined,
        } => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "ton_connect_request");
            push_num(&mut values, "session_id", session_id.to_string());
            push_num(&mut values, "expires", expires.to_string());
            push_str(&mut values, "topic", topic);
            push_str(&mut values, "trace_id", trace_id);
            push_bool(&mut values, "accepted", *accepted);
            push_bool(&mut values, "declined", *declined);
        }
        ServiceAction::NoPaidMessagesTokenUsed => {
            push_from(&mut values, "actor");
            push_str(&mut values, "action", "no_paid_messages_token_used");
        }
    }

    if message.action.is_none() {
        push_from(&mut values, "from");
        push_str(&mut values, "author", &message.signature);
        if !message.forwarded_from_id.is_zero() {
            push_bare(
                &mut values,
                "forwarded_from",
                wrap_peer_name(message.forwarded_from_id),
            );
            push_peer_id(&mut values, "forwarded_from_id", message.forwarded_from_id);
        } else if !message.forwarded_from_name.is_empty() {
            push_bare(
                &mut values,
                "forwarded_from",
                string_allow_null(&message.forwarded_from_name),
            );
        }
        if !message.saved_from_chat_id.is_zero() {
            push_bare(
                &mut values,
                "saved_from",
                wrap_peer_name(message.saved_from_chat_id),
            );
        }
        push_reply_to_msg_id(&mut values, "reply_to_message_id");
        if message.via_bot_id.0 != 0 {
            let username = format_username(&get_user(message.via_bot_id).username);
            if !username.is_empty() {
                push_str(&mut values, "via_bot", &username);
            }
        }
    }

    match &message.media.content {
        MediaContent::None | MediaContent::Unsupported(_) => {}
        MediaContent::Photo(photo) => {
            push_photo(&mut values, &photo.image);
            if photo.spoilered {
                push_bool(&mut values, "media_spoiler", true);
            }
            push_ttl(&mut values, "self_destruct_period_seconds");
        }
        MediaContent::Document(data) => {
            push_path(&mut values, &data.file, "file");
            push_str(&mut values, "file_name", &data.name);
            push_num(&mut values, "file_size", data.file.size.to_string());
            if data.thumb.width > 0 {
                push_path(&mut values, &data.thumb.file, "thumbnail");
                push_num(
                    &mut values,
                    "thumbnail_file_size",
                    data.thumb.file.size.to_string(),
                );
            }
            if data.is_sticker {
                push_str(&mut values, "media_type", "sticker");
                push_str(&mut values, "sticker_emoji", &data.sticker_emoji);
            } else if data.is_video_message {
                push_str(&mut values, "media_type", "video_message");
            } else if data.is_voice_message {
                push_str(&mut values, "media_type", "voice_message");
            } else if data.is_animated {
                push_str(&mut values, "media_type", "animation");
            } else if data.is_video_file {
                push_str(&mut values, "media_type", "video_file");
            } else if data.is_audio_file {
                push_str(&mut values, "media_type", "audio_file");
                push_str(&mut values, "performer", &data.song_performer);
                push_str(&mut values, "title", &data.song_title);
            }
            push_str(&mut values, "mime_type", &data.mime);
            if data.duration != 0 {
                push_num(&mut values, "duration_seconds", data.duration.to_string());
            }
            if data.width != 0 && data.height != 0 {
                push_num(&mut values, "width", data.width.to_string());
                push_num(&mut values, "height", data.height.to_string());
            }
            if data.spoilered {
                push_bool(&mut values, "media_spoiler", true);
            }
            push_ttl(&mut values, "self_destruct_period_seconds");
        }
        MediaContent::SharedContact(data) => {
            let contact_obj = serialize_object(
                context,
                &[
                    ("first_name", serialize_json_string(&data.info.first_name)),
                    ("last_name", serialize_json_string(&data.info.last_name)),
                    (
                        "phone_number",
                        serialize_json_string(&format_phone_number(&data.info.phone_number)),
                    ),
                ],
            );
            push_bare(&mut values, "contact_information", contact_obj);
            if !data.vcard.content.is_empty() {
                push_path(&mut values, &data.vcard, "contact_vcard");
                push_num(
                    &mut values,
                    "contact_vcard_file_size",
                    data.vcard.size.to_string(),
                );
            }
        }
        MediaContent::GeoPoint(data) => {
            let loc = if data.valid {
                serialize_object(
                    context,
                    &[
                        ("latitude", float_to_string(data.latitude).into_bytes()),
                        ("longitude", float_to_string(data.longitude).into_bytes()),
                    ],
                )
            } else {
                b"null".to_vec()
            };
            push_bare(&mut values, "location_information", loc);
            push_ttl(&mut values, "live_location_period_seconds");
        }
        MediaContent::Venue(data) => {
            push_str(&mut values, "place_name", &data.title);
            push_str(&mut values, "address", &data.address);
            if data.point.valid {
                let loc = serialize_object(
                    context,
                    &[
                        (
                            "latitude",
                            float_to_string(data.point.latitude).into_bytes(),
                        ),
                        (
                            "longitude",
                            float_to_string(data.point.longitude).into_bytes(),
                        ),
                    ],
                );
                push_bare(&mut values, "location_information", loc);
            }
        }
        MediaContent::Game(data) => {
            push_str(&mut values, "game_title", &data.title);
            push_str(&mut values, "game_description", &data.description);
            if data.bot_id.0 != 0 && !data.short_name.is_empty() {
                let bot = get_user(data.bot_id);
                if bot.is_bot && !bot.username.is_empty() {
                    let link = format!(
                        "{internal_links_domain}{}?game={}",
                        bot.username, data.short_name
                    );
                    push_str(&mut values, "game_link", &link);
                }
            }
        }
        MediaContent::Invoice(data) => {
            // Matches C++ `push("invoice_information", SerializeObject(...))` (string-quoted JSON object).
            let receipt = if data.receipt_msg_id != 0 {
                data.receipt_msg_id.to_string().into_bytes()
            } else {
                Vec::new()
            };
            let inv = serialize_object(
                context,
                &[
                    ("title", serialize_json_string(&data.title)),
                    ("description", serialize_json_string(&data.description)),
                    ("amount", data.amount.to_string().into_bytes()),
                    ("currency", serialize_json_string(&data.currency)),
                    ("receipt_message_id", receipt),
                ],
            );
            push_bytes_as_str(&mut values, "invoice_information", &inv);
        }
        MediaContent::Poll(data) => {
            context.nesting.push(ContainerKind::Object);
            let mut answers = Vec::with_capacity(data.answers.len());
            for answer in &data.answers {
                context.nesting.push(ContainerKind::Array);
                let txt = serialize_text(context, &answer.text, false);
                let obj = serialize_object(
                    context,
                    &[
                        ("text", txt),
                        ("voters", answer.votes.to_string().into_bytes()),
                        ("chosen", serialize_bool(answer.my)),
                    ],
                );
                context.nesting.pop();
                answers.push(obj);
            }
            let serialized_answers = serialize_array(context, &answers);
            context.nesting.pop();

            let q = serialize_text(context, &data.question, false);
            let poll_obj = serialize_object(
                context,
                &[
                    ("question", q),
                    ("closed", serialize_bool(data.closed)),
                    ("total_voters", data.total_votes.to_string().into_bytes()),
                    ("answers", serialized_answers),
                ],
            );
            push_bare(&mut values, "poll", poll_obj);
        }
        MediaContent::TodoList(data) => {
            context.nesting.push(ContainerKind::Object);
            let mut items = Vec::with_capacity(data.items.len());
            for item in &data.items {
                context.nesting.push(ContainerKind::Array);
                let txt = serialize_text(context, &item.text, false);
                let obj = serialize_object(
                    context,
                    &[("text", txt), ("id", item.id.to_string().into_bytes())],
                );
                context.nesting.pop();
                items.push(obj);
            }
            let serialized_items = serialize_array(context, &items);
            context.nesting.pop();

            let title = serialize_text(context, &data.title, false);
            let todo_obj = serialize_object(
                context,
                &[
                    ("title", title),
                    ("others_can_append", serialize_bool(data.others_can_append)),
                    (
                        "others_can_complete",
                        serialize_bool(data.others_can_complete),
                    ),
                    ("answers", serialized_items),
                ],
            );
            push_bare(&mut values, "todo_list", todo_obj);
        }
        MediaContent::GiveawayStart(data) => {
            context.nesting.push(ContainerKind::Array);
            let ch_items: Vec<Vec<u8>> = data
                .channels
                .iter()
                .map(|c| c.0.to_string().into_bytes())
                .collect();
            let serialized_channels = serialize_array(context, &ch_items);
            context.nesting.pop();

            context.nesting.push(ContainerKind::Array);
            let country_items: Vec<Vec<u8>> = data
                .countries
                .iter()
                .map(|code| serialize_json_string(code))
                .collect();
            let serialized_countries = serialize_array(context, &country_items);
            context.nesting.pop();

            let until_date = serialize_date(context, data.until_date);
            let gw_obj = serialize_object(
                context,
                &[
                    ("quantity", data.quantity.to_string().into_bytes()),
                    ("months", data.months.to_string().into_bytes()),
                    ("until_date", until_date),
                    ("channels", serialized_channels),
                    ("countries", serialized_countries),
                    (
                        "additional_prize",
                        serialize_json_string(&data.additional_prize),
                    ),
                    ("stars", data.credits.to_string().into_bytes()),
                    ("is_only_new_subscribers", serialize_bool(!data.all)),
                ],
            );
            push_bare(&mut values, "giveaway_information", gw_obj);
        }
        MediaContent::GiveawayResults(data) => {
            context.nesting.push(ContainerKind::Array);
            let winner_items: Vec<Vec<u8>> = data
                .winners
                .iter()
                .map(|id| id.0.to_string().into_bytes())
                .collect();
            let serialized_winners = serialize_array(context, &winner_items);
            context.nesting.pop();

            let until_date = serialize_date(context, data.until_date);
            let res_obj = serialize_object(
                context,
                &[
                    ("channel", data.channel.0.to_string().into_bytes()),
                    ("winners", serialized_winners),
                    (
                        "additional_prize",
                        serialize_json_string(&data.additional_prize),
                    ),
                    ("until_date", until_date),
                    ("launch_message_id", data.launch_id.to_string().into_bytes()),
                    (
                        "additional_peers_count",
                        data.additional_peers_count.to_string().into_bytes(),
                    ),
                    ("winners_count", data.winners_count.to_string().into_bytes()),
                    (
                        "unclaimed_count",
                        data.unclaimed_count.to_string().into_bytes(),
                    ),
                    ("months", data.months.to_string().into_bytes()),
                    ("stars", data.credits.to_string().into_bytes()),
                    ("is_refunded", serialize_bool(data.refunded)),
                    ("is_only_new_subscribers", serialize_bool(!data.all)),
                ],
            );
            push_bare(&mut values, "giveaway_results", res_obj);
        }
        MediaContent::PaidMedia(data) => {
            push_num(&mut values, "paid_stars_amount", data.stars.to_string());
        }
    }

    if let Some(ref rich) = message.rich_message {
        let serialized_rich = serialize_rich_message(context, rich);
        push_bare(&mut values, "rich_message", serialized_rich);
    } else {
        let txt = serialize_text(context, &message.text, false);
        let entities = serialize_text(context, &message.text, true);
        push_bare(&mut values, "text", txt);
        push_bare(&mut values, "text_entities", entities);
    }

    if !message.inline_button_rows.is_empty() {
        context.nesting.push(ContainerKind::Array);
        let mut rows = Vec::with_capacity(message.inline_button_rows.len());
        for row in &message.inline_button_rows {
            context.nesting.push(ContainerKind::Array);
            let mut buttons = Vec::with_capacity(row.len());
            for entry in row {
                let mut pairs: Vec<(&str, Vec<u8>)> =
                    vec![("type", serialize_json_string(entry.type_str()))];
                if !entry.text.is_empty() {
                    pairs.push(("text", serialize_json_string(&entry.text)));
                }
                if !entry.data.is_empty() {
                    let is_callback = matches!(
                        entry.kind,
                        crate::data::MarkupButtonType::Callback
                            | crate::data::MarkupButtonType::CallbackWithPassword
                    );
                    if is_callback {
                        pairs.push((
                            "dataBase64",
                            serialize_json_string(&base64_url_no_pad(&entry.data)),
                        ));
                        pairs.push(("data", serialize_json_string("")));
                    } else {
                        pairs.push(("data", serialize_json_bytes(&entry.data)));
                    }
                }
                if !entry.forward_text.is_empty() {
                    pairs.push(("forward_text", serialize_json_string(&entry.forward_text)));
                }
                if entry.button_id != 0 {
                    pairs.push(("button_id", entry.button_id.to_string().into_bytes()));
                }
                buttons.push(serialize_object(context, &pairs));
            }
            context.nesting.pop();
            rows.push(serialize_array(context, &buttons));
        }
        context.nesting.pop();
        push_bare(
            &mut values,
            "inline_bot_buttons",
            serialize_array(context, &rows),
        );
    }

    if !message.reactions.is_empty() {
        context.nesting.push(ContainerKind::Array);
        let mut reactions = Vec::with_capacity(message.reactions.len());
        for reaction in &message.reactions {
            reactions.push(serialize_reaction(context, reaction, peers));
        }
        push_bare(
            &mut values,
            "reactions",
            serialize_array(context, &reactions),
        );
        context.nesting.pop();
    }

    context.nesting.pop();
    let pairs: Vec<(&str, Vec<u8>)> = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    serialize_object(context, &pairs)
}

fn serialize_reaction(
    context: &mut JsonContext,
    reaction: &Reaction,
    peers: &BTreeMap<PeerId, Peer>,
) -> Vec<u8> {
    let empty_peer = Peer::default();
    context.nesting.push(ContainerKind::Object);
    let mut pairs: Vec<(&str, Vec<u8>)> = vec![
        ("type", serialize_json_string(reaction.kind.as_str())),
        ("count", reaction.count.to_string().into_bytes()),
    ];
    match reaction.kind {
        ReactionType::Emoji => {
            pairs.push(("emoji", serialize_json_string(&reaction.emoji)));
        }
        ReactionType::CustomEmoji => {
            pairs.push(("document_id", serialize_json_string(&reaction.document_id)));
        }
        ReactionType::Empty | ReactionType::Paid => {}
    }
    if !reaction.recent.is_empty() {
        context.nesting.push(ContainerKind::Array);
        let mut recents = Vec::with_capacity(reaction.recent.len());
        for recent in &reaction.recent {
            context.nesting.push(ContainerKind::Array);
            let peer_name = peers.get(&recent.peer_id).unwrap_or(&empty_peer).name();
            let date_str = serialize_date(context, recent.date);
            let obj = serialize_object(
                context,
                &[
                    ("from", string_allow_null(&peer_name)),
                    ("from_id", wrap_peer_id(recent.peer_id)),
                    ("date", date_str),
                ],
            );
            context.nesting.pop();
            recents.push(obj);
        }
        pairs.push(("recent", serialize_array(context, &recents)));
        context.nesting.pop();
    }
    let result = serialize_object(context, &pairs);
    context.nesting.pop();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogsMode {
    None,
    Chats,
    Left,
}

/// JSON export writer producing `result.json` (port of `Export::Output::JsonWriter`).
pub struct JsonWriter {
    settings: Settings,
    environment: Environment,
    context: JsonContext,
    current_nesting_had_item: bool,
    dialogs_mode: DialogsMode,
    output: Option<OutputFile>,
}

impl Default for JsonWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonWriter {
    /// Creates a new `JsonWriter`.
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            environment: Environment::default(),
            context: JsonContext::default(),
            current_nesting_had_item: false,
            dialogs_mode: DialogsMode::None,
            output: None,
        }
    }

    fn main_file_relative_path(&self) -> &'static str {
        "result.json"
    }

    fn path_with_relative_path(&self, path: &str) -> String {
        format!("{}{path}", self.settings.path)
    }

    fn push_nesting(&mut self, kind: ContainerKind) -> Vec<u8> {
        self.context.nesting.push(kind);
        self.current_nesting_had_item = false;
        match kind {
            ContainerKind::Object => b"{".to_vec(),
            ContainerKind::Array => b"[".to_vec(),
        }
    }

    fn prepare_object_item_start(&mut self, key: &str) -> Vec<u8> {
        let prefix = if self.current_nesting_had_item {
            ",\n"
        } else {
            "\n"
        };
        self.current_nesting_had_item = true;
        let mut out = Vec::new();
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(self.context.indentation().as_bytes());
        out.extend_from_slice(&serialize_json_string(key));
        out.extend_from_slice(b": ");
        out
    }

    fn prepare_array_item_start(&mut self) -> Vec<u8> {
        let prefix = if self.current_nesting_had_item {
            ",\n"
        } else {
            "\n"
        };
        self.current_nesting_had_item = true;
        let mut out = Vec::new();
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(self.context.indentation().as_bytes());
        out
    }

    fn pop_nesting(&mut self) -> Vec<u8> {
        let kind = self.context.nesting.pop().unwrap_or(ContainerKind::Object);
        self.current_nesting_had_item = true;
        let mut out = Vec::new();
        out.push(b'\n');
        out.extend_from_slice(self.context.indentation().as_bytes());
        out.push(match kind {
            ContainerKind::Object => b'}',
            ContainerKind::Array => b']',
        });
        out
    }

    fn write_block(&mut self, block: &[u8], stats: Option<&Stats>) -> OutputResult {
        match self.output.as_mut() {
            Some(file) => file.write_block(block, stats),
            None => OutputResult::fatal_error(self.main_file_path()),
        }
    }

    fn write_saved_contacts(&mut self, data: &ContactsList, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.prepare_object_item_start("contacts");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(&self.environment.about_contacts));
        block.extend_from_slice(&self.prepare_object_item_start("list"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        for index in sorted_contacts_indices(data) {
            let contact = &data.list[index];
            block.extend_from_slice(&self.prepare_array_item_start());
            let date_iso = serialize_date(&self.context, contact.date);
            let date_raw = serialize_date_raw(contact.date);
            let obj = if contact.first_name.is_empty()
                && contact.last_name.is_empty()
                && contact.phone_number.is_empty()
            {
                serialize_object(
                    &mut self.context,
                    &[("date", date_iso), ("date_unixtime", date_raw)],
                )
            } else {
                let uid = if contact.user_id.0 != 0 {
                    contact.user_id.0.to_string().into_bytes()
                } else {
                    Vec::new()
                };
                serialize_object(
                    &mut self.context,
                    &[
                        ("user_id", uid),
                        ("first_name", serialize_json_string(&contact.first_name)),
                        ("last_name", serialize_json_string(&contact.last_name)),
                        (
                            "phone_number",
                            serialize_json_string(&format_phone_number(&contact.phone_number)),
                        ),
                        ("date", date_iso),
                        ("date_unixtime", date_raw),
                    ],
                )
            };
            block.extend_from_slice(&obj);
        }
        block.extend_from_slice(&self.pop_nesting());
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn write_frequent_contacts(
        &mut self,
        data: &ContactsList,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mut block = self.prepare_object_item_start("frequent_contacts");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(&self.environment.about_frequent));
        block.extend_from_slice(&self.prepare_object_item_start("list"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));

        let lists: [(&[TopPeer], &str); 3] = [
            (&data.correspondents, "people"),
            (&data.inline_bots, "inline_bots"),
            (&data.phone_calls, "calls"),
        ];
        for (peers, category) in lists {
            for top in peers {
                let kind_str = if let Some(chat) = top.peer.as_chat() {
                    if chat.username.is_empty() {
                        if chat.is_broadcast {
                            "private_channel"
                        } else if chat.is_supergroup {
                            "private_supergroup"
                        } else {
                            "private_group"
                        }
                    } else if chat.is_broadcast {
                        "public_channel"
                    } else {
                        "public_supergroup"
                    }
                } else {
                    "user"
                };
                block.extend_from_slice(&self.prepare_array_item_start());
                let obj = serialize_object(
                    &mut self.context,
                    &[
                        (
                            "id",
                            peer_to_bare_id(top.peer.id()).to_string().into_bytes(),
                        ),
                        ("category", serialize_json_string(category)),
                        ("type", serialize_json_string(kind_str)),
                        ("name", string_allow_null(&top.peer.name())),
                        ("rating", float_allow_null(top.rating)),
                    ],
                );
                block.extend_from_slice(&obj);
            }
        }
        block.extend_from_slice(&self.pop_nesting());
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn write_sessions(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.prepare_object_item_start("sessions");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(&self.environment.about_sessions));
        block.extend_from_slice(&self.prepare_object_item_start("list"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        for session in &data.list {
            block.extend_from_slice(&self.prepare_array_item_start());
            let last_active_iso = serialize_date(&self.context, session.last_active);
            let created_iso = serialize_date(&self.context, session.created);
            let obj = serialize_object(
                &mut self.context,
                &[
                    ("last_active", last_active_iso),
                    (
                        "last_active_unixtime",
                        serialize_date_raw(session.last_active),
                    ),
                    ("last_ip", serialize_json_string(&session.ip)),
                    ("last_country", serialize_json_string(&session.country)),
                    ("last_region", serialize_json_string(&session.region)),
                    (
                        "application_name",
                        string_allow_null(&session.application_name),
                    ),
                    (
                        "application_version",
                        string_allow_empty(&session.application_version),
                    ),
                    ("device_model", serialize_json_string(&session.device_model)),
                    ("platform", serialize_json_string(&session.platform)),
                    (
                        "system_version",
                        serialize_json_string(&session.system_version),
                    ),
                    ("created", created_iso),
                    ("created_unixtime", serialize_date_raw(session.created)),
                ],
            );
            block.extend_from_slice(&obj);
        }
        block.extend_from_slice(&self.pop_nesting());
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn write_web_sessions(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.prepare_object_item_start("web_sessions");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(&self.environment.about_web_sessions));
        block.extend_from_slice(&self.prepare_object_item_start("list"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        for session in &data.web_list {
            block.extend_from_slice(&self.prepare_array_item_start());
            let last_active_iso = serialize_date(&self.context, session.last_active);
            let created_iso = serialize_date(&self.context, session.created);
            let obj = serialize_object(
                &mut self.context,
                &[
                    ("last_active", last_active_iso),
                    (
                        "last_active_unixtime",
                        serialize_date_raw(session.last_active),
                    ),
                    ("last_ip", serialize_json_string(&session.ip)),
                    ("last_region", serialize_json_string(&session.region)),
                    ("bot_username", string_allow_null(&session.bot_username)),
                    ("domain_name", string_allow_null(&session.domain)),
                    ("browser", serialize_json_string(&session.browser)),
                    ("platform", serialize_json_string(&session.platform)),
                    ("created", created_iso),
                    ("created_unixtime", serialize_date_raw(session.created)),
                ],
            );
            block.extend_from_slice(&obj);
        }
        block.extend_from_slice(&self.pop_nesting());
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn validate_dialogs_mode(
        &mut self,
        is_left_channel: bool,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mode = if is_left_channel {
            DialogsMode::Left
        } else {
            DialogsMode::Chats
        };
        if self.dialogs_mode == mode {
            return OutputResult::success();
        }
        if self.dialogs_mode != DialogsMode::None {
            let res = self.write_chats_end(stats);
            if res.is_error() {
                return res;
            }
        }
        self.dialogs_mode = mode;
        let (list_name, about) = if is_left_channel {
            ("left_chats", self.environment.about_left_chats.clone())
        } else {
            ("chats", self.environment.about_chats.clone())
        };
        self.write_chats_start(list_name, &about, stats)
    }

    fn write_chats_start(
        &mut self,
        list_name: &str,
        about: &str,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mut block = self.prepare_object_item_start(list_name);
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(about));
        block.extend_from_slice(&self.prepare_object_item_start("list"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        self.write_block(&block, stats)
    }

    fn write_chats_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.pop_nesting();
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn push_json_value(&mut self, block: &mut Vec<u8>, value: &Value) {
        match value {
            Value::Null => block.extend_from_slice(b"null"),
            Value::Bool(b) => block.extend_from_slice(if *b { b"true" } else { b"false" }),
            Value::Number(n) => {
                // QJsonValue stores all numbers as double, then formats via Data::NumberToString(double).
                if let Some(f) = n.as_f64() {
                    block.extend_from_slice(float_to_string(f).as_bytes());
                } else {
                    block.extend_from_slice(n.to_string().as_bytes());
                }
            }
            Value::String(s) => block.extend_from_slice(&serialize_json_string(s)),
            Value::Array(arr) => {
                block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
                for item in arr {
                    block.extend_from_slice(&self.prepare_array_item_start());
                    self.push_json_value(block, item);
                }
                block.extend_from_slice(&self.pop_nesting());
            }
            Value::Object(map) => {
                block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
                // serde_json Map uses BTreeMap by default (sorted keys, matching QJsonObject!).
                for (k, v) in map {
                    block.extend_from_slice(&self.prepare_object_item_start(k));
                    self.push_json_value(block, v);
                }
                block.extend_from_slice(&self.pop_nesting());
            }
        }
    }
}

impl AbstractWriter for JsonWriter {
    fn format(&self) -> OutputFormat {
        OutputFormat::Json
    }

    fn start(
        &mut self,
        settings: &Settings,
        environment: &Environment,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.settings = settings.clone();
        self.environment = environment.clone();
        self.context = JsonContext {
            nesting: Vec::new(),
            utc_offset_seconds: environment.utc_offset_seconds,
        };
        self.current_nesting_had_item = false;
        self.dialogs_mode = DialogsMode::None;
        let path = self.path_with_relative_path(self.main_file_relative_path());
        self.output = Some(OutputFile::new(path));
        if self.settings.only_single_peer() {
            return OutputResult::success();
        }
        let mut block = self.push_nesting(ContainerKind::Object);
        block.extend_from_slice(&self.prepare_object_item_start("about"));
        block.extend_from_slice(&serialize_json_string(&self.environment.about_telegram));
        self.write_block(&block, stats)
    }

    fn write_personal(&mut self, data: &PersonalInfo, stats: Option<&Stats>) -> OutputResult {
        let info = &data.user.info;
        let username = if data.user.username.is_empty() {
            Vec::new()
        } else {
            serialize_json_string(&format_username(&data.user.username))
        };
        let bio = if data.bio.is_empty() {
            Vec::new()
        } else {
            serialize_json_string(&data.bio)
        };
        let mut block = self.prepare_object_item_start("personal_information");
        let obj = serialize_object(
            &mut self.context,
            &[
                ("user_id", data.user.bare_id.to_string().into_bytes()),
                ("first_name", serialize_json_string(&info.first_name)),
                ("last_name", serialize_json_string(&info.last_name)),
                (
                    "phone_number",
                    serialize_json_string(&format_phone_number(&info.phone_number)),
                ),
                ("username", username),
                ("bio", bio),
            ],
        );
        block.extend_from_slice(&obj);
        self.write_block(&block, stats)
    }

    fn write_userpics_start(
        &mut self,
        _data: &UserpicsInfo,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mut block = self.prepare_object_item_start("profile_pictures");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        self.write_block(&block, stats)
    }

    fn write_userpics_slice(
        &mut self,
        data: &UserpicsSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mut block = Vec::new();
        for userpic in &data.list {
            let file = &userpic.image.file;
            let path = match file.skip_reason {
                SkipReason::Unavailable | SkipReason::DateLimits => {
                    "(Photo unavailable, please try again later)"
                }
                SkipReason::FileSize => {
                    "(Photo exceeds maximum size. Change data exporting settings to download.)"
                }
                SkipReason::FileType => {
                    "(Photo not included. Change data exporting settings to download.)"
                }
                SkipReason::None => file.relative_path.as_str(),
            };
            block.extend_from_slice(&self.prepare_array_item_start());
            let date_iso = if userpic.date != 0 {
                serialize_date(&self.context, userpic.date)
            } else {
                Vec::new()
            };
            let date_raw = if userpic.date != 0 {
                serialize_date_raw(userpic.date)
            } else {
                Vec::new()
            };
            let obj = serialize_object(
                &mut self.context,
                &[
                    ("date", date_iso),
                    ("date_unixtime", date_raw),
                    ("photo", serialize_json_string(path)),
                ],
            );
            block.extend_from_slice(&obj);
        }
        self.write_block(&block, stats)
    }

    fn write_userpics_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let block = self.pop_nesting();
        self.write_block(&block, stats)
    }

    fn write_stories_start(&mut self, _data: &StoriesInfo, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.prepare_object_item_start("stories");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        self.write_block(&block, stats)
    }

    fn write_stories_slice(&mut self, data: &StoriesSlice, stats: Option<&Stats>) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let mut block = Vec::new();
        for story in &data.list {
            let default_file = File::default();
            let file = story.file().unwrap_or(&default_file);
            let path = match file.skip_reason {
                SkipReason::Unavailable | SkipReason::DateLimits => {
                    "(Photo unavailable, please try again later)"
                }
                SkipReason::FileSize => {
                    "(Photo exceeds maximum size. Change data exporting settings to download.)"
                }
                SkipReason::FileType => {
                    "(Photo not included. Change data exporting settings to download.)"
                }
                SkipReason::None => file.relative_path.as_str(),
            };
            block.extend_from_slice(&self.prepare_array_item_start());
            let date_iso = if story.date != 0 {
                serialize_date(&self.context, story.date)
            } else {
                Vec::new()
            };
            let date_raw = if story.date != 0 {
                serialize_date_raw(story.date)
            } else {
                Vec::new()
            };
            let expires_iso = if story.expires != 0 {
                serialize_date(&self.context, story.expires)
            } else {
                Vec::new()
            };
            let expires_raw = if story.expires != 0 {
                serialize_date_raw(story.expires)
            } else {
                Vec::new()
            };
            let obj = serialize_object(
                &mut self.context,
                &[
                    ("date", date_iso),
                    ("date_unixtime", date_raw),
                    ("expires", expires_iso),
                    ("expires_unixtime", expires_raw),
                    ("pinned", serialize_bool(story.pinned)),
                    ("media", serialize_json_string(path)),
                ],
            );
            block.extend_from_slice(&obj);
        }
        self.write_block(&block, stats)
    }

    fn write_stories_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let block = self.pop_nesting();
        self.write_block(&block, stats)
    }

    fn write_profile_music_start(
        &mut self,
        _data: &ProfileMusicInfo,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let mut block = self.prepare_object_item_start("profile_music");
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        self.write_block(&block, stats)
    }

    fn write_profile_music_slice(
        &mut self,
        data: &ProfileMusicSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let empty_peers = BTreeMap::new();
        let mut block = Vec::new();
        for message in &data.list {
            block.extend_from_slice(&self.prepare_array_item_start());
            block.extend_from_slice(&serialize_message(
                &mut self.context,
                message,
                &empty_peers,
                "",
            ));
        }
        self.write_block(&block, stats)
    }

    fn write_profile_music_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let block = self.pop_nesting();
        self.write_block(&block, stats)
    }

    fn write_contacts_list(&mut self, data: &ContactsList, stats: Option<&Stats>) -> OutputResult {
        let res = self.write_saved_contacts(data, stats);
        if res.is_error() {
            return res;
        }
        self.write_frequent_contacts(data, stats)
    }

    fn write_sessions_list(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        let res = self.write_sessions(data, stats);
        if res.is_error() {
            return res;
        }
        self.write_web_sessions(data, stats)
    }

    fn write_other_data(&mut self, data: &File, stats: Option<&Stats>) -> OutputResult {
        let full_path = self.path_with_relative_path(&data.relative_path);
        let content = match fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(_) => return OutputResult::fatal_error(full_path),
        };
        if content.is_empty() {
            return OutputResult::success();
        }
        let parsed: Value = match serde_json::from_slice(&content) {
            Ok(v) => v,
            Err(_) => return OutputResult::fatal_error(full_path),
        };
        let mut block = self.prepare_object_item_start("other_data");
        self.push_json_value(&mut block, &parsed);
        self.write_block(&block, stats)
    }

    fn write_dialogs_start(&mut self, _data: &DialogsInfo, _stats: Option<&Stats>) -> OutputResult {
        OutputResult::success()
    }

    fn write_dialog_start(&mut self, data: &DialogInfo, stats: Option<&Stats>) -> OutputResult {
        if !self.settings.only_single_peer() {
            let res = self.validate_dialogs_mode(data.is_left_channel, stats);
            if res.is_error() {
                return res;
            }
        }
        let type_str = match data.kind {
            DialogType::Unknown => "",
            DialogType::SelfChat => "saved_messages",
            DialogType::Replies => "replies",
            DialogType::VerifyCodes => "verification_codes",
            DialogType::Personal => "personal_chat",
            DialogType::Bot => "bot_chat",
            DialogType::PrivateGroup => "private_group",
            DialogType::PrivateSupergroup => "private_supergroup",
            DialogType::PublicSupergroup => "public_supergroup",
            DialogType::PrivateChannel => "private_channel",
            DialogType::PublicChannel => "public_channel",
        };
        let mut block = if self.settings.only_single_peer() {
            Vec::new()
        } else {
            self.prepare_array_item_start()
        };
        block.extend_from_slice(&self.push_nesting(ContainerKind::Object));
        if !matches!(
            data.kind,
            DialogType::SelfChat | DialogType::Replies | DialogType::VerifyCodes
        ) {
            block.extend_from_slice(&self.prepare_object_item_start("name"));
            block.extend_from_slice(&string_allow_null(&data.name));
        }
        block.extend_from_slice(&self.prepare_object_item_start("type"));
        block.extend_from_slice(&string_allow_null(type_str));
        block.extend_from_slice(&self.prepare_object_item_start("id"));
        block.extend_from_slice(peer_to_bare_id(data.peer_id).to_string().as_bytes());
        block.extend_from_slice(&self.prepare_object_item_start("messages"));
        block.extend_from_slice(&self.push_nesting(ContainerKind::Array));
        self.write_block(&block, stats)
    }

    fn write_dialog_slice(&mut self, data: &MessagesSlice, stats: Option<&Stats>) -> OutputResult {
        let mut block = Vec::new();
        let domain = self.environment.internal_links_domain.clone();
        for message in &data.list {
            if skip_message_by_date(message, &self.settings) {
                continue;
            }
            block.extend_from_slice(&self.prepare_array_item_start());
            block.extend_from_slice(&serialize_message(
                &mut self.context,
                message,
                &data.peers,
                &domain,
            ));
        }
        if block.is_empty() {
            OutputResult::success()
        } else {
            self.write_block(&block, stats)
        }
    }

    fn write_dialog_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let mut block = self.pop_nesting();
        block.extend_from_slice(&self.pop_nesting());
        self.write_block(&block, stats)
    }

    fn write_dialogs_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        if self.settings.only_single_peer() || self.dialogs_mode == DialogsMode::None {
            return OutputResult::success();
        }
        self.write_chats_end(stats)
    }

    fn finish(&mut self, stats: Option<&Stats>) -> OutputResult {
        if self.settings.only_single_peer() {
            return OutputResult::success();
        }
        let block = self.pop_nesting();
        self.write_block(&block, stats)
    }

    fn main_file_path(&self) -> String {
        self.path_with_relative_path(self.main_file_relative_path())
    }
}
