//! Port of `export/output/export_output_html.h` and `export/output/export_output_html.cpp`.
//!
//! Generates the HTML export directory (`export_results.html`, `lists/*.html`,
//! `chats/chat_*/messages*.html`, CSS/JS/images assets, and image thumbnails) matching
//! Telegram Desktop's exact markup, class names, pagination (1000 messages per file),
//! userpic placeholders, service messages, media blocks, and rich messages.

use std::collections::BTreeMap;

use image::{ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::data::{
    ContactsList, DialogInfo, DialogType, DialogsInfo, Document, File, GiveawayResults,
    GiveawayStart, InlineButtonAction, InlineButtonActionType, InlineButtonPeerType, MediaContent,
    Message, MessagesSlice, Peer, PersonalInfo, PhoneCallState, Photo, Poll, ProfileMusicInfo,
    ProfileMusicSlice, ReactionType, RichBlock, RichBlockKind, RichButtonStyle, RichCaption,
    RichChannel, RichChannelSource, RichListItem, RichListItemContent, RichListKind, RichMapPoint,
    RichMapPointSource, RichMessage, RichQuoteContent, RichRelatedArticle, RichTableAlignment,
    RichTableCell, RichTableRow, RichTableVerticalAlignment, RichTaskState, RichText, RichTextType,
    SecureValueType, ServiceAction, SessionsList, SkipReason, StoriesInfo, StoriesSlice, TextPart,
    TextPartType, TodoList, TopPeer, User, UserpicsInfo, UserpicsSlice, application_color_index,
    contact_color_index, domain_application_id, format_date_text_with_offset,
    format_date_time_with_offset, format_duration, format_file_size, format_grams_amount,
    format_iso_date_with_offset, format_money_amount, format_phone_number,
    format_time_text_with_offset, is_svg_format, peer_color_index, peer_color_index_peer,
    skip_message_by_date, sorted_contacts_indices, write_image_thumb, write_image_thumb_exact,
};
use crate::output::assets::EXPORT_HTML_ASSETS;
use crate::output::file::OutputFile;
use crate::output::result::OutputResult;
use crate::output::stats::Stats;
use crate::output::{AbstractWriter, OutputFormat};
use crate::settings::{Environment, ExportTypes, Settings};
use crate::stand_in::{
    PEER_ID_CHAT_TYPE_MASK, PeerId, Rect, Size, TimeId, UserId, base64_url_no_pad,
    country_name_by_iso2, credits_is_empty, credits_is_ton, flag_emoji_by_iso2,
    format_image_size_text, layout_media_group_geometry, percent_encode_url, rect_united,
    scale_size_keep_aspect_ratio, unix_to_civil,
};

/// Maximum messages written into a single `messages*.html` file (`kMessagesInFile`).
pub const MESSAGES_IN_FILE: usize = 1000;

const PERSONAL_USERPIC_SIZE: i32 = 90;
const ENTRY_USERPIC_SIZE: i32 = 48;
const SERVICE_MESSAGE_PHOTO_SIZE: i32 = 60;
const HISTORY_USERPIC_SIZE: i32 = 42;
const SAVED_MESSAGES_COLOR_INDEX: u8 = 3;
const JOIN_WITHIN_SECONDS: i32 = 900;
const PHOTO_MAX_WIDTH: i32 = 520;
const PHOTO_MAX_HEIGHT: i32 = 520;
const PHOTO_MIN_WIDTH: i32 = 80;
const PHOTO_MIN_HEIGHT: i32 = 80;
const STICKER_MAX_WIDTH: i32 = 384;
const STICKER_MAX_HEIGHT: i32 = 384;
const STORY_THUMB_WIDTH: i32 = 45;
const STORY_THUMB_HEIGHT: i32 = 80;
const GROUP_WIDTH_MAX: i32 = 430;
const GROUP_WIDTH_MIN: i32 = 100;
const GROUP_SKIP: i32 = 4;

const CHATS_PRIORITY: i32 = 0;
const CONTACTS_PRIORITY: i32 = 2;
const FREQUENT_CONTACTS_PRIORITY: i32 = 3;
const USERPICS_PRIORITY: i32 = 4;
const STORIES_PRIORITY: i32 = 5;
const PROFILE_MUSIC_PRIORITY: i32 = 6;
const SESSIONS_PRIORITY: i32 = 7;
const WEB_SESSIONS_PRIORITY: i32 = 8;
const OTHER_PRIORITY: i32 = 9;

const RICH_TARGET_MAX_BYTES: usize = 4096;
const RICH_ANCHOR_DIRECT_BYTES: usize = 72;
const RICH_ANCHOR_PREFIX_BYTES: usize = 48;
const RICH_TABLE_SPAN_MAX: i32 = 1000;
const RICH_HASHTAG_MIN_LENGTH: usize = 2;
const RICH_HASHTAG_MAX_LENGTH: usize = 64;
const RICH_COMMAND_MAX_LENGTH: usize = 64;
const RICH_CASHTAG_MAX_LENGTH: usize = 8;
const RICH_USERNAME_MIN_LENGTH: usize = 5;
const RICH_USERNAME_MAX_LENGTH: usize = 32;

/// Escapes UTF-8 bytes for HTML text and attribute values (exact port of `SerializeString`
/// in `export/output/export_output_html.cpp:158-203`).
pub fn serialize_html_string(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 2);
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        match ch {
            b'\n' => out.extend_from_slice(b"<br>"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            b'\'' => out.extend_from_slice(b"&apos;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            0..=31 => {
                out.extend_from_slice(b"&#x");
                out.push(b'0' + (ch >> 4));
                let left = ch & 0x0F;
                if left >= 10 {
                    out.push(b'A' + (left - 10));
                } else {
                    out.push(b'0' + left);
                }
                out.push(b';');
            }
            0xE2 if i + 2 < bytes.len()
                && bytes[i + 1] == 0x80
                && (bytes[i + 2] == 0xA8 || bytes[i + 2] == 0xA9) =>
            {
                out.extend_from_slice(b"<br>");
                i += 2;
            }
            _ => out.push(ch),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

fn is_global_link(link: &str) -> bool {
    let lower = link.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn no_file_description(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::Unavailable | SkipReason::DateLimits => "Unavailable, please try again later.",
        SkipReason::FileSize => "Exceeds maximum size, change data exporting settings to download.",
        SkipReason::FileType => "Not included, change data exporting settings to download.",
        SkipReason::None => "",
    }
}

fn rich_file_presentation(file: Option<&File>) -> File {
    let mut result = file.cloned().unwrap_or_default();
    if !result.relative_path.is_empty() {
        result.skip_reason = SkipReason::None;
    } else {
        match result.skip_reason {
            SkipReason::FileType | SkipReason::FileSize => {}
            SkipReason::None | SkipReason::Unavailable | SkipReason::DateLimits => {
                result.skip_reason = SkipReason::Unavailable;
            }
        }
    }
    result
}

fn rich_photo_presentation(photo: Option<&Photo>) -> Photo {
    let mut result = photo.cloned().unwrap_or_default();
    result.image.file = rich_file_presentation(photo.map(|p| &p.image.file));
    result
}

fn rich_document_presentation(document: Option<&Document>) -> Document {
    let mut result = document.cloned().unwrap_or_default();
    result.file = rich_file_presentation(document.map(|d| &d.file));
    result.thumb.file = rich_file_presentation(document.map(|d| &d.thumb.file));
    result
}

fn rich_file_description(file: Option<&File>) -> &'static str {
    no_file_description(rich_file_presentation(file).skip_reason)
}

fn calculate_thumb_size(
    large_size: Size,
    max_width: i32,
    max_height: i32,
    expand_for_retina: bool,
) -> Size {
    let multiplier = if expand_for_retina { 2 } else { 1 };
    let check_width = large_size.width * multiplier;
    let check_height = large_size.height * multiplier;
    let small_size = if check_width > max_width || check_height > max_height {
        scale_size_keep_aspect_ratio(large_size, max_width, max_height)
    } else {
        large_size
    };
    let retina_size = Size::new(small_size.width & !0x01, small_size.height & !0x01);
    if retina_size.width < PHOTO_MIN_WIDTH || retina_size.height < PHOTO_MIN_HEIGHT {
        Size::default()
    } else {
        retina_size
    }
}

fn serialize_list(values: &[String]) -> String {
    match values.len() {
        0 => String::new(),
        1 => values[0].clone(),
        n => {
            let mut out = values[0].clone();
            for item in &values[1..(n - 1)] {
                out.push_str(", ");
                out.push_str(item);
            }
            out.push_str(" and ");
            out.push_str(&values[n - 1]);
            out
        }
    }
}

fn make_links(value: &str) -> String {
    const DOMAIN: &str = "https://telegram.org/";
    let mut result = String::new();
    let mut offset = 0;
    let bytes = value.as_bytes();
    while let Some(rel_start) = value[offset..].find(DOMAIN) {
        let start = offset + rel_start;
        let mut end = start + DOMAIN.len();
        while end < bytes.len() {
            let ch = bytes[end];
            if !ch.is_ascii_alphanumeric() && ch != b'-' && ch != b'_' && ch != b'/' {
                break;
            }
            end += 1;
        }
        if start > offset {
            let link = &value[start..end];
            result.push_str(&value[offset..start]);
            result.push_str("<a href=\"");
            result.push_str(link);
            result.push_str("\">");
            result.push_str(link);
            result.push_str("</a>");
            offset = end;
        } else {
            break;
        }
    }
    if result.is_empty() {
        return value.to_string();
    }
    if offset < value.len() {
        result.push_str(&value[offset..]);
    }
    result
}

fn hex_value(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

fn has_encoded_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            let decoded = (hi << 4) | lo;
            if decoded < 32 || decoded == 127 {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_strict_target(value: &str) -> bool {
    if value.is_empty() || value.len() > RICH_TARGET_MAX_BYTES {
        return false;
    }
    if value.bytes().any(|b| b < 32 || b == 127) {
        return false;
    }
    if value.trim() != value {
        return false;
    }
    for ch in value.chars() {
        if matches!(
            get_general_category(ch),
            GeneralCategory::Control
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        ) {
            return false;
        }
    }
    true
}

fn parse_scheme_and_host(url: &str) -> Option<(String, &str)> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let rest = &url[(colon + 1)..];
    let host = if let Some(after_slashes) = rest.strip_prefix("//") {
        let authority_end = after_slashes
            .find(['/', '?', '#'])
            .unwrap_or(after_slashes.len());
        let authority = &after_slashes[..authority_end];
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        if let Some(bracket_end) = host_port.strip_prefix('[').and_then(|s| s.find(']')) {
            &host_port[..=(bracket_end + 1)]
        } else {
            host_port.split(':').next().unwrap_or("")
        }
    } else {
        ""
    };
    Some((scheme.to_ascii_lowercase(), host))
}

/// Validates and normalizes a message hyperlink target (port of `SafeMessageHref`).
pub fn safe_message_href(value: &str) -> Option<String> {
    if !is_strict_target(value) || value.starts_with("//") || has_encoded_control(value) {
        return None;
    }
    let source = if parse_scheme_and_host(value).is_none() {
        format!("https://{value}")
    } else {
        value.to_string()
    };
    let (scheme, host) = parse_scheme_and_host(&source)?;
    if !matches!(scheme.as_str(), "http" | "https" | "mailto" | "tel" | "tg") {
        return None;
    }
    if matches!(scheme.as_str(), "http" | "https") && host.is_empty() {
        return None;
    }
    if source.chars().any(char::is_whitespace) {
        return None;
    }
    if is_strict_target(&source) && !has_encoded_control(&source) {
        Some(source)
    } else {
        None
    }
}

/// Validates an HTTP/HTTPS URL (port of `SafeHttpHref`).
pub fn safe_http_href(value: &str) -> Option<String> {
    if !is_strict_target(value) || value.starts_with("//") || has_encoded_control(value) {
        return None;
    }
    let (scheme, host) = parse_scheme_and_host(value)?;
    if !matches!(scheme.as_str(), "http" | "https") || host.is_empty() {
        return None;
    }
    if value.chars().any(char::is_whitespace) {
        return None;
    }
    Some(value.to_string())
}

fn safe_email_href(address: &str) -> Option<String> {
    if !is_strict_target(address) {
        return None;
    }
    let sep = address.find('@')?;
    if sep == 0 || Some(sep) != address.rfind('@') || sep + 1 == address.len() {
        return None;
    }
    if address.chars().any(char::is_whitespace) {
        return None;
    }
    if address
        .bytes()
        .any(|b| matches!(b, b'/' | b'\\' | b':' | b'%' | b'?' | b'#' | b'&' | b'='))
    {
        return None;
    }
    let result = format!("mailto:{address}");
    is_strict_target(&result).then_some(result)
}

fn safe_phone_href(phone: &str) -> Option<String> {
    if !is_strict_target(phone) {
        return None;
    }
    let mut has_digit = false;
    for ch in phone.bytes() {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(ch, b'+' | b'-' | b' ' | b'.' | b'(' | b')') {
            return None;
        }
    }
    let result = format!("tel:{phone}");
    (has_digit && is_strict_target(&result)).then_some(result)
}

fn is_ascii_word(value: &str, min_len: usize, max_len: usize) -> bool {
    value.len() >= min_len
        && value.len() <= max_len
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn safe_mention_href(mention: &str, internal_links_domain: &str) -> Option<String> {
    if !is_strict_target(mention) || mention.len() < 2 {
        return None;
    }
    let username = mention.strip_prefix('@')?;
    if !username
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    safe_http_href(&format!("{internal_links_domain}{username}"))
}

fn is_unicode_hashtag_word(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < RICH_HASHTAG_MIN_LENGTH || chars.len() > RICH_HASHTAG_MAX_LENGTH {
        return false;
    }
    let mut all_decimal = true;
    for ch in chars {
        let cat = get_general_category(ch);
        let is_mark = matches!(
            cat,
            GeneralCategory::NonspacingMark
                | GeneralCategory::SpacingMark
                | GeneralCategory::EnclosingMark
        );
        if !ch.is_alphanumeric() && cat != GeneralCategory::ConnectorPunctuation && !is_mark {
            return false;
        }
        all_decimal = all_decimal && ch.is_numeric();
    }
    !all_decimal
}

fn is_hashtag_action(value: &str) -> bool {
    match value.find('@') {
        None => is_unicode_hashtag_word(value),
        Some(sep) => {
            is_unicode_hashtag_word(&value[..sep])
                && Some(sep) == value.rfind('@')
                && is_ascii_word(&value[(sep + 1)..], 1, RICH_USERNAME_MAX_LENGTH)
        }
    }
}

fn is_bot_command_action(value: &str) -> bool {
    match value.find('@') {
        None => is_ascii_word(value, 1, RICH_COMMAND_MAX_LENGTH),
        Some(sep) => {
            is_ascii_word(&value[..sep], 1, RICH_COMMAND_MAX_LENGTH)
                && Some(sep) == value.rfind('@')
                && is_ascii_word(
                    &value[(sep + 1)..],
                    RICH_USERNAME_MIN_LENGTH,
                    RICH_USERNAME_MAX_LENGTH,
                )
        }
    }
}

fn is_cashtag_action(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= RICH_CASHTAG_MAX_LENGTH
        && value.bytes().all(|b| b.is_ascii_uppercase())
}

fn safe_action_data(target: &str, prefix: char) -> Option<String> {
    if !is_strict_target(target) || target.len() < 2 {
        return None;
    }
    let rest = target.strip_prefix(prefix)?;
    let valid = match prefix {
        '#' => is_hashtag_action(rest),
        '/' => is_bot_command_action(rest),
        '$' => is_cashtag_action(rest),
        _ => false,
    };
    (valid && is_strict_target(rest)).then(|| rest.to_string())
}

fn is_ascii_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

fn safe_relative_emoji_href(path: &str, relative_base: &str) -> Option<String> {
    if !is_strict_target(path)
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.contains('%')
        || path.contains('?')
        || path.contains('#')
        || path == TextPart::UNAVAILABLE_EMOJI
        || is_ascii_decimal(path)
    {
        return None;
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None;
        }
    }
    let result = format!("{relative_base}{path}");
    is_strict_target(&result).then_some(result)
}

fn format_text_link(text: &str, target: &str) -> String {
    match safe_message_href(target) {
        Some(href) => format!("<a href=\"{}\">{text}</a>", serialize_html_string(&href)),
        None => text.to_string(),
    }
}

fn format_custom_emoji(custom_emoji: &str, text: &str, relative_link_base: &str) -> String {
    let open = if custom_emoji.is_empty() {
        "<a href=\"\" onclick=\"return ShowNotLoadedEmoji();\">".to_string()
    } else if custom_emoji == TextPart::UNAVAILABLE_EMOJI {
        "<a href=\"\" onclick=\"return ShowNotAvailableEmoji();\">".to_string()
    } else {
        // Matches exact spacing in `export_output_html.cpp:303` (`<a href = "..."`).
        format!(
            "<a href = \"{}\">",
            serialize_html_string(&format!("{relative_link_base}{custom_emoji}"))
        )
    };
    format!("{open}{text}</a>")
}

/// Formats `TextPart` slices into HTML (port of `FormatText` in `export_output_html.cpp:310-369`).
pub fn format_html_text(
    data: &[TextPart],
    internal_links_domain: &str,
    relative_link_base: &str,
) -> String {
    let mut out = String::new();
    for part in data {
        let text = serialize_html_string(&part.text);
        let segment = match part.kind {
            TextPartType::Text
            | TextPartType::Unknown
            | TextPartType::BankCard
            | TextPartType::TonAddress => text,
            TextPartType::Mention => {
                let tail = text.get(1..).unwrap_or("");
                format!("<a href=\"{internal_links_domain}{tail}\">{text}</a>")
            }
            TextPartType::Hashtag => {
                let tail = text.get(1..).unwrap_or("");
                format!(
                    "<a href=\"\" onclick=\"return ShowHashtag({})\">{text}</a>",
                    serialize_html_string(&format!("\"{tail}\""))
                )
            }
            TextPartType::BotCommand => {
                let tail = text.get(1..).unwrap_or("");
                format!(
                    "<a href=\"\" onclick=\"return ShowBotCommand({})\">{text}</a>",
                    serialize_html_string(&format!("\"{tail}\""))
                )
            }
            TextPartType::Url => format_text_link(&text, &part.text),
            TextPartType::Email => format!("<a href=\"mailto:{text}\">{text}</a>"),
            TextPartType::Bold => format!("<strong>{text}</strong>"),
            TextPartType::Italic => format!("<em>{text}</em>"),
            TextPartType::Code => format!("<code>{text}</code>"),
            TextPartType::Pre => format!("<pre>{text}</pre>"),
            TextPartType::TextUrl => format_text_link(&text, &part.additional),
            TextPartType::MentionName => {
                format!("<a href=\"\" onclick=\"return ShowMentionName()\">{text}</a>")
            }
            TextPartType::Phone => format!("<a href=\"tel:{text}\">{text}</a>"),
            TextPartType::Cashtag => {
                let tail = text.get(1..).unwrap_or("");
                format!(
                    "<a href=\"\" onclick=\"return ShowCashtag({})\">{text}</a>",
                    serialize_html_string(&format!("\"{tail}\""))
                )
            }
            TextPartType::Underline => format!("<u>{text}</u>"),
            TextPartType::Strike => format!("<s>{text}</s>"),
            TextPartType::Blockquote => format!("<blockquote>{text}</blockquote>"),
            TextPartType::Spoiler => format!(
                "<span class=\"spoiler hidden\" onclick=\"ShowSpoiler(this)\"><span aria-hidden=\"true\">{text}</span></span>"
            ),
            TextPartType::CustomEmoji => {
                format_custom_emoji(&part.additional, &text, relative_link_base)
            }
        };
        out.push_str(&segment);
    }
    out
}

fn format_username(username: &str) -> String {
    if username.is_empty() {
        String::new()
    } else {
        format!("@{username}")
    }
}

fn display_date(date: TimeId, previous_date: TimeId, utc_offset_secs: i32) -> bool {
    if previous_date == 0 {
        return true;
    }
    let (y1, m1, d1, _, _, _) = unix_to_civil(i64::from(date), utc_offset_secs);
    let (y2, m2, d2, _, _, _) = unix_to_civil(i64::from(previous_date), utc_offset_secs);
    (y1, m1, d1) != (y2, m2, d2)
}

#[derive(Debug, Clone)]
struct TagEntry {
    name: String,
    block: bool,
}

/// Tag-stack HTML formatter with 1-space indentation (port of `details::HtmlContext`).
#[derive(Debug, Default)]
struct HtmlContext {
    tags: Vec<TagEntry>,
}

impl HtmlContext {
    fn push_tag(&mut self, tag: &str, attributes: BTreeMap<&str, String>) -> String {
        let mut block = true;
        let mut empty = false;
        let mut inner = String::new();
        for (name, value) in attributes {
            if name == "inline" {
                block = false;
            } else if name == "empty" {
                empty = true;
            } else if !name.is_empty() {
                inner.push(' ');
                inner.push_str(name);
                inner.push_str("=\"");
                inner.push_str(&serialize_html_string(&value));
                inner.push('"');
            }
        }
        let mut result = String::new();
        if block {
            result.push('\n');
            result.push_str(&self.indent());
        }
        result.push('<');
        result.push_str(tag);
        result.push_str(&inner);
        if empty {
            result.push('/');
        }
        result.push('>');
        if block {
            result.push('\n');
        }
        if !empty {
            self.tags.push(TagEntry {
                name: tag.to_string(),
                block,
            });
        }
        result
    }

    fn pop_tag(&mut self) -> String {
        let Some(data) = self.tags.pop() else {
            return String::new();
        };
        let mut result = String::new();
        if data.block {
            result.push('\n');
            result.push_str(&self.indent());
        }
        result.push_str("</");
        result.push_str(&data.name);
        result.push('>');
        if data.block {
            result.push('\n');
        }
        result
    }

    fn indent(&self) -> String {
        " ".repeat(self.tags.len())
    }

    fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
struct UserpicData {
    color_index: u8,
    pixel_size: i32,
    image_link: String,
    large_link: String,
    first_name: String,
    last_name: String,
    tooltip: String,
}

#[derive(Debug, Clone, Default)]
struct StoryData {
    image_link: String,
    link: String,
}

#[derive(Debug, Clone, Default)]
struct MediaData {
    title: String,
    description: String,
    status: String,
    classes: String,
    thumb: String,
    link: String,
}

struct PeersMap<'a> {
    data: &'a BTreeMap<PeerId, Peer>,
    empty_peer: Peer,
    empty_user: User,
}

impl<'a> PeersMap<'a> {
    fn new(data: &'a BTreeMap<PeerId, Peer>) -> Self {
        Self {
            data,
            empty_peer: Peer::default(),
            empty_user: User::default(),
        }
    }

    fn peer(&self, peer_id: PeerId) -> &Peer {
        self.data.get(&peer_id).unwrap_or(&self.empty_peer)
    }

    fn user(&self, user_id: UserId) -> &User {
        self.peer(PeerId::from_user(user_id))
            .as_user()
            .unwrap_or(&self.empty_user)
    }

    fn wrap_peer_name(&self, peer_id: PeerId) -> String {
        let name = self.peer(peer_id).name();
        if name.is_empty() {
            "Deleted".to_string()
        } else {
            serialize_html_string(&name)
        }
    }

    fn wrap_user_name(&self, user_id: UserId) -> String {
        let name = self.user(user_id).name();
        if name.is_empty() {
            "Deleted Account".to_string()
        } else {
            serialize_html_string(&name)
        }
    }

    fn wrap_user_names(&self, ids: &[UserId]) -> String {
        let list: Vec<String> = ids.iter().map(|&id| self.wrap_user_name(id)).collect();
        serialize_list(&list)
    }
}

fn fill_userpic_names_from_peer(data: &mut UserpicData, peer: &Peer) {
    if let Some(u) = peer.as_user() {
        data.first_name = u.info.first_name.clone();
        data.last_name = u.info.last_name.clone();
    } else if peer.as_chat().is_some() {
        data.first_name = peer.name();
    }
}

fn fill_userpic_names_from_full(data: &mut UserpicData, full: &str) {
    let mut parts = full.split(' ');
    if let Some(first) = parts.next() {
        data.first_name = first.to_string();
    }
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if !data.last_name.is_empty() {
            data.last_name.push(' ');
        }
        data.last_name.push_str(part);
    }
}

fn compose_name(data: &UserpicData, empty: &str) -> String {
    if data.first_name.is_empty() && data.last_name.is_empty() {
        empty.to_string()
    } else {
        format!("{} {}", data.first_name, data.last_name)
    }
}

fn write_userpic_thumb(
    base_path: &str,
    large_path: &str,
    userpic: &UserpicData,
    postfix: &str,
) -> String {
    write_image_thumb_exact(
        base_path,
        large_path,
        userpic.pixel_size * 2,
        userpic.pixel_size * 2,
        postfix,
    )
}

fn prepare_audio_media_data(data: &Document) -> MediaData {
    let has_file = !data.file.relative_path.is_empty();
    let title = if !data.song_performer.is_empty() && !data.song_title.is_empty() {
        format!("{} \u{2013} {}", data.song_performer, data.song_title)
    } else if !data.name.is_empty() {
        data.name.clone()
    } else {
        "Audio file".to_string()
    };
    let mut status = format_duration(i64::from(data.duration));
    if !has_file {
        status.push_str(", ");
        status.push_str(&format_file_size(data.file.size));
    }
    MediaData {
        title,
        description: no_file_description(data.file.skip_reason).to_string(),
        status,
        classes: "media_audio_file".to_string(),
        thumb: String::new(),
        link: data.file.relative_path.clone(),
    }
}

fn prepare_file_media_data(data: &Document) -> MediaData {
    MediaData {
        title: if data.name.is_empty() {
            "File".to_string()
        } else {
            data.name.clone()
        },
        description: no_file_description(data.file.skip_reason).to_string(),
        status: format_file_size(data.file.size),
        classes: "media_file".to_string(),
        thumb: String::new(),
        link: data.file.relative_path.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    Service,
    Default,
}

#[derive(Debug, Clone)]
struct MessageInfo {
    id: i32,
    kind: MessageKind,
    from_id: PeerId,
    via_bot_id: UserId,
    date: TimeId,
    forwarded_from_id: PeerId,
    forwarded_from_name: String,
    forwarded: bool,
    show_forwarded_as_original: bool,
    forwarded_date: TimeId,
}

impl Default for MessageInfo {
    fn default() -> Self {
        Self {
            id: 0,
            kind: MessageKind::Service,
            from_id: PeerId::ZERO,
            via_bot_id: UserId(0),
            date: 0,
            forwarded_from_id: PeerId::ZERO,
            forwarded_from_name: String::new(),
            forwarded: false,
            show_forwarded_as_original: false,
            forwarded_date: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RichTailState {
    Empty,
    Media,
    Other,
}

fn rich_block_attributes(classes: &str, kind: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("class", format!("rich_block {classes}")),
        ("data-rich-kind", kind.to_string()),
    ])
}

fn rich_heading_tag(level: i32) -> &'static str {
    match level {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        _ => "h6",
    }
}

fn rich_bool(v: bool) -> String {
    if v { "true" } else { "false" }.to_string()
}

fn rich_button_metadata(value: &str) -> String {
    percent_encode_url(value.as_bytes())
}

fn rich_button_peer_types(types: &[InlineButtonPeerType]) -> String {
    let mut result = String::from("[");
    for (idx, pt) in types.iter().enumerate() {
        if idx > 0 {
            result.push(',');
        }
        result.push('"');
        result.push_str(pt.as_str());
        result.push('"');
    }
    result.push(']');
    rich_button_metadata(&result)
}

fn rich_button_attributes(
    action: &InlineButtonAction,
    style: Option<RichButtonStyle>,
    inline_button: bool,
) -> BTreeMap<&'static str, String> {
    let resolved_style = style.unwrap_or(RichButtonStyle::Default);
    let style_name = resolved_style.as_str();
    let classes = format!(
        "rich_button {} rich_button_style_{style_name} rich_button_action_{}",
        if inline_button {
            "rich_button_inline"
        } else {
            "rich_button_block"
        },
        action.type_str()
    );
    let mut result = BTreeMap::from([
        ("class", classes),
        ("data-button-type", action.type_str().to_string()),
        ("inline", String::new()),
    ]);
    if style.is_some() {
        result.insert("data-button-style", style_name.to_string());
    }
    match action.kind {
        InlineButtonActionType::Url
        | InlineButtonActionType::Auth
        | InlineButtonActionType::WebView => {
            result.insert("data-button-url", rich_button_metadata(&action.url));
            if action.kind == InlineButtonActionType::Auth {
                if let Some(ref fwd) = action.forward_text {
                    result.insert("data-button-forward-text", rich_button_metadata(fwd));
                }
                result.insert("data-button-id", action.button_id.to_string());
            }
        }
        InlineButtonActionType::Callback | InlineButtonActionType::CallbackWithPassword => {
            result.insert(
                "data-callback-data-base64",
                base64_url_no_pad(&action.callback_data),
            );
            result.insert(
                "data-button-requires-password",
                rich_bool(action.requires_password),
            );
        }
        InlineButtonActionType::Game
        | InlineButtonActionType::Buy
        | InlineButtonActionType::Disabled => {}
        InlineButtonActionType::SwitchInline | InlineButtonActionType::SwitchInlineSame => {
            result.insert("data-button-query", rich_button_metadata(&action.query));
            result.insert("data-button-same-peer", rich_bool(action.same_peer));
            if let Some(ref peer_types) = action.peer_types {
                result.insert("data-button-peer-types", rich_button_peer_types(peer_types));
            }
        }
        InlineButtonActionType::UserProfile => {
            result.insert("data-button-user-id", action.user_id.to_string());
        }
        InlineButtonActionType::CopyText => {
            result.insert("data-copy-text", rich_button_metadata(&action.copy_text));
        }
    }
    if action.kind == InlineButtonActionType::Disabled {
        result.insert("aria-disabled", "true".to_string());
    }
    result
}

fn rich_text_has_output(text: &RichText) -> bool {
    match text.kind {
        RichTextType::Empty => false,
        RichTextType::Plain => !text.text.is_empty(),
        RichTextType::Concat => text.children.iter().any(rich_text_has_output),
        _ => true,
    }
}

fn rich_caption_has_output(caption: &RichCaption) -> bool {
    rich_text_has_output(&caption.text) || rich_text_has_output(&caption.credit)
}

fn append_plain_target(result: &mut String, text: &RichText) -> bool {
    match text.kind {
        RichTextType::Empty => true,
        RichTextType::Plain | RichTextType::CustomEmoji => {
            if result.len() + text.text.len() > RICH_TARGET_MAX_BYTES {
                return false;
            }
            result.push_str(&text.text);
            true
        }
        RichTextType::Button | RichTextType::Math | RichTextType::InlineImage => false,
        _ => text
            .children
            .iter()
            .all(|child| append_plain_target(result, child)),
    }
}

fn plain_target(text: &RichText) -> Option<String> {
    let mut out = String::new();
    append_plain_target(&mut out, text).then_some(out)
}

fn anchor_token(name: &str) -> String {
    if name.is_empty() {
        return "empty".to_string();
    }
    let bytes = name.as_bytes();
    if bytes.len() <= RICH_ANCHOR_DIRECT_BYTES {
        return base64_url_no_pad(bytes);
    }
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{hex}",
        base64_url_no_pad(&bytes[..RICH_ANCHOR_PREFIX_BYTES])
    )
}

fn rich_channel_source_name(source: RichChannelSource) -> &'static str {
    match source {
        RichChannelSource::ChatEmpty => "chat_empty",
        RichChannelSource::Chat => "chat",
        RichChannelSource::ChatForbidden => "chat_forbidden",
        RichChannelSource::Channel => "channel",
        RichChannelSource::ChannelForbidden => "channel_forbidden",
        RichChannelSource::Community => "community",
        RichChannelSource::CommunityForbidden => "community_forbidden",
    }
}

fn rich_channel_status(channel: &RichChannel) -> String {
    let label = match channel.source {
        RichChannelSource::ChatEmpty | RichChannelSource::Chat => "Chat",
        RichChannelSource::ChatForbidden => "Chat unavailable",
        RichChannelSource::Channel => "Channel",
        RichChannelSource::ChannelForbidden => "Channel unavailable",
        RichChannelSource::Community => "Community",
        RichChannelSource::CommunityForbidden => "Community unavailable",
    };
    if let Some(ref username) = channel.username
        && !username.is_empty()
    {
        format!("@{username}, {label}")
    } else {
        label.to_string()
    }
}

fn rich_map_point_source_name(source: RichMapPointSource) -> &'static str {
    match source {
        RichMapPointSource::GeoPointEmpty => "geo_point_empty",
        RichMapPointSource::GeoPoint => "geo_point",
        RichMapPointSource::InputGeoPointEmpty => "input_geo_point_empty",
        RichMapPointSource::InputGeoPoint => "input_geo_point",
    }
}

fn rich_map_coordinates(point: &RichMapPoint) -> Option<(String, String)> {
    if matches!(
        point.source,
        RichMapPointSource::GeoPointEmpty | RichMapPointSource::InputGeoPointEmpty
    ) {
        return None;
    }
    if !point.latitude.is_finite()
        || !point.longitude.is_finite()
        || !(-90.0..=90.0).contains(&point.latitude)
        || !(-180.0..=180.0).contains(&point.longitude)
    {
        return None;
    }
    Some((
        crate::data::float_to_string(point.latitude),
        crate::data::float_to_string(point.longitude),
    ))
}

struct RichHtmlRenderer<'a> {
    wrap: &'a mut HtmlWrap,
    message: &'a RichMessage,
    message_id: i32,
    link_active: bool,
    internal_links_domain: &'a str,
    base_path: &'a str,
    first_anchor_ids: BTreeMap<String, String>,
    anchor_occurrences: BTreeMap<String, i32>,
    text_anchor_ids: BTreeMap<usize, String>,
    block_anchor_ids: BTreeMap<usize, String>,
}

impl<'a> RichHtmlRenderer<'a> {
    fn new(
        wrap: &'a mut HtmlWrap,
        message: &'a RichMessage,
        message_id: i32,
        internal_links_domain: &'a str,
        base_path: &'a str,
    ) -> Self {
        Self {
            wrap,
            message,
            message_id,
            link_active: false,
            internal_links_domain,
            base_path,
            first_anchor_ids: BTreeMap::new(),
            anchor_occurrences: BTreeMap::new(),
            text_anchor_ids: BTreeMap::new(),
            block_anchor_ids: BTreeMap::new(),
        }
    }

    fn render_message(&mut self) -> String {
        self.collect_anchors();
        let mut result = self.wrap.push_tag(
            "div",
            BTreeMap::from([
                ("class", "text rich_message".to_string()),
                ("data-rich-message", self.message_id.to_string()),
                ("data-rich-part", rich_bool(self.message.part)),
                (
                    "dir",
                    if self.message.rtl { "rtl" } else { "auto" }.to_string(),
                ),
                ("inline", String::new()),
            ]),
        );
        result.push_str(&self.render_blocks(&self.message.blocks));
        result.push_str(&self.wrap.pop_tag());
        result
    }

    fn collect_anchors(&mut self) {
        self.first_anchor_ids.clear();
        self.anchor_occurrences.clear();
        self.text_anchor_ids.clear();
        self.block_anchor_ids.clear();
        self.collect_block_anchors_slice(&self.message.blocks);
    }

    fn register_anchor(&mut self, name: &str) -> String {
        let base = format!(
            "rich-message-{}-anchor-{}",
            self.message_id,
            anchor_token(name)
        );
        let occ = self.anchor_occurrences.entry(base.clone()).or_insert(0);
        *occ += 1;
        let result = if *occ > 1 {
            format!("{base}-{occ}")
        } else {
            base
        };
        self.first_anchor_ids
            .entry(name.to_string())
            .or_insert_with(|| result.clone());
        result
    }

    fn collect_text_anchors(&mut self, text: &RichText) {
        if text.kind == RichTextType::Anchor {
            let id = self.register_anchor(&text.data);
            self.text_anchor_ids
                .insert(text as *const RichText as usize, id);
        }
        for child in &text.children {
            self.collect_text_anchors(child);
        }
        for old in &text.old_children {
            self.collect_text_anchors(old);
        }
    }

    fn collect_block_anchors_slice(&mut self, blocks: &[RichBlock]) {
        for block in blocks {
            self.collect_block_anchors(block);
        }
    }

    fn collect_block_anchors(&mut self, block: &RichBlock) {
        match block.kind {
            RichBlockKind::Heading
            | RichBlockKind::Paragraph
            | RichBlockKind::Footer
            | RichBlockKind::Thinking
            | RichBlockKind::AuthorDate
            | RichBlockKind::Code
            | RichBlockKind::RelatedArticles => {
                self.collect_text_anchors(&block.text);
            }
            RichBlockKind::Anchor => {
                let id = self.register_anchor(&block.name);
                self.block_anchor_ids
                    .insert(block as *const RichBlock as usize, id);
            }
            RichBlockKind::List => {
                for item in &block.list_items {
                    match item.content {
                        RichListItemContent::Text => {
                            if let Some(ref t) = item.text {
                                self.collect_text_anchors(t);
                            }
                        }
                        RichListItemContent::Blocks => {
                            self.collect_block_anchors_slice(&item.blocks);
                        }
                    }
                }
            }
            RichBlockKind::Quote => {
                match block.quote_content {
                    RichQuoteContent::Text => self.collect_text_anchors(&block.text),
                    RichQuoteContent::Blocks => self.collect_block_anchors_slice(&block.blocks),
                }
                self.collect_text_anchors(&block.quote_caption);
            }
            RichBlockKind::Photo
            | RichBlockKind::Video
            | RichBlockKind::Audio
            | RichBlockKind::File
            | RichBlockKind::Embed
            | RichBlockKind::Map
            | RichBlockKind::InputMap => {
                self.collect_text_anchors(&block.caption.text);
                self.collect_text_anchors(&block.caption.credit);
            }
            RichBlockKind::EmbedPost | RichBlockKind::Collage | RichBlockKind::Slideshow => {
                self.collect_block_anchors_slice(&block.blocks);
                self.collect_text_anchors(&block.caption.text);
                self.collect_text_anchors(&block.caption.credit);
            }
            RichBlockKind::Cover => {
                self.collect_block_anchors_slice(&block.blocks);
            }
            RichBlockKind::Table => {
                self.collect_text_anchors(&block.text);
                for row in &block.table_rows {
                    for cell in &row.cells {
                        if let Some(ref t) = cell.text {
                            self.collect_text_anchors(t);
                        }
                    }
                }
            }
            RichBlockKind::Details => {
                self.collect_text_anchors(&block.text);
                self.collect_block_anchors_slice(&block.blocks);
            }
            RichBlockKind::ButtonRow => {
                for btn in &block.buttons {
                    self.collect_text_anchors(btn);
                }
            }
            RichBlockKind::Unsupported
            | RichBlockKind::Divider
            | RichBlockKind::Channel
            | RichBlockKind::Math
            | RichBlockKind::Unknown => {}
        }
    }

    fn fragment_href(&self, target: &str) -> Option<String> {
        if !is_strict_target(target) {
            return None;
        }
        let name = target.strip_prefix('#')?;
        self.first_anchor_ids.get(name).map(|id| format!("#{id}"))
    }

    fn media_item_size(&self, block: &RichBlock) -> Option<Size> {
        match block.kind {
            RichBlockKind::Photo => {
                let photo = self.message.photos.get(&block.photo_id)?;
                if !photo.image.file.relative_path.is_empty() {
                    let s = Size::new(photo.image.width, photo.image.height);
                    (!s.is_empty()).then_some(s)
                } else {
                    None
                }
            }
            RichBlockKind::Video => {
                let doc = self.message.documents.get(&block.document_id)?;
                if !doc.file.relative_path.is_empty() && !doc.thumb.file.relative_path.is_empty() {
                    let s = Size::new(doc.width, doc.height);
                    (!s.is_empty()).then_some(s)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn media_group_sizes(&self, blocks: &[RichBlock]) -> Vec<Size> {
        if blocks.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(blocks.len());
        for block in blocks {
            let Some(size) = self.media_item_size(block) else {
                return Vec::new();
            };
            if rich_caption_has_output(&block.caption)
                || block.optional_url.as_ref().is_some_and(|u| !u.is_empty())
            {
                return Vec::new();
            }
            out.push(size);
        }
        out
    }

    fn tail_state_slice(&self, blocks: &[RichBlock]) -> RichTailState {
        for block in blocks.iter().rev() {
            let state = self.tail_state(block);
            if state != RichTailState::Empty {
                return state;
            }
        }
        RichTailState::Empty
    }

    fn tail_state(&self, block: &RichBlock) -> RichTailState {
        match block.kind {
            RichBlockKind::Anchor => RichTailState::Empty,
            RichBlockKind::List => {
                let Some(item) = block.list_items.last() else {
                    return RichTailState::Empty;
                };
                if item.content == RichListItemContent::Blocks {
                    let st = self.tail_state_slice(&item.blocks);
                    if st != RichTailState::Empty {
                        return st;
                    }
                }
                RichTailState::Other
            }
            RichBlockKind::Quote => {
                if rich_text_has_output(&block.quote_caption) {
                    return RichTailState::Other;
                }
                if block.quote_content == RichQuoteContent::Blocks {
                    let st = self.tail_state_slice(&block.blocks);
                    if st != RichTailState::Empty {
                        return st;
                    }
                }
                RichTailState::Other
            }
            RichBlockKind::Photo => {
                if rich_caption_has_output(&block.caption)
                    || block.optional_url.as_ref().is_some_and(|u| !u.is_empty())
                {
                    RichTailState::Other
                } else {
                    RichTailState::Media
                }
            }
            RichBlockKind::Video
            | RichBlockKind::Audio
            | RichBlockKind::File
            | RichBlockKind::Map
            | RichBlockKind::InputMap => {
                if rich_caption_has_output(&block.caption) {
                    RichTailState::Other
                } else {
                    RichTailState::Media
                }
            }
            RichBlockKind::Cover => self.tail_state_slice(&block.blocks),
            RichBlockKind::Embed => {
                if rich_caption_has_output(&block.caption)
                    || (block.poster_photo_id.is_some()
                        && block.optional_url.as_ref().is_some_and(|u| !u.is_empty()))
                {
                    RichTailState::Other
                } else {
                    RichTailState::Media
                }
            }
            RichBlockKind::EmbedPost => {
                if rich_caption_has_output(&block.caption) {
                    return RichTailState::Other;
                }
                let st = self.tail_state_slice(&block.blocks);
                if st != RichTailState::Empty {
                    return st;
                }
                if block.url.is_empty() {
                    RichTailState::Media
                } else {
                    RichTailState::Other
                }
            }
            RichBlockKind::Collage | RichBlockKind::Slideshow => {
                if rich_caption_has_output(&block.caption) {
                    RichTailState::Other
                } else {
                    self.tail_state_slice(&block.blocks)
                }
            }
            RichBlockKind::Channel => RichTailState::Media,
            RichBlockKind::Details => {
                if block.open {
                    let st = self.tail_state_slice(&block.blocks);
                    if st != RichTailState::Empty {
                        return st;
                    }
                }
                RichTailState::Other
            }
            RichBlockKind::RelatedArticles => {
                if let Some(article) = block.related_articles.last() {
                    if !article.url.is_empty() {
                        return RichTailState::Other;
                    }
                    let desc = article.description.as_deref().unwrap_or("");
                    if let Some(pid) = article.photo_id
                        && !desc.is_empty()
                    {
                        let photo = self.message.photos.get(&pid);
                        if !rich_file_description(photo.map(|p| &p.image.file)).is_empty() {
                            return RichTailState::Other;
                        }
                    }
                    RichTailState::Media
                } else if rich_text_has_output(&block.text) {
                    RichTailState::Other
                } else {
                    RichTailState::Empty
                }
            }
            _ => RichTailState::Other,
        }
    }

    fn render_texts(&mut self, texts: &[RichText]) -> String {
        let mut out = String::new();
        for t in texts {
            out.push_str(&self.render_text(t));
        }
        out
    }

    fn render_text_children(&mut self, text: &RichText) -> String {
        if !text.children.is_empty() {
            return self.render_texts(&text.children);
        }
        let mut result = self.wrap.push_tag(
            "span",
            BTreeMap::from([
                ("class", "rich_inline_fallback".to_string()),
                ("data-rich-kind", "unsupported-text".to_string()),
                ("inline", String::new()),
            ]),
        );
        result.push_str("Unsupported rich text");
        result.push_str(&self.wrap.pop_tag());
        result
    }

    fn render_text_link(
        &mut self,
        text: &RichText,
        href: Option<String>,
        mut attributes: BTreeMap<&'static str, String>,
    ) -> String {
        attributes.insert("inline", String::new());
        if href.is_none() || self.link_active {
            attributes.remove("href");
            attributes.remove("onclick");
            attributes.insert("class", "rich_inert_link".to_string());
            let mut result = self.wrap.push_tag("span", attributes);
            result.push_str(&self.render_text_children(text));
            result.push_str(&self.wrap.pop_tag());
            return result;
        }
        if let Some(h) = href {
            attributes.insert("href", h);
        }
        let mut result = self.wrap.push_tag("a", attributes);
        let prev = self.link_active;
        self.link_active = true;
        result.push_str(&self.render_text_children(text));
        self.link_active = prev;
        result.push_str(&self.wrap.pop_tag());
        result
    }

    fn render_button(&mut self, button: &RichText, inline_button: bool) -> String {
        let Some(ref payload) = button.button else {
            return String::new();
        };
        let action = &payload.action;
        let mut attributes = rich_button_attributes(action, payload.style, inline_button);
        let mut tag = "span";
        let nested = self.link_active;
        let url = if action.kind == InlineButtonActionType::Url {
            safe_message_href(&action.url)
        } else {
            None
        };
        if !nested && let Some(href) = url {
            tag = "a";
            attributes.insert("href", href);
        } else if !nested && action.kind == InlineButtonActionType::CopyText {
            tag = "button";
            attributes.insert("type", "button".to_string());
            attributes.insert(
                "onclick",
                "return ShowTextCopied(decodeURIComponent(this.dataset.copyText))".to_string(),
            );
        }
        let mut result = self.wrap.push_tag(tag, attributes);
        let prev = self.link_active;
        self.link_active = true;
        if let Some(first) = button.children.first() {
            result.push_str(&self.render_text(first));
        }
        self.link_active = prev;
        result.push_str(&self.wrap.pop_tag());
        result
    }

    fn render_custom_emoji_link(
        &mut self,
        alt: &str,
        mut attributes: BTreeMap<&'static str, String>,
    ) -> String {
        attributes.insert("inline", String::new());
        if self.link_active {
            attributes.remove("href");
            attributes.remove("onclick");
            attributes.insert("class", "rich_inert_link".to_string());
            let mut result = self.wrap.push_tag("span", attributes);
            result.push_str(alt);
            result.push_str(&self.wrap.pop_tag());
            return result;
        }
        let mut result = self.wrap.push_tag("a", attributes);
        let prev = self.link_active;
        self.link_active = true;
        result.push_str(alt);
        self.link_active = prev;
        result.push_str(&self.wrap.pop_tag());
        result
    }

    fn render_text(&mut self, text: &RichText) -> String {
        let wrap_children =
            |this: &mut Self, tag: &str, mut attrs: BTreeMap<&'static str, String>| {
                attrs.insert("inline", String::new());
                let mut res = this.wrap.push_tag(tag, attrs);
                res.push_str(&this.render_text_children(text));
                res.push_str(&this.wrap.pop_tag());
                res
            };
        match text.kind {
            RichTextType::Empty => String::new(),
            RichTextType::Plain => serialize_html_string(&text.text),
            RichTextType::Concat => self.render_texts(&text.children),
            RichTextType::Bold => wrap_children(self, "strong", BTreeMap::new()),
            RichTextType::Italic => wrap_children(self, "em", BTreeMap::new()),
            RichTextType::Underline => wrap_children(self, "u", BTreeMap::new()),
            RichTextType::Strike => wrap_children(self, "s", BTreeMap::new()),
            RichTextType::Fixed => wrap_children(
                self,
                "code",
                BTreeMap::from([("class", "rich_inline_code".to_string())]),
            ),
            RichTextType::Url => {
                let href = if text.data.starts_with('#') {
                    self.fragment_href(&text.data)
                } else {
                    safe_http_href(&text.data)
                };
                let mut attrs = BTreeMap::new();
                if text.id != 0 {
                    attrs.insert("data-webpage-id", text.id.to_string());
                }
                self.render_text_link(text, href, attrs)
            }
            RichTextType::Email => {
                let href = safe_email_href(&text.data);
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::Phone => {
                let href = safe_phone_href(&text.data);
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::Subscript => wrap_children(self, "sub", BTreeMap::new()),
            RichTextType::Superscript => wrap_children(self, "sup", BTreeMap::new()),
            RichTextType::Marked => wrap_children(self, "mark", BTreeMap::new()),
            RichTextType::Anchor => {
                let mut attrs = BTreeMap::from([
                    ("class", "rich_reference".to_string()),
                    ("inline", String::new()),
                ]);
                if let Some(id) = self
                    .text_anchor_ids
                    .get(&(text as *const RichText as usize))
                {
                    attrs.insert("id", id.clone());
                }
                let mut res = self.wrap.push_tag("span", attrs);
                res.push_str(&self.render_texts(&text.children));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::Math => {
                let mut res = self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "rich_math_inline".to_string()),
                        ("data-rich-kind", "inline-math".to_string()),
                        ("dir", "ltr".to_string()),
                        ("inline", String::new()),
                    ]),
                );
                res.push_str(&serialize_html_string(&text.data));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::CustomEmoji => {
                let mut res = self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "rich_custom_emoji".to_string()),
                        ("data-document-id", text.id.to_string()),
                        ("inline", String::new()),
                    ]),
                );
                let alt = serialize_html_string(&text.text);
                let unavailable = text.custom_emoji_data == TextPart::UNAVAILABLE_EMOJI;
                let not_loaded =
                    text.custom_emoji_data.is_empty() || is_ascii_decimal(&text.custom_emoji_data);
                let href = if unavailable || not_loaded {
                    None
                } else {
                    safe_relative_emoji_href(&text.custom_emoji_data, &self.wrap.base)
                };
                if let Some(h) = href {
                    res.push_str(
                        &self.render_custom_emoji_link(&alt, BTreeMap::from([("href", h)])),
                    );
                } else if unavailable || not_loaded {
                    let onclick = if unavailable {
                        "return ShowNotAvailableEmoji()"
                    } else {
                        "return ShowNotLoadedEmoji()"
                    };
                    res.push_str(&self.render_custom_emoji_link(
                        &alt,
                        BTreeMap::from([("href", String::new()), ("onclick", onclick.to_string())]),
                    ));
                } else {
                    res.push_str(&alt);
                }
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::Spoiler => {
                let mut res = self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "spoiler hidden".to_string()),
                        ("inline", String::new()),
                        ("onclick", "ShowSpoiler(this)".to_string()),
                    ]),
                );
                res.push_str(&self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("aria-hidden", "true".to_string()),
                        ("inline", String::new()),
                    ]),
                ));
                res.push_str(&self.render_text_children(text));
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::Mention => {
                let href = plain_target(text)
                    .and_then(|t| safe_mention_href(&t, self.internal_links_domain));
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::Hashtag | RichTextType::BotCommand | RichTextType::Cashtag => {
                let prefix = match text.kind {
                    RichTextType::Hashtag => '#',
                    RichTextType::BotCommand => '/',
                    _ => '$',
                };
                let action = plain_target(text).and_then(|t| safe_action_data(&t, prefix));
                let Some(act) = action else {
                    return self.render_text_link(text, None, BTreeMap::new());
                };
                let (attr, cb) = match text.kind {
                    RichTextType::Hashtag => ("data-tag", "return ShowHashtag(this.dataset.tag)"),
                    RichTextType::BotCommand => (
                        "data-command",
                        "return ShowBotCommand(this.dataset.command)",
                    ),
                    _ => ("data-tag", "return ShowCashtag(this.dataset.tag)"),
                };
                self.render_text_link(
                    text,
                    Some(String::new()),
                    BTreeMap::from([(attr, act), ("onclick", cb.to_string())]),
                )
            }
            RichTextType::AutoUrl => {
                let href = plain_target(text).and_then(|t| safe_http_href(&t));
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::AutoEmail => {
                let href = plain_target(text).and_then(|t| safe_email_href(&t));
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::AutoPhone => {
                let href = plain_target(text).and_then(|t| safe_phone_href(&t));
                self.render_text_link(text, href, BTreeMap::new())
            }
            RichTextType::BankCard => wrap_children(
                self,
                "span",
                BTreeMap::from([("class", "rich_bank_card".to_string())]),
            ),
            RichTextType::TonAddress => wrap_children(
                self,
                "span",
                BTreeMap::from([("class", "rich_ton_address".to_string())]),
            ),
            RichTextType::MentionName => self.render_text_link(
                text,
                Some(String::new()),
                BTreeMap::from([
                    ("data-user-id", text.id.to_string()),
                    ("onclick", "return ShowMentionName()".to_string()),
                ]),
            ),
            RichTextType::FormattedDate => {
                let iso = format_iso_date_with_offset(text.date, self.wrap.utc_offset_seconds);
                let title = format_date_time_with_offset(
                    text.date,
                    self.wrap.utc_offset_seconds,
                    false,
                    '.',
                    ':',
                    ' ',
                );
                let mut res = self.wrap.push_tag(
                    "time",
                    BTreeMap::from([
                        ("class", "rich_formatted_date".to_string()),
                        ("data-date", text.date.to_string()),
                        ("data-day-of-week", rich_bool(text.day_of_week)),
                        ("data-long-date", rich_bool(text.long_date)),
                        ("data-long-time", rich_bool(text.long_time)),
                        ("data-relative", rich_bool(text.relative)),
                        ("data-short-date", rich_bool(text.short_date)),
                        ("data-short-time", rich_bool(text.short_time)),
                        ("datetime", iso),
                        ("dir", "ltr".to_string()),
                        ("inline", String::new()),
                        ("title", title),
                    ]),
                );
                res.push_str(&self.render_text_children(text));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::InlineImage => {
                let mut res = self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "rich_inline_fallback".to_string()),
                        ("data-document-id", text.id.to_string()),
                        ("data-rich-kind", "inline-image".to_string()),
                        ("data-source-height", text.height.to_string()),
                        ("data-source-width", text.width.to_string()),
                        ("inline", String::new()),
                    ]),
                );
                res.push_str("Inline image");
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::Diff => {
                let mut res = self.wrap.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "rich_diff".to_string()),
                        ("data-rich-kind", "diff".to_string()),
                        ("inline", String::new()),
                    ]),
                );
                res.push_str(&self.render_text_children(text));
                res.push_str(&self.wrap.push_tag(
                    "del",
                    BTreeMap::from([
                        ("class", "rich_diff_previous".to_string()),
                        ("inline", String::new()),
                    ]),
                ));
                res.push_str("Previous: ");
                if text.old_children.is_empty() {
                    res.push_str(&self.wrap.push_tag(
                        "span",
                        BTreeMap::from([
                            ("class", "rich_inline_fallback".to_string()),
                            ("data-rich-kind", "unsupported-text".to_string()),
                            ("inline", String::new()),
                        ]),
                    ));
                    res.push_str("Unsupported rich text");
                    res.push_str(&self.wrap.pop_tag());
                } else {
                    res.push_str(&self.render_texts(&text.old_children));
                }
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichTextType::Button => self.render_button(text, true),
        }
    }

    fn render_caption(&mut self, caption: &RichCaption) -> String {
        let text = self.render_text(&caption.text);
        let credit = self.render_text(&caption.credit);
        if text.is_empty() && credit.is_empty() {
            return String::new();
        }
        let mut res = self.wrap.push_tag(
            "figcaption",
            BTreeMap::from([
                ("class", "rich_media_caption".to_string()),
                ("dir", "auto".to_string()),
            ]),
        );
        if !text.is_empty() {
            res.push_str(&self.wrap.push_div("rich_caption_text", ""));
            res.push_str(&text);
            res.push_str(&self.wrap.pop_tag());
        }
        if !credit.is_empty() {
            res.push_str(&self.wrap.push_tag(
                "cite",
                BTreeMap::from([("class", "rich_caption_credit".to_string())]),
            ));
            res.push_str(&credit);
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_source_meta(&mut self, source: &str, embed_post: bool) -> String {
        if source.is_empty() {
            return String::new();
        }
        let class = if embed_post {
            "rich_embed_post_meta"
        } else {
            "rich_source_meta"
        };
        let mut res = self.wrap.push_div(class, "");
        if let Some(href) = safe_http_href(source)
            && !self.link_active
        {
            res.push_str(&self.wrap.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "rich_source_link".to_string()),
                    ("href", href),
                    ("inline", String::new()),
                ]),
            ));
            let prev = self.link_active;
            self.link_active = true;
            res.push_str(&serialize_html_string(source));
            self.link_active = prev;
            res.push_str(&self.wrap.pop_tag());
        } else {
            res.push_str(&self.wrap.push_tag(
                "span",
                BTreeMap::from([
                    ("class", "rich_source_text".to_string()),
                    ("inline", String::new()),
                ]),
            ));
            res.push_str(&serialize_html_string(source));
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_blocks(&mut self, blocks: &[RichBlock]) -> String {
        let mut out = String::new();
        for b in blocks {
            out.push_str(&self.render_block(b));
        }
        out
    }

    fn render_fallback(&mut self, kind: &str, label: &str) -> String {
        let mut res = self
            .wrap
            .push_tag("div", rich_block_attributes("rich_fallback", kind));
        res.push_str(&serialize_html_string(label));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_media_group(
        &mut self,
        block: &RichBlock,
        kind: &str,
        render_group_caption: bool,
    ) -> String {
        let mut attrs = rich_block_attributes("rich_media_group", kind);
        attrs.insert("data-rich-has-items", rich_bool(!block.blocks.is_empty()));
        attrs.insert(
            "data-rich-items-end-media",
            rich_bool(self.tail_state_slice(&block.blocks) == RichTailState::Media),
        );
        let mut res = self.wrap.push_tag("section", attrs);
        res.push_str(&self.wrap.push_div("rich_media_group_items", ""));
        res.push_str(&self.render_blocks(&block.blocks));
        res.push_str(&self.wrap.pop_tag());
        if render_group_caption {
            res.push_str(&self.render_caption(&block.caption));
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_block(&mut self, block: &RichBlock) -> String {
        match block.kind {
            RichBlockKind::Unsupported => {
                self.render_fallback("unsupported", "Unsupported rich content")
            }
            RichBlockKind::Unknown => self.render_fallback("unknown", "Unknown rich content"),
            RichBlockKind::Heading => {
                let mut attrs = rich_block_attributes("rich_heading", "heading");
                attrs.insert("data-level", block.heading_level.to_string());
                let mut res = self
                    .wrap
                    .push_tag(rich_heading_tag(block.heading_level), attrs);
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Paragraph => {
                let mut attrs = rich_block_attributes("rich_paragraph", "paragraph");
                attrs.insert("dir", "auto".to_string());
                let mut res = self.wrap.push_tag("p", attrs);
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Footer => {
                let mut attrs = rich_block_attributes("rich_footer", "footer");
                attrs.insert("dir", "auto".to_string());
                let mut res = self.wrap.push_tag("footer", attrs);
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Thinking => {
                let mut attrs = rich_block_attributes("rich_thinking", "thinking");
                attrs.insert("dir", "auto".to_string());
                let mut res = self.wrap.push_tag("p", attrs);
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::AuthorDate => {
                let mut attrs = rich_block_attributes("rich_author_date", "author-date");
                attrs.insert("dir", "auto".to_string());
                let mut res = self.wrap.push_tag("address", attrs);
                res.push_str(&self.render_text(&block.text));
                if block.date != 0 {
                    let iso = format_iso_date_with_offset(block.date, self.wrap.utc_offset_seconds);
                    res.push_str(" \u{2022} ");
                    res.push_str(&self.wrap.push_tag(
                        "time",
                        BTreeMap::from([
                            ("data-date", block.date.to_string()),
                            ("datetime", iso),
                            ("dir", "ltr".to_string()),
                            ("inline", String::new()),
                        ]),
                    ));
                    let formatted = format_date_time_with_offset(
                        block.date,
                        self.wrap.utc_offset_seconds,
                        false,
                        '.',
                        ':',
                        ' ',
                    );
                    res.push_str(&serialize_html_string(&formatted));
                    res.push_str(&self.wrap.pop_tag());
                }
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Code => {
                let mut attrs = rich_block_attributes("rich_code", "code");
                attrs.insert("dir", "auto".to_string());
                attrs.insert("inline", String::new());
                if !block.language.is_empty() {
                    attrs.insert("data-language", block.language.clone());
                }
                let mut res = self.wrap.push_tag("pre", attrs);
                res.push_str(&self.wrap.push_tag(
                    "code",
                    BTreeMap::from([
                        ("class", "rich_code_content".to_string()),
                        ("inline", String::new()),
                    ]),
                ));
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Divider => {
                let mut attrs = rich_block_attributes("rich_divider", "divider");
                attrs.insert("empty", String::new());
                self.wrap.push_tag("hr", attrs)
            }
            RichBlockKind::Anchor => {
                let Some(id) = self
                    .block_anchor_ids
                    .get(&(block as *const RichBlock as usize))
                    .cloned()
                else {
                    return self.render_fallback("malformed", "Malformed rich content");
                };
                let mut attrs = rich_block_attributes("rich_anchor", "anchor");
                attrs.insert("id", id);
                attrs.insert("inline", String::new());
                let mut res = self.wrap.push_tag("span", attrs);
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::List => self.render_list(block),
            RichBlockKind::Quote => self.render_quote(block),
            RichBlockKind::Photo => {
                let mut attrs = rich_block_attributes("rich_media_item", "photo");
                attrs.insert("data-photo-id", block.photo_id.to_string());
                attrs.insert("data-spoiler", rich_bool(block.spoiler));
                if let Some(wp) = block.optional_webpage_id {
                    attrs.insert("data-webpage-id", wp.to_string());
                }
                let mut res = self.wrap.push_tag("figure", attrs);
                let photo = self.message.photos.get(&block.photo_id);
                res.push_str(&self.wrap.push_rich_photo_media(photo, self.base_path));
                if let Some(ref url) = block.optional_url {
                    res.push_str(&self.render_source_meta(url, false));
                }
                res.push_str(&self.render_caption(&block.caption));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Video => {
                let mut attrs = rich_block_attributes("rich_media_item", "video");
                attrs.insert("data-document-id", block.document_id.to_string());
                attrs.insert("data-autoplay", rich_bool(block.autoplay));
                attrs.insert("data-loop", rich_bool(block.is_loop));
                attrs.insert("data-spoiler", rich_bool(block.spoiler));
                let mut res = self.wrap.push_tag("figure", attrs);
                let doc = self.message.documents.get(&block.document_id);
                res.push_str(&self.wrap.push_rich_video_media(doc, self.base_path));
                res.push_str(&self.render_caption(&block.caption));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Cover => self.render_media_group(block, "cover", false),
            RichBlockKind::Embed => self.render_embed(block),
            RichBlockKind::EmbedPost => self.render_embed_post(block),
            RichBlockKind::Collage => self.render_collage(block),
            RichBlockKind::Slideshow => self.render_slideshow(block),
            RichBlockKind::Channel => self.render_channel(block),
            RichBlockKind::Audio => {
                let mut attrs = rich_block_attributes("rich_media_item", "audio");
                attrs.insert("data-document-id", block.document_id.to_string());
                let mut res = self.wrap.push_tag("figure", attrs);
                let doc = self.message.documents.get(&block.document_id);
                res.push_str(&self.wrap.push_rich_audio_media(doc));
                res.push_str(&self.render_caption(&block.caption));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::File => {
                let mut attrs = rich_block_attributes("rich_media_item", "file");
                attrs.insert("data-document-id", block.document_id.to_string());
                let mut res = self.wrap.push_tag("figure", attrs);
                let doc = self.message.documents.get(&block.document_id);
                res.push_str(&self.wrap.push_rich_file_media(doc));
                res.push_str(&self.render_caption(&block.caption));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Math => {
                let mut attrs = rich_block_attributes("rich_math_display", "math");
                attrs.insert("dir", "ltr".to_string());
                attrs.insert("inline", String::new());
                let mut res = self.wrap.push_tag("div", attrs);
                res.push_str(&serialize_html_string(&block.formula));
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::Table => self.render_table(block),
            RichBlockKind::Details => {
                let mut attrs = rich_block_attributes("rich_details", "details");
                if block.open {
                    attrs.insert("open", String::new());
                }
                let mut res = self.wrap.push_tag("details", attrs);
                res.push_str(&self.wrap.push_tag("summary", BTreeMap::new()));
                res.push_str(&self.render_text(&block.text));
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.push_div("rich_details_body", ""));
                res.push_str(&self.render_blocks(&block.blocks));
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.pop_tag());
                res
            }
            RichBlockKind::RelatedArticles => self.render_related_articles(block),
            RichBlockKind::Map => self.render_map(block, "map"),
            RichBlockKind::InputMap => self.render_map(block, "input-map"),
            RichBlockKind::ButtonRow => {
                let alignment = block.button_alignment.as_str();
                let mut attrs = rich_block_attributes(
                    &format!("rich_button_row rich_button_align_{alignment}"),
                    "button-row",
                );
                attrs.insert("data-button-alignment", alignment.to_string());
                let mut res = self.wrap.push_tag("div", attrs);
                res.push_str(&self.wrap.push_div("rich_button_group", ""));
                for btn in &block.buttons {
                    res.push_str(&self.render_button(btn, false));
                }
                res.push_str(&self.wrap.pop_tag());
                res.push_str(&self.wrap.pop_tag());
                res
            }
        }
    }

    fn render_list(&mut self, block: &RichBlock) -> String {
        let ordered = block.list_kind == RichListKind::Ordered;
        let mut attrs =
            rich_block_attributes("rich_list", if ordered { "ordered-list" } else { "list" });
        if ordered {
            if block.ordered_list.reversed {
                attrs.insert("reversed", String::new());
            }
            if let Some(start) = block.ordered_list.start {
                attrs.insert("start", start.to_string());
            }
            if let Some(ref lt) = block.ordered_list.list_type {
                let native = matches!(lt.as_str(), "1" | "a" | "A" | "i" | "I");
                attrs.insert(if native { "type" } else { "data-list-type" }, lt.clone());
            }
        }
        let mut res = self.wrap.push_tag(if ordered { "ol" } else { "ul" }, attrs);
        for item in &block.list_items {
            res.push_str(&self.render_list_item(item, ordered));
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_list_item(&mut self, item: &RichListItem, ordered: bool) -> String {
        let task = item.task_state != RichTaskState::None;
        let checked = item.task_state == RichTaskState::Checked;
        let custom_marker = ordered && !task && item.num.as_ref().is_some_and(|n| !n.is_empty());
        let mut classes = String::from("rich_list_item");
        if task {
            classes.push_str(" rich_task_item");
        } else if custom_marker {
            classes.push_str(" rich_order_item");
        }
        let mut attrs = BTreeMap::from([
            ("class", classes),
            (
                "data-rich-content",
                match item.content {
                    RichListItemContent::Blocks => "blocks",
                    RichListItemContent::Text => "text",
                }
                .to_string(),
            ),
        ]);
        if let Some(ref num) = item.num {
            attrs.insert("data-num", num.clone());
        }
        if let Some(ref item_type) = item.item_type {
            attrs.insert("data-item-type", item_type.clone());
        }
        if let Some(val) = item.value {
            attrs.insert("value", val.to_string());
        }
        let mut res = self.wrap.push_tag("li", attrs);
        if task {
            res.push_str(&self.wrap.push_tag(
                "span",
                BTreeMap::from([
                    ("aria-checked", rich_bool(checked)),
                    ("class", "rich_task_marker".to_string()),
                    ("inline", String::new()),
                    ("role", "checkbox".to_string()),
                ]),
            ));
            res.push_str(if checked { "\u{2611}" } else { "\u{2610}" });
            res.push_str(&self.wrap.pop_tag());
        } else if custom_marker && let Some(ref num) = item.num {
            res.push_str(&self.wrap.push_tag(
                "span",
                BTreeMap::from([
                    ("class", "rich_order_marker".to_string()),
                    ("inline", String::new()),
                ]),
            ));
            res.push_str(&serialize_html_string(num));
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.push_tag(
            if item.content == RichListItemContent::Blocks {
                "div"
            } else {
                "span"
            },
            BTreeMap::from([
                ("class", "rich_list_content".to_string()),
                ("inline", String::new()),
            ]),
        ));
        match item.content {
            RichListItemContent::Text => {
                if let Some(ref t) = item.text {
                    res.push_str(&self.render_text(t));
                }
            }
            RichListItemContent::Blocks => {
                res.push_str(&self.render_blocks(&item.blocks));
            }
        }
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_quote(&mut self, block: &RichBlock) -> String {
        let kind = if block.pullquote {
            "pullquote"
        } else if block.quote_content == RichQuoteContent::Blocks {
            "quote-blocks"
        } else {
            "quote"
        };
        let classes = if block.pullquote {
            "rich_quote rich_pullquote"
        } else {
            "rich_quote"
        };
        let mut res = self.wrap.push_tag(
            if block.pullquote {
                "aside"
            } else {
                "blockquote"
            },
            rich_block_attributes(classes, kind),
        );
        res.push_str(&self.wrap.push_div("rich_quote_body", ""));
        match block.quote_content {
            RichQuoteContent::Text => res.push_str(&self.render_text(&block.text)),
            RichQuoteContent::Blocks => res.push_str(&self.render_blocks(&block.blocks)),
        }
        res.push_str(&self.wrap.pop_tag());
        let caption = self.render_text(&block.quote_caption);
        if !caption.is_empty() {
            res.push_str(&self.wrap.push_tag(
                "cite",
                BTreeMap::from([("class", "rich_quote_caption".to_string())]),
            ));
            res.push_str(&caption);
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_collage(&mut self, block: &RichBlock) -> String {
        let sizes = self.media_group_sizes(&block.blocks);
        if sizes.is_empty() {
            return self.render_media_group(block, "collage", true);
        }
        let layout =
            layout_media_group_geometry(&sizes, GROUP_WIDTH_MAX, GROUP_WIDTH_MIN, GROUP_SKIP);
        let mut full = Rect::default();
        for &part in &layout {
            full = rect_united(full, part);
        }
        if layout.len() != sizes.len() || full.is_empty() {
            return self.render_media_group(block, "collage", true);
        }
        let pct = |val: i32, total: i32| format!("{:.3}%", (val as f64) * 100.0 / (total as f64));
        let mut attrs = rich_block_attributes("rich_media_group", "collage");
        attrs.insert("data-rich-has-items", "true".to_string());
        attrs.insert("data-rich-items-end-media", "true".to_string());
        let mut res = self.wrap.push_tag("section", attrs);
        res.push_str(&self.wrap.push_div(
            "rich_collage_box",
            &format!("aspect-ratio: {} / {}", full.width, full.height),
        ));
        for (i, geometry) in layout.iter().enumerate() {
            res.push_str(&self.wrap.push_div(
                "rich_collage_item",
                &format!(
                    "left: {}; top: {}; width: {}; height: {}",
                    pct(geometry.x - full.x, full.width),
                    pct(geometry.y - full.y, full.height),
                    pct(geometry.width, full.width),
                    pct(geometry.height, full.height)
                ),
            ));
            res.push_str(&self.render_block(&block.blocks[i]));
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.render_caption(&block.caption));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_slideshow(&mut self, block: &RichBlock) -> String {
        let sizes = self.media_group_sizes(&block.blocks);
        if sizes.is_empty() {
            return self.render_media_group(block, "slideshow", true);
        }
        let mut widest = sizes[0];
        for &s in &sizes {
            if i64::from(s.width) * i64::from(widest.height)
                > i64::from(widest.width) * i64::from(s.height)
            {
                widest = s;
            }
        }
        let count = block.blocks.len();
        let controls = count > 1;
        let mut attrs = rich_block_attributes("rich_media_group", "slideshow");
        attrs.insert("data-rich-has-items", "true".to_string());
        attrs.insert("data-rich-items-end-media", "true".to_string());
        let mut res = self.wrap.push_tag("section", attrs);
        res.push_str(&self.wrap.push_div(
            "rich_slideshow_box",
            &format!("aspect-ratio: {} / {}", widest.width, widest.height),
        ));
        res.push_str(&self.wrap.push_div("rich_slideshow_track", ""));
        for item in &block.blocks {
            res.push_str(&self.wrap.push_div("rich_slide", ""));
            res.push_str(&self.render_block(item));
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        if controls {
            res.push_str(&self.wrap.push_tag(
                "button",
                BTreeMap::from([
                    ("class", "rich_slideshow_prev".to_string()),
                    ("type", "button".to_string()),
                    ("inline", String::new()),
                ]),
            ));
            res.push_str("&#8592;");
            res.push_str(&self.wrap.pop_tag());
            res.push_str(&self.wrap.push_tag(
                "button",
                BTreeMap::from([
                    ("class", "rich_slideshow_next".to_string()),
                    ("type", "button".to_string()),
                    ("inline", String::new()),
                ]),
            ));
            res.push_str("&#8594;");
            res.push_str(&self.wrap.pop_tag());
            res.push_str(&self.wrap.push_div("rich_slideshow_dots", ""));
            for _ in 0..count {
                res.push_str(&self.wrap.push_tag(
                    "button",
                    BTreeMap::from([
                        ("class", "rich_slideshow_dot".to_string()),
                        ("type", "button".to_string()),
                        ("inline", String::new()),
                    ]),
                ));
                res.push_str(&self.wrap.pop_tag());
            }
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.render_caption(&block.caption));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_embed(&mut self, block: &RichBlock) -> String {
        let has_poster = block.poster_photo_id.is_some();
        let has_meta = has_poster && block.optional_url.as_ref().is_some_and(|u| !u.is_empty());
        let mut attrs = rich_block_attributes("rich_media_item rich_embed", "embed");
        attrs.insert("data-full-width", rich_bool(block.full_width));
        attrs.insert("data-allow-scrolling", rich_bool(block.allow_scrolling));
        attrs.insert("data-rich-has-meta", rich_bool(has_meta));
        if let Some(pid) = block.poster_photo_id {
            attrs.insert("data-poster-photo-id", pid.to_string());
        }
        if let Some(w) = block.width {
            attrs.insert("data-source-width", w.to_string());
        }
        if let Some(h) = block.height {
            attrs.insert("data-source-height", h.to_string());
        }
        let mut res = self.wrap.push_tag("figure", attrs);
        if let Some(pid) = block.poster_photo_id {
            let photo = self.message.photos.get(&pid);
            res.push_str(&self.wrap.push_rich_photo_media(photo, self.base_path));
            if let Some(ref url) = block.optional_url {
                res.push_str(&self.render_source_meta(url, false));
            }
        } else {
            let mut media = MediaData {
                title: "Embed".to_string(),
                classes: "media_file".to_string(),
                ..Default::default()
            };
            if let Some(ref url) = block.optional_url {
                media.status = url.clone();
                if let Some(href) = safe_http_href(url) {
                    media.link = href;
                }
            }
            res.push_str(&self.wrap.push_generic_media(&media));
        }
        res.push_str(&self.render_caption(&block.caption));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_embed_post(&mut self, block: &RichBlock) -> String {
        let photo = self.message.photos.get(&block.author_photo_id);
        let has_meta = !block.url.is_empty();
        let mut attrs = rich_block_attributes("rich_embed_post", "embed-post");
        attrs.insert("data-webpage-id", block.webpage_id.to_string());
        attrs.insert("data-author-photo-id", block.author_photo_id.to_string());
        attrs.insert("data-date", block.date.to_string());
        attrs.insert("data-rich-has-meta", rich_bool(has_meta));
        attrs.insert("data-rich-has-items", rich_bool(!block.blocks.is_empty()));
        attrs.insert(
            "data-rich-items-end-media",
            rich_bool(self.tail_state_slice(&block.blocks) == RichTailState::Media),
        );
        let mut res = self.wrap.push_tag("section", attrs);
        res.push_str(&self.wrap.push_div("rich_embed_post_author", ""));
        let media = MediaData {
            title: if block.author.is_empty() {
                "Embed post".to_string()
            } else {
                block.author.clone()
            },
            description: rich_file_description(photo.map(|p| &p.image.file)).to_string(),
            status: if block.date != 0 {
                format_date_time_with_offset(
                    block.date,
                    self.wrap.utc_offset_seconds,
                    false,
                    '.',
                    ':',
                    ' ',
                )
            } else {
                String::new()
            },
            classes: "media_contact".to_string(),
            ..Default::default()
        };
        res.push_str(
            &self
                .wrap
                .push_rich_reference_media(media, photo, self.base_path),
        );
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.render_source_meta(&block.url, true));
        res.push_str(&self.wrap.push_div("rich_embed_post_blocks", ""));
        res.push_str(&self.render_blocks(&block.blocks));
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.render_caption(&block.caption));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_channel(&mut self, block: &RichBlock) -> String {
        let ch = &block.channel;
        let mut attrs = rich_block_attributes("rich_reference_block", "channel");
        attrs.insert(
            "data-channel-source",
            rich_channel_source_name(ch.source).to_string(),
        );
        attrs.insert("data-channel-id", ch.id.to_string());
        attrs.insert("data-broadcast", rich_bool(ch.broadcast));
        attrs.insert("data-megagroup", rich_bool(ch.megagroup));
        attrs.insert("data-monoforum", rich_bool(ch.monoforum));
        if let Some(ah) = ch.access_hash {
            attrs.insert("data-access-hash", ah.to_string());
        }
        let mut res = self.wrap.push_tag("section", attrs);
        let mut media = MediaData {
            title: ch
                .title
                .as_deref()
                .filter(|t| !t.is_empty())
                .unwrap_or("Channel")
                .to_string(),
            status: rich_channel_status(ch),
            classes: "media_contact".to_string(),
            ..Default::default()
        };
        if let Some(ref username) = ch.username
            && is_ascii_word(username, RICH_USERNAME_MIN_LENGTH, RICH_USERNAME_MAX_LENGTH)
        {
            let target = format!("{}{username}", self.internal_links_domain);
            if let Some(href) = safe_http_href(&target) {
                media.link = href;
            }
        }
        res.push_str(&self.wrap.push_generic_media(&media));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_map(&mut self, block: &RichBlock, kind: &str) -> String {
        let point = &block.map_point;
        let mut attrs = rich_block_attributes("rich_reference_block", kind);
        attrs.insert(
            "data-point-source",
            rich_map_point_source_name(point.source).to_string(),
        );
        attrs.insert("data-zoom", block.zoom.to_string());
        attrs.insert("data-source-width", block.map_width.to_string());
        attrs.insert("data-source-height", block.map_height.to_string());
        if let Some(ah) = point.access_hash {
            attrs.insert("data-access-hash", ah.to_string());
        }
        if let Some(acc) = point.accuracy_radius {
            attrs.insert("data-accuracy-radius", acc.to_string());
        }
        let mut res = self.wrap.push_tag("figure", attrs);
        let mut media = MediaData {
            title: "Location".to_string(),
            classes: "media_location".to_string(),
            ..Default::default()
        };
        if let Some((lat, lon)) = rich_map_coordinates(point) {
            let joined = format!("{lat},{lon}");
            media.status = format!("{lat}, {lon}");
            let target = format!("https://maps.google.com/maps?q={joined}&ll={joined}&z=16");
            if let Some(href) = safe_http_href(&target) {
                media.link = href;
            }
        }
        res.push_str(&self.wrap.push_generic_media(&media));
        res.push_str(&self.render_caption(&block.caption));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_related_articles(&mut self, block: &RichBlock) -> String {
        let title = self.render_text(&block.text);
        let mut res = self.wrap.push_tag(
            "section",
            rich_block_attributes("rich_related_articles", "related-articles"),
        );
        if !title.is_empty() {
            res.push_str(&self.wrap.push_tag(
                "div",
                BTreeMap::from([
                    ("class", "rich_related_title".to_string()),
                    ("dir", "auto".to_string()),
                ]),
            ));
            res.push_str(&title);
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.push_div("rich_related_items", ""));
        for (idx, article) in block.related_articles.iter().enumerate() {
            res.push_str(&self.render_related_article(article, idx as i32));
        }
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_related_article(&mut self, article: &RichRelatedArticle, index: i32) -> String {
        let mut attrs = BTreeMap::from([
            ("class", "rich_related_article".to_string()),
            ("data-rich-article-index", index.to_string()),
            ("data-webpage-id", article.webpage_id.to_string()),
        ]);
        if let Some(pid) = article.photo_id {
            attrs.insert("data-photo-id", pid.to_string());
        }
        if let Some(pd) = article.published_date {
            attrs.insert("data-published-date", pd.to_string());
        }
        let mut res = self.wrap.push_tag("article", attrs);
        let mut media = MediaData {
            title: article
                .title
                .as_deref()
                .filter(|t| !t.is_empty())
                .unwrap_or("Related article")
                .to_string(),
            ..Default::default()
        };
        if let Some(ref author) = article.author
            && !author.is_empty()
        {
            media.status = author.clone();
        }
        if let Some(pd) = article.published_date {
            let formatted = format_date_time_with_offset(
                pd,
                self.wrap.utc_offset_seconds,
                false,
                '.',
                ':',
                ' ',
            );
            if !formatted.is_empty() {
                if !media.status.is_empty() {
                    media.status.push_str(", ");
                }
                media.status.push_str(&formatted);
            }
        }
        let desc = article.description.as_deref().unwrap_or("");
        let mut displaced_description = false;
        if let Some(pid) = article.photo_id {
            let photo = self.message.photos.get(&pid);
            let file_desc = rich_file_description(photo.map(|p| &p.image.file));
            media.classes = "media_photo".to_string();
            media.description = if file_desc.is_empty() {
                desc.to_string()
            } else {
                file_desc.to_string()
            };
            displaced_description = !file_desc.is_empty() && !desc.is_empty();
            res.push_str(
                &self
                    .wrap
                    .push_rich_reference_media(media, photo, self.base_path),
            );
        } else {
            media.classes = "media_file".to_string();
            media.description = desc.to_string();
            res.push_str(&self.wrap.push_generic_media(&media));
        }
        if displaced_description {
            res.push_str(&self.wrap.push_div("rich_article_description", ""));
            res.push_str(&serialize_html_string(desc));
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.render_source_meta(&article.url, false));
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_table(&mut self, block: &RichBlock) -> String {
        let mut wrap_attrs = rich_block_attributes("rich_table_wrap", "table");
        wrap_attrs.insert("tabindex", "0".to_string());
        let mut res = self.wrap.push_tag("div", wrap_attrs);
        let mut classes = String::from("rich_table");
        if block.bordered {
            classes.push_str(" bordered");
        }
        if block.striped {
            classes.push_str(" striped");
        }
        if block.compact {
            classes.push_str(" compact");
        }
        res.push_str(
            &self
                .wrap
                .push_tag("table", BTreeMap::from([("class", classes)])),
        );
        let caption = self.render_text(&block.text);
        if !caption.is_empty() {
            res.push_str(&self.wrap.push_tag("caption", BTreeMap::new()));
            res.push_str(&caption);
            res.push_str(&self.wrap.pop_tag());
        }
        res.push_str(&self.wrap.push_tag("tbody", BTreeMap::new()));
        for row in &block.table_rows {
            res.push_str(&self.render_table_row(row));
        }
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.wrap.pop_tag());
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_table_row(&mut self, row: &RichTableRow) -> String {
        let mut res = self.wrap.push_tag("tr", BTreeMap::new());
        for cell in &row.cells {
            res.push_str(&self.render_table_cell(cell));
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }

    fn render_table_cell(&mut self, cell: &RichTableCell) -> String {
        let h_class = match cell.alignment {
            RichTableAlignment::Left => "rich_align_left",
            RichTableAlignment::Center => "rich_align_center",
            RichTableAlignment::Right => "rich_align_right",
        };
        let v_class = match cell.vertical_alignment {
            RichTableVerticalAlignment::Top => "rich_valign_top",
            RichTableVerticalAlignment::Middle => "rich_valign_middle",
            RichTableVerticalAlignment::Bottom => "rich_valign_bottom",
        };
        let mut attrs = BTreeMap::from([("class", format!("{h_class} {v_class}"))]);
        if let Some(source) = cell.colspan
            && source > 0
        {
            let span = source.min(RICH_TABLE_SPAN_MAX);
            attrs.insert("colspan", span.to_string());
            if span != source {
                attrs.insert("data-source-colspan", source.to_string());
            }
        }
        if let Some(source) = cell.rowspan
            && source > 0
        {
            let span = source.min(RICH_TABLE_SPAN_MAX);
            attrs.insert("rowspan", span.to_string());
            if span != source {
                attrs.insert("data-source-rowspan", source.to_string());
            }
        }
        let mut res = self
            .wrap
            .push_tag(if cell.header { "th" } else { "td" }, attrs);
        if let Some(ref t) = cell.text {
            res.push_str(&self.render_text(t));
        }
        res.push_str(&self.wrap.pop_tag());
        res
    }
}

/// Manages one output `.html` file, its relative base (`../` nesting), and tag context
/// (port of `HtmlWriter::Wrap`).
struct HtmlWrap {
    file: OutputFile,
    composed_start: String,
    closed: bool,
    base: String,
    context: HtmlContext,
    utc_offset_seconds: i32,
}

impl HtmlWrap {
    fn new(path: &str, base_dir: &str, utc_offset_seconds: i32) -> Self {
        let left = path.strip_prefix(base_dir).unwrap_or(path);
        let nesting = left.chars().filter(|&c| c == '/').count();
        let base = "../".repeat(nesting);
        let mut wrap = Self {
            file: OutputFile::new(path),
            composed_start: String::new(),
            closed: false,
            base,
            context: HtmlContext::default(),
            utc_offset_seconds,
        };
        wrap.composed_start = wrap.compose_start();
        wrap
    }

    fn compose_start(&mut self) -> String {
        let mut result = format!("<!DOCTYPE html>{}", self.push_tag("html", BTreeMap::new()));
        result.push_str(&self.push_tag("head", BTreeMap::new()));
        result.push_str(&self.push_tag(
            "meta",
            BTreeMap::from([("charset", "utf-8".to_string()), ("empty", String::new())]),
        ));
        result.push_str(&self.push_tag("title", BTreeMap::from([("inline", String::new())])));
        result.push_str("Exported Data");
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_tag(
            "meta",
            BTreeMap::from([
                ("name", "viewport".to_string()),
                (
                    "content",
                    "width=device-width, initial-scale=1.0".to_string(),
                ),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.push_tag(
            "link",
            BTreeMap::from([
                ("href", format!("{}css/style.css", self.base)),
                ("rel", "stylesheet".to_string()),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.push_tag(
            "script",
            BTreeMap::from([
                ("src", format!("{}js/script.js", self.base)),
                ("type", "text/javascript".to_string()),
            ]),
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_tag(
            "body",
            BTreeMap::from([("onload", "CheckLocation();".to_string())]),
        ));
        result.push_str(&self.push_div("page_wrap", ""));
        result
    }

    fn push_tag(&mut self, tag: &str, attributes: BTreeMap<&str, String>) -> String {
        self.context.push_tag(tag, attributes)
    }

    fn pop_tag(&mut self) -> String {
        self.context.pop_tag()
    }

    fn push_div(&mut self, class_name: &str, style: &str) -> String {
        if style.is_empty() {
            self.push_tag("div", BTreeMap::from([("class", class_name.to_string())]))
        } else {
            self.push_tag(
                "div",
                BTreeMap::from([
                    ("class", class_name.to_string()),
                    ("style", style.to_string()),
                ]),
            )
        }
    }

    fn relative_path(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn write_block(&mut self, block: &str, stats: Option<&Stats>) -> OutputResult {
        let res = if block.is_empty() {
            self.file.write_block(b"", stats)
        } else if self.file.is_empty() {
            let mut full = String::with_capacity(self.composed_start.len() + block.len());
            full.push_str(&self.composed_start);
            full.push_str(block);
            self.file.write_block(full.as_bytes(), stats)
        } else {
            self.file.write_block(block.as_bytes(), stats)
        };
        if res.is_error() {
            self.closed = true;
        }
        res
    }

    fn close(&mut self, stats: Option<&Stats>) -> OutputResult {
        if !self.closed {
            self.closed = true;
            if !self.file.is_empty() {
                let mut block = String::new();
                while !self.context.is_empty() {
                    block.push_str(&self.context.pop_tag());
                }
                return self.file.write_block(block.as_bytes(), stats);
            }
        }
        OutputResult::success()
    }

    fn push_header(&mut self, header: &str, path: &str) -> String {
        let mut result = self.push_div("page_header", "");
        if path.is_empty() {
            result.push_str(&self.push_div("content", ""));
        } else {
            let rel = self.relative_path(path);
            result.push_str(&self.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "content block_link".to_string()),
                    ("href", rel),
                    ("onclick", "return GoBack(this)".to_string()),
                ]),
            ));
        }
        result.push_str(&self.push_div("text bold", ""));
        result.push_str(&serialize_html_string(header));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_section(&mut self, header: &str, kind: &str, count: i32, link: &str) -> String {
        let mut result = self.push_tag(
            "a",
            BTreeMap::from([
                ("class", format!("section block_link {kind}")),
                ("href", format!("{link}#allow_back")),
            ]),
        );
        result.push_str(&self.push_div("counter details", ""));
        result.push_str(&count.to_string());
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("label bold", ""));
        result.push_str(&serialize_html_string(header));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_about(&mut self, text: &str, with_divider: bool) -> String {
        let class = if with_divider {
            "page_about details with_divider"
        } else {
            "page_about details"
        };
        let mut result = self.push_div(class, "");
        result.push_str(&make_links(&serialize_html_string(text)));
        result.push_str(&self.pop_tag());
        result
    }

    fn push_userpic(&mut self, userpic: &UserpicData) -> String {
        let size = format!("{}px", userpic.pixel_size);
        let mut result = String::new();
        if !userpic.large_link.is_empty() {
            let href = self.relative_path(&userpic.large_link);
            result.push_str(&self.push_tag(
                "a",
                BTreeMap::from([("class", "userpic_link".to_string()), ("href", href)]),
            ));
        }
        let size_style = format!("width: {size}; height: {size}");
        if !userpic.image_link.is_empty() {
            let src = self.relative_path(&userpic.image_link);
            result.push_str(&self.push_tag(
                "img",
                BTreeMap::from([
                    ("class", "userpic".to_string()),
                    ("style", size_style),
                    ("src", src),
                    ("empty", String::new()),
                ]),
            ));
        } else {
            result.push_str(&self.push_tag(
                "div",
                BTreeMap::from([
                    (
                        "class",
                        format!("userpic userpic{}", userpic.color_index + 1),
                    ),
                    ("style", size_style),
                ]),
            ));
            if userpic.tooltip.is_empty() {
                result.push_str(&self.push_div("initials", &format!("line-height: {size}")));
            } else {
                result.push_str(&self.push_tag(
                    "div",
                    BTreeMap::from([
                        ("class", "initials".to_string()),
                        ("style", format!("line-height: {size}")),
                        ("title", userpic.tooltip.clone()),
                    ]),
                ));
            }
            let first_char = |from: &str| -> String {
                from.trim()
                    .chars()
                    .next()
                    .map(|c| serialize_html_string(&c.to_string()))
                    .unwrap_or_default()
            };
            result.push_str(&first_char(&userpic.first_name));
            result.push_str(&first_char(&userpic.last_name));
            result.push_str(&self.pop_tag());
            result.push_str(&self.pop_tag());
        }
        if !userpic.large_link.is_empty() {
            result.push_str(&self.pop_tag());
        }
        result
    }

    fn push_generic_list_entry(
        &mut self,
        link: &str,
        userpic: &UserpicData,
        name: &str,
        subname: &str,
        details: &[String],
        info: &str,
    ) -> String {
        let mut result = if link.is_empty() {
            self.push_div("entry clearfix", "")
        } else {
            let href = format!("{}#allow_back", self.relative_path(link));
            self.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "entry block_link clearfix".to_string()),
                    ("href", href),
                ]),
            )
        };
        result.push_str(&self.push_div("pull_left userpic_wrap", ""));
        result.push_str(&self.push_userpic(userpic));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("body", ""));
        if !info.is_empty() {
            result.push_str(&self.push_div("pull_right info details", ""));
            result.push_str(&serialize_html_string(info));
            result.push_str(&self.pop_tag());
        }
        if !name.is_empty() {
            result.push_str(&self.push_div("name bold", ""));
            result.push_str(&serialize_html_string(name));
            result.push_str(&self.pop_tag());
        }
        if !subname.is_empty() {
            result.push_str(&self.push_div("subname bold", ""));
            result.push_str(&serialize_html_string(subname));
            result.push_str(&self.pop_tag());
        }
        for detail in details {
            result.push_str(&self.push_div("details_entry details", ""));
            result.push_str(&serialize_html_string(detail));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_list_entry(
        &mut self,
        userpic: &UserpicData,
        name: &str,
        details: &str,
        info: &str,
        link: &str,
    ) -> String {
        self.push_generic_list_entry(link, userpic, name, "", &[details.to_string()], info)
    }

    fn push_stories_list_entry(
        &mut self,
        story: &StoryData,
        name: &str,
        details: &[String],
        info: &str,
        caption: &[TextPart],
        internal_links_domain: &str,
    ) -> String {
        let link = story.link.as_str();
        let mut result = self.push_div("entry clearfix", "");
        if !link.is_empty() {
            let href = format!("{}#allow_back", self.relative_path(link));
            result.push_str(&self.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "pull_left userpic_wrap".to_string()),
                    ("href", href),
                ]),
            ));
        } else {
            result.push_str(&self.push_div("pull_left userpic_wrap", ""));
        }
        if !story.image_link.is_empty() {
            let src = self.relative_path(&story.image_link);
            result.push_str(&self.push_tag(
                "img",
                BTreeMap::from([
                    ("class", "story".to_string()),
                    (
                        "style",
                        format!("width: {STORY_THUMB_WIDTH}px; height: {STORY_THUMB_HEIGHT}px"),
                    ),
                    ("src", src),
                    ("empty", String::new()),
                ]),
            ));
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("body", ""));
        if !info.is_empty() {
            result.push_str(&self.push_div("pull_right info details", ""));
            result.push_str(&serialize_html_string(info));
            result.push_str(&self.pop_tag());
        }
        if !name.is_empty() {
            if !link.is_empty() {
                let href = format!("{}#allow_back", self.relative_path(link));
                result.push_str(&self.push_tag(
                    "a",
                    BTreeMap::from([("class", "block_link expanded".to_string()), ("href", href)]),
                ));
            }
            result.push_str(&self.push_div("name bold", ""));
            result.push_str(&serialize_html_string(name));
            result.push_str(&self.pop_tag());
            if !link.is_empty() {
                result.push_str(&self.pop_tag());
            }
        }
        if !caption.is_empty() {
            let text = format_html_text(caption, internal_links_domain, &self.base);
            if !text.is_empty() {
                result.push_str(&self.push_div("text", ""));
                result.push_str(&text);
                result.push_str(&self.pop_tag());
            }
        }
        for detail in details {
            result.push_str(&self.push_div("details_entry details", ""));
            result.push_str(&serialize_html_string(detail));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_audio_entry(
        &mut self,
        _name: &str,
        info: &str,
        details: &[String],
        duration: &str,
        link: &str,
    ) -> String {
        let mut result = self.push_div("entry clearfix", "");
        if !link.is_empty() {
            let href = format!("{}#allow_back", self.relative_path(link));
            result.push_str(&self.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "pull_left userpic_wrap".to_string()),
                    ("href", href),
                ]),
            ));
        } else {
            result.push_str(&self.push_div("pull_left userpic_wrap", ""));
        }
        result.push_str(&self.push_div("userpic audio_icon", ""));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("body", ""));
        if !duration.is_empty() {
            result.push_str(&self.push_div("pull_right info details", ""));
            result.push_str(&serialize_html_string(duration));
            result.push_str(&self.pop_tag());
        }
        if !info.is_empty() {
            if !link.is_empty() {
                let href = format!("{}#allow_back", self.relative_path(link));
                result.push_str(&self.push_tag(
                    "a",
                    BTreeMap::from([("class", "block_link expanded".to_string()), ("href", href)]),
                ));
            }
            result.push_str(&self.push_div("name bold", ""));
            result.push_str(&serialize_html_string(info));
            result.push_str(&self.pop_tag());
            if !link.is_empty() {
                result.push_str(&self.pop_tag());
            }
        }
        for detail in details {
            result.push_str(&self.push_div("details_entry details", ""));
            result.push_str(&serialize_html_string(detail));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_session_list_entry(
        &mut self,
        api_id: i32,
        name: &str,
        subname: &str,
        details: &[String],
        info: &str,
    ) -> String {
        let userpic = UserpicData {
            color_index: application_color_index(api_id),
            pixel_size: ENTRY_USERPIC_SIZE,
            first_name: name.to_string(),
            ..Default::default()
        };
        self.push_generic_list_entry("", &userpic, name, subname, details, info)
    }

    fn push_service_message(
        &mut self,
        message_id: i32,
        dialog: &DialogInfo,
        base_path: &str,
        serialized: &str,
        photo: Option<&Photo>,
    ) -> String {
        let mut result = self.push_tag(
            "div",
            BTreeMap::from([
                ("class", "message service".to_string()),
                ("id", format!("message{message_id}")),
            ]),
        );
        result.push_str(&self.push_div("body details", ""));
        result.push_str(serialized);
        result.push_str(&self.pop_tag());
        if let Some(p) = photo {
            let mut userpic = UserpicData {
                color_index: dialog.color_index,
                first_name: dialog.name.clone(),
                last_name: dialog.last_name.clone(),
                pixel_size: SERVICE_MESSAGE_PHOTO_SIZE,
                large_link: p.image.file.relative_path.clone(),
                ..Default::default()
            };
            userpic.image_link =
                write_userpic_thumb(base_path, &userpic.large_link, &userpic, "_thumb");
            result.push_str(&self.push_div("userpic_wrap", ""));
            result.push_str(&self.push_userpic(&userpic));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result
    }

    fn message_needs_wrap(&self, message: &Message, previous: Option<&MessageInfo>) -> bool {
        let Some(prev) = previous else {
            return true;
        };
        if prev.kind != MessageKind::Default {
            return true;
        }
        if message.from_id.is_zero() || prev.from_id != message.from_id {
            return true;
        }
        if message.via_bot_id != prev.via_bot_id {
            return true;
        }
        if display_date(message.date, prev.date, self.utc_offset_seconds) {
            return true;
        }
        if message.forwarded != prev.forwarded
            || message.show_forwarded_as_original != prev.show_forwarded_as_original
            || message.forwarded_from_id != prev.forwarded_from_id
            || message.forwarded_from_name != prev.forwarded_from_name
        {
            return true;
        }
        let limit =
            if !message.forwarded_from_id.is_zero() || !message.forwarded_from_name.is_empty() {
                1
            } else {
                JOIN_WITHIN_SECONDS
            };
        (message.date - prev.date).abs() > limit
    }

    fn forwarded_needs_wrap(&self, message: &Message, previous: Option<&MessageInfo>) -> bool {
        if self.message_needs_wrap(message, previous) {
            return true;
        }
        let Some(prev) = previous else {
            return true;
        };
        if message.forwarded_from_id.is_zero()
            || message.forwarded_from_id != prev.forwarded_from_id
        {
            return true;
        }
        if !message.forwarded_from_id.is_user() {
            return true;
        }
        (message.forwarded_date - prev.forwarded_date).abs() > JOIN_WITHIN_SECONDS
    }

    fn push_generic_media(&mut self, data: &MediaData) -> String {
        let mut result = self.push_div("media_wrap clearfix", "");
        if data.link.is_empty() {
            result.push_str(
                &self.push_div(&format!("media clearfix pull_left {}", data.classes), ""),
            );
        } else {
            let href = if is_global_link(&data.link) {
                data.link.clone()
            } else {
                self.relative_path(&data.link)
            };
            result.push_str(&self.push_tag(
                "a",
                BTreeMap::from([
                    (
                        "class",
                        format!("media clearfix pull_left block_link {}", data.classes),
                    ),
                    ("href", href),
                ]),
            ));
        }
        if data.thumb.is_empty() {
            result.push_str(&self.push_div("fill pull_left", ""));
            result.push_str(&self.pop_tag());
        } else {
            let src = self.relative_path(&data.thumb);
            result.push_str(&self.push_tag(
                "img",
                BTreeMap::from([
                    ("class", "thumb pull_left".to_string()),
                    ("src", src),
                    ("empty", String::new()),
                ]),
            ));
        }
        result.push_str(&self.push_div("body", ""));
        if !data.title.is_empty() {
            result.push_str(&self.push_div("title bold", ""));
            result.push_str(&serialize_html_string(&data.title));
            result.push_str(&self.pop_tag());
        }
        if !data.description.is_empty() {
            result.push_str(&self.push_div("description", ""));
            result.push_str(&serialize_html_string(&data.description));
            result.push_str(&self.pop_tag());
        }
        if !data.status.is_empty() {
            result.push_str(&self.push_div("status details", ""));
            result.push_str(&serialize_html_string(&data.status));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_sticker_media(&mut self, data: &Document, base_path: &str) -> String {
        let (thumb, size) = write_image_thumb(
            base_path,
            &data.file.relative_path,
            |s| calculate_thumb_size(s, STICKER_MAX_WIDTH, STICKER_MAX_HEIGHT, false),
            Some(ImageFormat::Png),
            "_thumb",
        );
        if thumb.is_empty() {
            let mut status = data.sticker_emoji.clone();
            let link = if data.file.relative_path.is_empty() {
                if !status.is_empty() {
                    status.push_str(", ");
                }
                status.push_str(&format_file_size(data.file.size));
                String::new()
            } else {
                data.file.relative_path.clone()
            };
            return self.push_generic_media(&MediaData {
                title: "Sticker".to_string(),
                status,
                link,
                description: no_file_description(data.file.skip_reason).to_string(),
                classes: "media_photo".to_string(),
                thumb: String::new(),
            });
        }
        let mut result = self.push_div("media_wrap clearfix", "");
        let href = self.relative_path(&data.file.relative_path);
        result.push_str(&self.push_tag(
            "a",
            BTreeMap::from([
                ("class", "sticker_wrap clearfix pull_left".to_string()),
                ("href", href),
            ]),
        ));
        let src = self.relative_path(&thumb);
        result.push_str(&self.push_tag(
            "img",
            BTreeMap::from([
                ("class", "sticker".to_string()),
                (
                    "style",
                    format!("width: {}px; height: {}px", size.width / 2, size.height / 2),
                ),
                ("src", src),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_animated_media(&mut self, data: &Document) -> String {
        let thumb_size = calculate_thumb_size(
            Size::new(data.width, data.height),
            PHOTO_MAX_WIDTH,
            PHOTO_MAX_HEIGHT,
            true,
        );
        if data.thumb.file.relative_path.is_empty()
            || data.file.relative_path.is_empty()
            || thumb_size.is_empty()
        {
            return self.push_generic_media(&MediaData {
                title: "Animation".to_string(),
                status: format_file_size(data.file.size),
                link: data.file.relative_path.clone(),
                description: no_file_description(data.file.skip_reason).to_string(),
                classes: "media_video".to_string(),
                thumb: String::new(),
            });
        }
        let mut result = self.push_div("media_wrap clearfix", "");
        let href = self.relative_path(&data.file.relative_path);
        result.push_str(&self.push_tag(
            "a",
            BTreeMap::from([
                ("class", "animated_wrap clearfix pull_left".to_string()),
                ("href", href),
            ]),
        ));
        result.push_str(&self.push_div("video_play_bg", ""));
        result.push_str(&self.push_div("gif_play", ""));
        result.push_str("GIF");
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        let src = self.relative_path(&data.thumb.file.relative_path);
        result.push_str(&self.push_tag(
            "img",
            BTreeMap::from([
                ("class", "animated".to_string()),
                (
                    "style",
                    format!(
                        "width: {}px; height: {}px",
                        thumb_size.width / 2,
                        thumb_size.height / 2
                    ),
                ),
                ("src", src),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_video_file_media(&mut self, data: &Document) -> String {
        let thumb_size = calculate_thumb_size(
            Size::new(data.width, data.height),
            PHOTO_MAX_WIDTH,
            PHOTO_MAX_HEIGHT,
            true,
        );
        if data.thumb.file.relative_path.is_empty()
            || data.file.relative_path.is_empty()
            || thumb_size.is_empty()
        {
            let mut status = format_duration(i64::from(data.duration));
            let link = if data.file.relative_path.is_empty() {
                status.push_str(", ");
                status.push_str(&format_file_size(data.file.size));
                String::new()
            } else {
                data.file.relative_path.clone()
            };
            return self.push_generic_media(&MediaData {
                title: "Video file".to_string(),
                status,
                link,
                description: no_file_description(data.file.skip_reason).to_string(),
                classes: "media_video".to_string(),
                thumb: String::new(),
            });
        }
        let mut result = self.push_div("media_wrap clearfix", "");
        let href = self.relative_path(&data.file.relative_path);
        result.push_str(&self.push_tag(
            "a",
            BTreeMap::from([
                ("class", "video_file_wrap clearfix pull_left".to_string()),
                ("href", href),
            ]),
        ));
        result.push_str(&self.push_div("video_play_bg", ""));
        result.push_str(&self.push_div("video_play", ""));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("video_duration", ""));
        result.push_str(&format_duration(i64::from(data.duration)));
        result.push_str(&self.pop_tag());
        let src = self.relative_path(&data.thumb.file.relative_path);
        result.push_str(&self.push_tag(
            "img",
            BTreeMap::from([
                ("class", "video_file".to_string()),
                (
                    "style",
                    format!(
                        "width: {}px; height: {}px",
                        thumb_size.width / 2,
                        thumb_size.height / 2
                    ),
                ),
                ("src", src),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_photo_media(&mut self, data: &Photo, base_path: &str) -> String {
        let (thumb, size) = write_image_thumb(
            base_path,
            &data.image.file.relative_path,
            |s| calculate_thumb_size(s, PHOTO_MAX_WIDTH, PHOTO_MAX_HEIGHT, false),
            None,
            "_thumb",
        );
        if thumb.is_empty() {
            let mut status = format_image_size_text(data.image.width, data.image.height);
            let link = if data.image.file.relative_path.is_empty() {
                status.push_str(", ");
                status.push_str(&format_file_size(data.image.file.size));
                String::new()
            } else {
                data.image.file.relative_path.clone()
            };
            return self.push_generic_media(&MediaData {
                title: "Photo".to_string(),
                status,
                link,
                description: no_file_description(data.image.file.skip_reason).to_string(),
                classes: "media_photo".to_string(),
                thumb: String::new(),
            });
        }
        let mut result = self.push_div("media_wrap clearfix", "");
        let href = self.relative_path(&data.image.file.relative_path);
        result.push_str(&self.push_tag(
            "a",
            BTreeMap::from([
                ("class", "photo_wrap clearfix pull_left".to_string()),
                ("href", href),
            ]),
        ));
        let src = self.relative_path(&thumb);
        result.push_str(&self.push_tag(
            "img",
            BTreeMap::from([
                ("class", "photo".to_string()),
                (
                    "style",
                    format!("width: {}px; height: {}px", size.width / 2, size.height / 2),
                ),
                ("src", src),
                ("empty", String::new()),
            ]),
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_rich_photo_media(&mut self, data: Option<&Photo>, base_path: &str) -> String {
        let presentation = rich_photo_presentation(data);
        self.push_photo_media(&presentation, base_path)
    }

    fn push_rich_video_media(&mut self, data: Option<&Document>, base_path: &str) -> String {
        let mut presentation = rich_document_presentation(data);
        let thumb_path = presentation.thumb.file.relative_path.clone();
        if !thumb_path.is_empty() {
            let readable = !is_svg_format(thumb_path.rsplit('.').next().unwrap_or(""))
                && ImageReader::open(format!("{base_path}{thumb_path}"))
                    .ok()
                    .and_then(|r| r.with_guessed_format().ok())
                    .and_then(|r| r.decode().ok())
                    .is_some();
            if !readable {
                presentation.thumb.file.relative_path.clear();
            }
        }
        self.push_video_file_media(&presentation)
    }

    fn push_rich_audio_media(&mut self, data: Option<&Document>) -> String {
        let presentation = rich_document_presentation(data);
        self.push_generic_media(&prepare_audio_media_data(&presentation))
    }

    fn push_rich_file_media(&mut self, data: Option<&Document>) -> String {
        let presentation = rich_document_presentation(data);
        self.push_generic_media(&prepare_file_media_data(&presentation))
    }

    fn push_rich_reference_media(
        &mut self,
        mut data: MediaData,
        photo: Option<&Photo>,
        base_path: &str,
    ) -> String {
        let presentation = rich_photo_presentation(photo);
        let path = presentation.image.file.relative_path;
        data.link = path.clone();
        data.thumb = if path.is_empty() {
            String::new()
        } else {
            write_image_thumb_exact(
                base_path,
                &path,
                ENTRY_USERPIC_SIZE * 2,
                ENTRY_USERPIC_SIZE * 2,
                "_rich_card_thumb",
            )
        };
        self.push_generic_media(&data)
    }

    fn push_poll(&mut self, data: &Poll, internal_links_domain: &str) -> String {
        let base = self.base.clone();
        let mut result = self.push_div("media_wrap clearfix", "");
        result.push_str(&self.push_div("media_poll", ""));
        result.push_str(&self.push_div("question bold", ""));
        result.push_str(&format_html_text(
            &data.question,
            internal_links_domain,
            &base,
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("details", ""));
        result.push_str(&serialize_html_string(if data.closed {
            "Final results"
        } else {
            "Anonymous poll"
        }));
        result.push_str(&self.pop_tag());

        let votes = |count: i32| -> String {
            if count > 1 {
                format!("{count} votes")
            } else if count > 0 {
                format!("{count} vote")
            } else {
                "No votes".to_string()
            }
        };
        for answer in &data.answers {
            result.push_str(&self.push_div("answer", ""));
            let details = if answer.votes == 0 {
                String::new()
            } else if !answer.my {
                format!(" <span class=\"details\">{}</span>", votes(answer.votes))
            } else {
                format!(
                    " <span class=\"details\">{}, chosen vote</span>",
                    votes(answer.votes)
                )
            };
            result.push_str(&format!(
                "- {}{details}",
                format_html_text(&answer.text, internal_links_domain, &base)
            ));
            result.push_str(&self.pop_tag());
        }
        // Matches `export_output_html.cpp:5192` (`pushDiv("total details\t")`).
        result.push_str(&self.push_div("total details\t", ""));
        result.push_str(&votes(data.total_votes));
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_todo_list(&mut self, data: &TodoList, internal_links_domain: &str) -> String {
        let base = self.base.clone();
        let mut result = self.push_div("media_wrap clearfix", "");
        result.push_str(&self.push_div("media_poll", ""));
        result.push_str(&self.push_div("question bold", ""));
        result.push_str(&format_html_text(&data.title, internal_links_domain, &base));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("details", ""));
        result.push_str(&serialize_html_string("To-do List"));
        result.push_str(&self.pop_tag());
        for item in &data.items {
            result.push_str(&self.push_div("answer", ""));
            result.push_str(&format!(
                "- {}",
                format_html_text(&item.text, internal_links_domain, &base)
            ));
            result.push_str(&self.pop_tag());
        }
        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_giveaway_start(&mut self, peers: &PeersMap<'_>, data: &GiveawayStart) -> String {
        let mut result = self.push_div("media_wrap clearfix", "");
        result.push_str(&self.push_div("media_giveaway", ""));
        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string(if data.quantity > 1 {
            "Giveaway Prizes"
        } else {
            "Giveaway Prize"
        }));
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_body", ""));
        result.push_str(&format!(
            "<b>{}</b> {}",
            data.quantity,
            serialize_html_string(&data.additional_prize)
        ));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string("with"));
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_body", ""));
        if data.credits > 0 {
            result.push_str(&format!(
                "<b>{}{}</b> {}<b>{}</b> {}",
                data.credits,
                if data.credits == 1 { " Star" } else { " Stars" },
                if data.quantity == 1 {
                    "will be distributed to "
                } else {
                    "will be distributed among "
                },
                data.quantity,
                if data.quantity == 1 {
                    "winner."
                } else {
                    "winners."
                }
            ));
        } else {
            result.push_str(&format!(
                "<b>{}</b> {} for <b>{}</b> {}",
                data.quantity,
                if data.quantity > 1 {
                    "Telegram Premium Subscriptions"
                } else {
                    "Telegram Premium Subscription"
                },
                data.months,
                if data.months > 1 { "months." } else { "month." }
            ));
        }
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string("Participants"));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("section_body", ""));
        let mut any_channel = false;
        let mut any_group = false;
        for &channel in &data.channels {
            if let Some(chat) = peers.peer(PeerId::from_channel(channel)).as_chat() {
                if chat.is_broadcast {
                    any_channel = true;
                } else if chat.is_supergroup {
                    any_group = true;
                }
            }
        }
        let ch_count = data.channels.len();
        let participants = match (data.all, any_group, any_channel, ch_count) {
            (true, false, true, 1) => "All subscribers of the channel:",
            (true, false, true, n) if n > 1 => "All subscribers of the channels:",
            (true, true, false, 1) | (true, true, true, 1) => "All members of the group:",
            (true, true, false, n) if n > 1 => "All members of the groups:",
            (true, true, true, n) if n > 1 => "All members of the groups and channels:",
            (false, false, true, 1) => "All users who joined the channel below after this date:",
            (false, false, true, n) if n > 1 => {
                "All users who joined the channels below after this date:"
            }
            (false, true, false, 1) | (false, true, true, 1) => {
                "All users who joined the group below after this date:"
            }
            (false, true, false, n) if n > 1 => {
                "All users who joined the groups below after this date:"
            }
            (false, true, true, n) if n > 1 => {
                "All users who joined the groups and channels below after this date:"
            }
            _ => "",
        };
        result.push_str(&serialize_html_string(participants));
        result.push_str(&self.pop_tag());

        if !data.countries.is_empty() {
            let formatted_countries: Vec<String> = data
                .countries
                .iter()
                .map(|code| {
                    format!(
                        "{}\u{00A0}{}",
                        flag_emoji_by_iso2(code),
                        country_name_by_iso2(code)
                    )
                })
                .collect();
            let united = serialize_list(&formatted_countries);
            result.push_str(&self.push_div("section_body", ""));
            result.push_str(&serialize_html_string(&format!("from {united}")));
            result.push_str(&self.pop_tag());
        }

        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string("Winners Selection Date"));
        result.push_str(&self.pop_tag());
        result.push_str(&self.push_div("section_body", ""));
        result.push_str(&format_date_time_with_offset(
            data.until_date,
            self.utc_offset_seconds,
            false,
            '.',
            ':',
            ' ',
        ));
        result.push_str(&self.pop_tag());

        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn push_giveaway_results<F>(
        &mut self,
        peers: &PeersMap<'_>,
        data: &GiveawayResults,
        wrap_message_link: &F,
    ) -> String
    where
        F: Fn(i32, &str) -> String,
    {
        let mut result = self.push_div("media_wrap clearfix", "");
        result.push_str(&self.push_div("media_giveaway", ""));
        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string(if data.winners_count > 1 {
            "Winners Selected!"
        } else {
            "Winner Selected!"
        }));
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_body", ""));
        result.push_str(&format!(
            "<b>{}</b> {} of the {} was randomly selected by Telegram.",
            data.winners_count,
            if data.winners_count > 1 {
                "winners"
            } else {
                "winner"
            },
            wrap_message_link(data.launch_id, "Giveaway")
        ));
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_title bold", ""));
        result.push_str(&serialize_html_string(if data.winners_count > 1 {
            "Winners"
        } else {
            "Winner"
        }));
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_body", ""));
        let winners: Vec<String> = data
            .winners
            .iter()
            .map(|&w| format!("<b>{}</b>", peers.wrap_peer_name(w)))
            .collect();
        result.push_str(&winners.join(", "));
        if (data.winners_count as usize) > data.winners.len() {
            result.push_str(&format!(
                " and {} more!",
                data.winners_count as usize - data.winners.len()
            ));
        }
        result.push_str(&self.pop_tag());

        result.push_str(&self.push_div("section_body", ""));
        let prize = if data.credits > 0 && data.winners_count == 1 {
            format!(
                "The winner received <b>{}</b>{}",
                data.credits,
                if data.credits == 1 {
                    " Star."
                } else {
                    " Stars."
                }
            )
        } else if data.credits > 0 && data.winners_count > 1 {
            format!(
                "All winners received <b>{}</b>{}",
                data.credits,
                if data.credits == 1 {
                    " Star in total."
                } else {
                    " Stars in total."
                }
            )
        } else if data.unclaimed_count != 0 {
            serialize_html_string("Some winners couldn't be selected.")
        } else if data.winners_count == 1 {
            "The winner received their gift link in a private message.".to_string()
        } else if data.winners_count > 1 {
            "All winners received gift links in private messages.".to_string()
        } else {
            String::new()
        };
        result.push_str(&prize);
        result.push_str(&self.pop_tag());

        result.push_str(&self.pop_tag());
        result.push_str(&self.pop_tag());
        result
    }

    fn prepare_media_data(
        &self,
        message: &Message,
        peers: &PeersMap<'_>,
        internal_links_domain: &str,
    ) -> MediaData {
        let mut result = MediaData::default();
        if let ServiceAction::PhoneCall {
            state, duration, ..
        } = &message.action
        {
            result.classes = "media_call".to_string();
            result.title = peers
                .peer(if message.out {
                    message.peer_id
                } else {
                    message.self_id
                })
                .name();
            result.status = match state {
                PhoneCallState::Invitation => "Invitation",
                PhoneCallState::Active => "Ongoing",
                PhoneCallState::Missed if message.out => "Cancelled",
                _ if message.out => "Outgoing",
                PhoneCallState::Missed => "Missed",
                PhoneCallState::Busy => "Declined",
                _ => "Incoming",
            }
            .to_string();
            if *duration > 0 {
                result.classes.push_str(" success");
                result.status.push_str(&format!(" ({duration} seconds)"));
            }
            return result;
        }

        match &message.media.content {
            MediaContent::Photo(data) => {
                if message.media.ttl != 0 {
                    result.title = "Self-destructing photo".to_string();
                    result.status = if data.id != 0 {
                        "Please view it on your mobile"
                    } else {
                        "Expired"
                    }
                    .to_string();
                    result.classes = "media_photo".to_string();
                }
            }
            MediaContent::Document(data) => {
                if message.media.ttl != 0 {
                    result.title = "Self-destructing video".to_string();
                    result.status = if data.id != 0 {
                        "Please view it on your mobile"
                    } else {
                        "Expired"
                    }
                    .to_string();
                    result.classes = "media_video".to_string();
                    return result;
                }
                let has_file = !data.file.relative_path.is_empty();
                result.link = data.file.relative_path.clone();
                result.description = no_file_description(data.file.skip_reason).to_string();
                if data.is_sticker || data.is_animated || data.is_video_file {
                    // Handled by dedicated visual push_*_media methods.
                } else if data.is_video_message {
                    result.title = "Video message".to_string();
                    result.status = format_duration(i64::from(data.duration));
                    if !has_file {
                        result.status.push_str(", ");
                        result.status.push_str(&format_file_size(data.file.size));
                    }
                    result.thumb = data.thumb.file.relative_path.clone();
                    result.classes = "media_video".to_string();
                } else if data.is_voice_message {
                    result.title = "Voice message".to_string();
                    result.status = format_duration(i64::from(data.duration));
                    if !has_file {
                        result.status.push_str(", ");
                        result.status.push_str(&format_file_size(data.file.size));
                    }
                    result.classes = "media_voice_message".to_string();
                } else if data.is_audio_file {
                    result = prepare_audio_media_data(data);
                } else {
                    result = prepare_file_media_data(data);
                }
            }
            MediaContent::SharedContact(data) => {
                result.title = format!("{} {}", data.info.first_name, data.info.last_name);
                result.classes = "media_contact".to_string();
                result.status = format_phone_number(&data.info.phone_number);
                if !data.vcard.content.is_empty() {
                    result.status.push_str(" - vCard");
                    result.link = data.vcard.relative_path.clone();
                }
            }
            MediaContent::GeoPoint(data) => {
                if message.media.ttl != 0 {
                    result.classes = "media_live_location".to_string();
                    result.title = "Live location".to_string();
                } else {
                    result.classes = "media_location".to_string();
                    result.title = "Location".to_string();
                }
                if data.valid {
                    let lat = crate::data::float_to_string(data.latitude);
                    let lon = crate::data::float_to_string(data.longitude);
                    let coords = format!("{lat},{lon}");
                    result.status = format!("{lat}, {lon}");
                    result.link =
                        format!("https://maps.google.com/maps?q={coords}&ll={coords}&z=16");
                }
            }
            MediaContent::Venue(data) => {
                result.classes = "media_venue".to_string();
                result.title = data.title.clone();
                result.description = data.address.clone();
                if data.point.valid {
                    let lat = crate::data::float_to_string(data.point.latitude);
                    let lon = crate::data::float_to_string(data.point.longitude);
                    let coords = format!("{lat},{lon}");
                    result.link =
                        format!("https://maps.google.com/maps?q={coords}&ll={coords}&z=16");
                }
            }
            MediaContent::Game(data) => {
                result.classes = "media_game".to_string();
                result.title = data.title.clone();
                result.description = data.description.clone();
                if data.bot_id.0 != 0 && !data.short_name.is_empty() {
                    let bot = peers.user(data.bot_id);
                    if bot.is_bot && !bot.username.is_empty() {
                        let link = format!(
                            "{internal_links_domain}{}?game={}",
                            bot.username, data.short_name
                        );
                        result.link = link.clone();
                        result.status = link;
                    }
                }
            }
            MediaContent::Invoice(data) => {
                result.classes = "media_invoice".to_string();
                result.title = data.title.clone();
                result.description = data.description.clone();
                result.status = format_money_amount(data.amount as i64, &data.currency);
            }
            MediaContent::PaidMedia(data) => {
                result.classes = "media_invoice".to_string();
                result.status = format_money_amount(data.stars as i64, "XTR");
            }
            MediaContent::Poll(_)
            | MediaContent::TodoList(_)
            | MediaContent::GiveawayStart(_)
            | MediaContent::GiveawayResults(_)
            | MediaContent::Unsupported(_)
            | MediaContent::None => {}
        }
        result
    }

    fn push_media<F>(
        &mut self,
        message: &Message,
        base_path: &str,
        peers: &PeersMap<'_>,
        internal_links_domain: &str,
        wrap_message_link: &F,
    ) -> String
    where
        F: Fn(i32, &str) -> String,
    {
        let data = self.prepare_media_data(message, peers, internal_links_domain);
        if !data.classes.is_empty() {
            return self.push_generic_media(&data);
        }
        match &message.media.content {
            MediaContent::Document(doc) => {
                if doc.is_sticker {
                    self.push_sticker_media(doc, base_path)
                } else if doc.is_animated {
                    self.push_animated_media(doc)
                } else if doc.is_video_file {
                    self.push_video_file_media(doc)
                } else {
                    String::new()
                }
            }
            MediaContent::Photo(photo) => self.push_photo_media(photo, base_path),
            MediaContent::Poll(poll) => self.push_poll(poll, internal_links_domain),
            MediaContent::TodoList(todo) => self.push_todo_list(todo, internal_links_domain),
            MediaContent::GiveawayStart(gw) => self.push_giveaway_start(peers, gw),
            MediaContent::GiveawayResults(gw) => {
                self.push_giveaway_results(peers, gw, wrap_message_link)
            }
            _ => String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_message<F>(
        &mut self,
        message: &Message,
        previous: Option<&MessageInfo>,
        dialog: &DialogInfo,
        base_path: &str,
        peers: &PeersMap<'_>,
        internal_links_domain: &str,
        wrap_message_link: &F,
    ) -> (MessageInfo, String)
    where
        F: Fn(i32, &str) -> String,
    {
        let mut info = MessageInfo {
            id: message.id,
            kind: MessageKind::Service,
            from_id: message.from_id,
            via_bot_id: message.via_bot_id,
            date: message.date,
            forwarded_from_id: message.forwarded_from_id,
            forwarded_from_name: message.forwarded_from_name.clone(),
            forwarded: message.forwarded,
            show_forwarded_as_original: message.show_forwarded_as_original,
            forwarded_date: message.forwarded_date,
        };

        if matches!(message.media.content, MediaContent::Unsupported(_)) {
            let html = self.push_service_message(
                message.id,
                dialog,
                base_path,
                "This message is not supported by this version of Telegram Desktop. Please update the application.",
                None,
            );
            return (info, html);
        }

        let is_channel = matches!(
            dialog.kind,
            DialogType::PrivateChannel | DialogType::PublicChannel
        );
        let service_from = peers.wrap_peer_name(message.from_id);
        let service_text = self.format_service_action_text(
            message,
            dialog,
            peers,
            is_channel,
            &service_from,
            internal_links_domain,
            wrap_message_link,
        );

        if !service_text.is_empty() {
            let photo = match &message.action {
                ServiceAction::ChatEditPhoto { photo } => Some(photo),
                ServiceAction::SuggestProfilePhoto { photo } => Some(photo),
                _ => None,
            };
            let html =
                self.push_service_message(message.id, dialog, base_path, &service_text, photo);
            return (info, html);
        }

        info.kind = MessageKind::Default;
        let wrap = self.message_needs_wrap(message, previous);
        let from_peer_id = message.from_id;
        let show_forwarded_info = message.forwarded && !message.show_forwarded_as_original;

        let mut forwarded_userpic = UserpicData::default();
        if message.forwarded {
            forwarded_userpic.color_index = if !message.forwarded_from_id.is_zero() {
                peer_color_index_peer(message.forwarded_from_id)
            } else {
                peer_color_index(message.id as u64)
            };
            forwarded_userpic.pixel_size = HISTORY_USERPIC_SIZE;
            if !message.forwarded_from_id.is_zero() {
                fill_userpic_names_from_peer(
                    &mut forwarded_userpic,
                    peers.peer(message.forwarded_from_id),
                );
            } else {
                fill_userpic_names_from_full(&mut forwarded_userpic, &message.forwarded_from_name);
            }
        }

        let userpic = if message.show_forwarded_as_original {
            forwarded_userpic.clone()
        } else {
            let mut u = UserpicData {
                color_index: peer_color_index_peer(from_peer_id),
                pixel_size: HISTORY_USERPIC_SIZE,
                ..Default::default()
            };
            fill_userpic_names_from_peer(&mut u, peers.peer(from_peer_id));
            u
        };

        let via = if message.via_bot_id.0 != 0 {
            let u = peers.user(message.via_bot_id);
            if !u.username.is_empty() {
                serialize_html_string(&u.username)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let class_name = if wrap {
            "message default clearfix"
        } else {
            "message default clearfix joined"
        };
        let mut block = self.push_tag(
            "div",
            BTreeMap::from([
                ("class", class_name.to_string()),
                ("id", format!("message{}", message.id)),
            ]),
        );
        if wrap {
            block.push_str(&self.push_div("pull_left userpic_wrap", ""));
            block.push_str(&self.push_userpic(&userpic));
            block.push_str(&self.pop_tag());
        }
        block.push_str(&self.push_div("body", ""));
        block.push_str(&self.push_tag(
            "div",
            BTreeMap::from([
                ("class", "pull_right date details".to_string()),
                (
                    "title",
                    format_date_time_with_offset(
                        message.date,
                        self.utc_offset_seconds,
                        true,
                        '.',
                        ':',
                        ' ',
                    ),
                ),
            ]),
        ));
        block.push_str(&format_time_text_with_offset(
            message.date,
            self.utc_offset_seconds,
        ));
        block.push_str(&self.pop_tag());
        if wrap {
            block.push_str(&self.push_div("from_name", ""));
            block.push_str(&serialize_html_string(&compose_name(
                &userpic,
                "Deleted Account",
            )));
            if !via.is_empty() && (!message.forwarded || message.show_forwarded_as_original) {
                block.push_str(&format!(" via @{via}"));
            }
            block.push_str(&self.pop_tag());
        }
        if show_forwarded_info {
            let forwarded_wrap = self.forwarded_needs_wrap(message, previous);
            if forwarded_wrap {
                block.push_str(&self.push_div("pull_left forwarded userpic_wrap", ""));
                block.push_str(&self.push_userpic(&forwarded_userpic));
                block.push_str(&self.pop_tag());
            }
            block.push_str(&self.push_div("forwarded body", ""));
            if forwarded_wrap {
                block.push_str(&self.push_div("from_name", ""));
                block.push_str(&serialize_html_string(&compose_name(
                    &forwarded_userpic,
                    "Deleted Account",
                )));
                if !via.is_empty() {
                    block.push_str(&format!(" via @{via}"));
                }
                block.push_str(&self.push_tag(
                    "span",
                    BTreeMap::from([
                        ("class", "date details".to_string()),
                        (
                            "title",
                            format_date_time_with_offset(
                                message.forwarded_date,
                                self.utc_offset_seconds,
                                true,
                                '.',
                                ':',
                                ' ',
                            ),
                        ),
                        ("inline", String::new()),
                    ]),
                ));
                block.push(' ');
                block.push_str(&format_date_time_with_offset(
                    message.forwarded_date,
                    self.utc_offset_seconds,
                    false,
                    '.',
                    ':',
                    ' ',
                ));
                block.push_str(&self.pop_tag());
                block.push_str(&self.pop_tag());
            }
        }
        if message.reply_to_msg_id != 0 {
            block.push_str(&self.push_div("reply_to details", ""));
            if !message.reply_to_peer_id.is_zero() {
                block.push_str("In reply to a message in another chat");
            } else {
                block.push_str("In reply to ");
                block.push_str(&wrap_message_link(message.reply_to_msg_id, "this message"));
            }
            block.push_str(&self.pop_tag());
        }

        block.push_str(&self.push_media(
            message,
            base_path,
            peers,
            internal_links_domain,
            wrap_message_link,
        ));

        if let Some(ref rich) = message.rich_message {
            let mut renderer =
                RichHtmlRenderer::new(self, rich, message.id, internal_links_domain, base_path);
            block.push_str(&renderer.render_message());
        } else {
            let text = format_html_text(&message.text, internal_links_domain, &self.base);
            if !text.is_empty() {
                block.push_str(&self.push_div("text", ""));
                block.push_str(&text);
                block.push_str(&self.pop_tag());
            }
        }

        if !message.inline_button_rows.is_empty() {
            block.push_str(&self.push_tag(
                "table",
                BTreeMap::from([("class", "bot_buttons_table".to_string())]),
            ));
            block.push_str(&self.push_tag("tbody", BTreeMap::new()));
            for row in &message.inline_button_rows {
                block.push_str(&self.push_tag("tr", BTreeMap::new()));
                block.push_str(&self.push_tag(
                    "td",
                    BTreeMap::from([("class", "bot_button_row".to_string())]),
                ));
                for (btn_idx, button) in row.iter().enumerate() {
                    let mut content = String::new();
                    if !button.data.is_empty() {
                        content.push_str("Data: ");
                        content.push_str(&String::from_utf8_lossy(&button.data));
                        content.push_str(" | ");
                    }
                    if !button.forward_text.is_empty() {
                        content.push_str("Forward text: ");
                        content.push_str(&button.forward_text);
                        content.push_str(" | ");
                    }
                    content.push_str("Type: ");
                    content.push_str(button.type_str());

                    let mut a_attrs = BTreeMap::new();
                    if button.kind == crate::data::MarkupButtonType::Url {
                        if let Ok(s) = std::str::from_utf8(&button.data)
                            && let Some(href) = safe_message_href(s)
                        {
                            a_attrs.insert("href", href);
                        }
                    } else {
                        let escaped = content.replace('\\', "\\\\").replace('\'', "\\'");
                        a_attrs.insert("onclick", format!("return ShowTextCopied('{escaped}');"));
                    }
                    block.push_str(&self.push_div("bot_button", ""));
                    block.push_str(&self.push_tag("a", a_attrs));
                    block.push_str(&self.push_tag("div", BTreeMap::new()));
                    block.push_str(&serialize_html_string(&button.text));
                    block.push_str(&self.pop_tag());
                    block.push_str(&self.pop_tag());
                    block.push_str(&self.pop_tag());
                    if btn_idx + 1 != row.len() {
                        block.push_str(&self.push_div("bot_button_column_separator", ""));
                        block.push_str(&self.pop_tag());
                    }
                }
                block.push_str(&self.pop_tag());
                block.push_str(&self.pop_tag());
            }
            block.push_str(&self.pop_tag());
            block.push_str(&self.pop_tag());
        }

        if !message.signature.is_empty() {
            block.push_str(&self.push_div("signature details", ""));
            block.push_str(&serialize_html_string(&message.signature));
            block.push_str(&self.pop_tag());
        }
        if show_forwarded_info {
            block.push_str(&self.pop_tag());
        }
        if !message.reactions.is_empty() {
            block.push_str(
                &self.push_tag("span", BTreeMap::from([("class", "reactions".to_string())])),
            );
            for reaction in &message.reactions {
                let mut reaction_class = String::from("reaction");
                if reaction
                    .recent
                    .iter()
                    .any(|r| peers.peer(r.peer_id).as_user().is_some_and(|u| u.is_self))
                {
                    reaction_class.push_str(" active");
                }
                if reaction.kind == ReactionType::Paid {
                    reaction_class.push_str(" paid");
                }
                block.push_str(&self.push_tag("span", BTreeMap::from([("class", reaction_class)])));
                block.push_str(
                    &self.push_tag("span", BTreeMap::from([("class", "emoji".to_string())])),
                );
                match reaction.kind {
                    ReactionType::Emoji => {
                        block.push_str(&serialize_html_string(&reaction.emoji));
                    }
                    ReactionType::CustomEmoji => {
                        block.push_str(&format_custom_emoji(
                            &reaction.document_id,
                            "\u{1F44B}",
                            &self.base,
                        ));
                    }
                    ReactionType::Paid => {
                        block.push_str(&serialize_html_string("\u{2B50}"));
                    }
                    ReactionType::Empty => {}
                }
                block.push_str(&self.pop_tag());
                if !reaction.recent.is_empty() {
                    block.push_str(
                        &self.push_tag("span", BTreeMap::from([("class", "userpics".to_string())])),
                    );
                    for recent in &reaction.recent {
                        let p = peers.peer(recent.peer_id);
                        let u = UserpicData {
                            color_index: p.color_index(),
                            pixel_size: 20,
                            first_name: p
                                .as_user()
                                .map(|usr| usr.info.first_name.clone())
                                .unwrap_or_else(|| p.name()),
                            last_name: p
                                .as_user()
                                .map(|usr| usr.info.last_name.clone())
                                .unwrap_or_default(),
                            tooltip: p.name(),
                            ..Default::default()
                        };
                        block.push_str(&self.push_userpic(&u));
                    }
                    block.push_str(&self.pop_tag());
                }
                if reaction.recent.is_empty() || (reaction.count as usize) > reaction.recent.len() {
                    block.push_str(
                        &self.push_tag("span", BTreeMap::from([("class", "count".to_string())])),
                    );
                    block.push_str(&reaction.count.to_string());
                    block.push_str(&self.pop_tag());
                }
                block.push_str(&self.pop_tag());
            }
            block.push_str(&self.pop_tag());
        }
        block.push_str(&self.pop_tag());
        block.push_str(&self.pop_tag());
        (info, block)
    }

    #[allow(clippy::too_many_arguments)]
    fn format_service_action_text<F>(
        &mut self,
        message: &Message,
        dialog: &DialogInfo,
        peers: &PeersMap<'_>,
        is_channel: bool,
        service_from: &str,
        internal_links_domain: &str,
        wrap_message_link: &F,
    ) -> String
    where
        F: Fn(i32, &str) -> String,
    {
        let wrap_reply = |label: &str| wrap_message_link(message.reply_to_msg_id, label);
        match &message.action {
            ServiceAction::None | ServiceAction::PhoneCall { .. } => String::new(),
            ServiceAction::ChatCreate { title, user_ids } => {
                let mut s = format!(
                    "{service_from} created group &laquo;{}&raquo;",
                    serialize_html_string(title)
                );
                if !user_ids.is_empty() {
                    s.push_str(" with members ");
                    s.push_str(&peers.wrap_user_names(user_ids));
                }
                s
            }
            ServiceAction::ChatEditTitle { title } => {
                let esc = serialize_html_string(title);
                if is_channel {
                    format!("Channel title changed to &laquo;{esc}&raquo;")
                } else {
                    format!("{service_from} changed group title to &laquo;{esc}&raquo;")
                }
            }
            ServiceAction::ChatEditPhoto { .. } => {
                if is_channel {
                    "Channel photo changed".to_string()
                } else {
                    format!("{service_from} changed group photo")
                }
            }
            ServiceAction::ChatDeletePhoto => {
                if is_channel {
                    "Channel photo removed".to_string()
                } else {
                    format!("{service_from} removed group photo")
                }
            }
            ServiceAction::ChatAddUser { user_ids } => {
                format!("{service_from} invited {}", peers.wrap_user_names(user_ids))
            }
            ServiceAction::ChatDeleteUser { user_id } => {
                format!("{service_from} removed {}", peers.wrap_user_name(*user_id))
            }
            ServiceAction::ChatJoinedByLink { inviter_id } => format!(
                "{service_from} joined group by link from {}",
                peers.wrap_user_name(*inviter_id)
            ),
            ServiceAction::ChannelCreate { title } => {
                format!(
                    "Channel &laquo;{}&raquo; created",
                    serialize_html_string(title)
                )
            }
            ServiceAction::ChatMigrateTo { .. } => {
                format!("{service_from} converted this group to a supergroup")
            }
            ServiceAction::ChannelMigrateFrom { title, .. } => format!(
                "{service_from} converted a basic group to this supergroup &laquo;{}&raquo;",
                serialize_html_string(title)
            ),
            ServiceAction::PinMessage => {
                format!("{service_from} pinned {}", wrap_reply("this message"))
            }
            ServiceAction::HistoryClear => "History cleared".to_string(),
            ServiceAction::GameScore { score, .. } => {
                format!(
                    "{service_from} scored {score} in {}",
                    wrap_reply("this game")
                )
            }
            ServiceAction::PaymentSent {
                currency,
                amount,
                recurring_init,
                recurring_used,
            } => {
                let formatted = format_money_amount(*amount as i64, currency);
                if *recurring_used {
                    format!("You were charged {formatted} via recurring payment")
                } else {
                    let mut res = format!(
                        "You have successfully transferred {formatted} for {}",
                        wrap_reply("this invoice")
                    );
                    if *recurring_init {
                        res.push_str(" and allowed future recurring payments");
                    }
                    res
                }
            }
            ServiceAction::ScreenshotTaken => format!("{service_from} took a screenshot"),
            ServiceAction::CustomAction { message: text } => serialize_html_string(text),
            ServiceAction::BotAllowed {
                app,
                domain,
                attach_menu,
                from_request,
                ..
            } => {
                if *attach_menu {
                    "You allowed this bot to message you when you added it in the attachment menu."
                        .to_string()
                } else if *from_request {
                    "You allowed this bot to message you in his web-app.".to_string()
                } else if app.is_empty() {
                    // Matches exact condition in `export_output_html.cpp:4030`.
                    format!(
                        "You allowed this bot to message you when you opened {}",
                        serialize_html_string(app)
                    )
                } else {
                    format!(
                        "You allowed this bot to message you when you logged in on {}",
                        serialize_html_string(domain)
                    )
                }
            }
            ServiceAction::SecureValuesSent { types } => {
                let list: Vec<String> = types
                    .iter()
                    .map(|t| {
                        match t {
                            SecureValueType::PersonalDetails => "Personal details",
                            SecureValueType::Passport => "Passport",
                            SecureValueType::DriverLicense => "Driver license",
                            SecureValueType::IdentityCard => "Identity card",
                            SecureValueType::InternalPassport => "Internal passport",
                            SecureValueType::Address => "Address information",
                            SecureValueType::UtilityBill => "Utility bill",
                            SecureValueType::BankStatement => "Bank statement",
                            SecureValueType::RentalAgreement => "Rental agreement",
                            SecureValueType::PassportRegistration => "Passport registration",
                            SecureValueType::TemporaryRegistration => "Temporary registration",
                            SecureValueType::Phone => "Phone number",
                            SecureValueType::Email => "Email",
                        }
                        .to_string()
                    })
                    .collect();
                format!(
                    "You have sent the following documents: {}",
                    serialize_list(&list)
                )
            }
            ServiceAction::ContactSignUp => format!("{service_from} joined Telegram"),
            ServiceAction::GeoProximityReached {
                from_id,
                to_id,
                distance,
                from_self,
                to_self,
            } => {
                let from_name = peers.wrap_peer_name(*from_id);
                let to_name = peers.wrap_peer_name(*to_id);
                let dist_str = if *distance >= 1000 {
                    let km = (10 * (*distance / 10)) as f64 / 1000.0;
                    format!("{km} km")
                } else if *distance == 1 {
                    "1 meter".to_string()
                } else {
                    format!("{distance} meters")
                };
                if *from_self {
                    format!("You are now within {dist_str} from {to_name}")
                } else if *to_self {
                    format!("{from_name} is now within {dist_str} from you")
                } else {
                    format!("{from_name} is now within {dist_str} from {to_name}")
                }
            }
            ServiceAction::PhoneNumberRequest => {
                format!("{service_from} requested your phone number")
            }
            ServiceAction::GroupCall { duration } => {
                let dur = if *duration != 0 {
                    format!(" ({duration} seconds)")
                } else {
                    String::new()
                };
                if is_channel {
                    format!("Voice chat{dur}")
                } else {
                    format!("{service_from} started voice chat{dur}")
                }
            }
            ServiceAction::InviteToGroupCall { user_ids } => format!(
                "{service_from} invited {} to the voice chat",
                peers.wrap_user_names(user_ids)
            ),
            ServiceAction::SetMessagesTtl { period } => {
                let period_text = if *period == 7 * 86400 {
                    "7 days"
                } else if *period == 86400 {
                    "24 hours"
                } else {
                    ""
                };
                if is_channel {
                    if *period != 0 {
                        format!("New messages will auto-delete in {period_text}")
                    } else {
                        "New messages will not auto-delete".to_string()
                    }
                } else if *period != 0 {
                    format!("{service_from} has set messages to auto-delete in {period_text}")
                } else {
                    format!("{service_from} has set messages not to auto-delete")
                }
            }
            ServiceAction::GroupCallScheduled { date } => {
                let date_text = format_date_time_with_offset(
                    *date,
                    self.utc_offset_seconds,
                    false,
                    '.',
                    ':',
                    ' ',
                );
                if is_channel {
                    format!("Voice chat scheduled for {date_text}")
                } else {
                    format!("{service_from} scheduled a voice chat for {date_text}")
                }
            }
            ServiceAction::SetChatTheme { emoji } => {
                if emoji.is_empty() {
                    if is_channel {
                        "Channel theme was disabled".to_string()
                    } else {
                        format!("{service_from} disabled chat theme")
                    }
                } else {
                    let esc = serialize_html_string(emoji);
                    if is_channel {
                        format!("Channel theme was changed to {esc}")
                    } else {
                        format!("{service_from} changed chat theme to {esc}")
                    }
                }
            }
            ServiceAction::ChatJoinedByRequest => format!("{service_from} joined group by request"),
            ServiceAction::ChatJoinedViaCommunity { .. } => {
                format!("{service_from} joined group via a community")
            }
            ServiceAction::WebViewDataSent { text } => format!(
                "You have just successfully transferred data from the &laquo;{}&raquo; button to the bot",
                serialize_html_string(text)
            ),
            ServiceAction::GiftPremium { cost, days } => {
                if *days == 0 || cost.is_empty() {
                    format!("{service_from} sent you a gift.")
                } else {
                    format!(
                        "{service_from} sent you a gift for {}: Telegram Premium for {days} days.",
                        serialize_html_string(cost)
                    )
                }
            }
            ServiceAction::TopicCreate { title } => {
                format!(
                    "{service_from} created topic &laquo;{}&raquo;",
                    serialize_html_string(title)
                )
            }
            ServiceAction::TopicEdit {
                title,
                icon_emoji_id,
            } => {
                let mut parts = Vec::new();
                if !title.is_empty() {
                    parts.push(format!(
                        "title to &laquo;{}&raquo;",
                        serialize_html_string(title)
                    ));
                }
                if let Some(icon_id) = icon_emoji_id {
                    parts.push(format!("icon to &laquo;{icon_id}&raquo;"));
                }
                format!("{service_from} changed topic {}", parts.join(","))
            }
            ServiceAction::SuggestProfilePhoto { .. } => {
                format!("{service_from} suggests to use this photo")
            }
            ServiceAction::RequestedPeer { .. } => "requested: ".to_string(),
            ServiceAction::SetChatWallPaper { same, .. } => {
                if *same {
                    format!(
                        "{service_from} set {} for this chat",
                        wrap_reply("the same background")
                    )
                } else {
                    format!("{service_from} set a new background for this chat")
                }
            }
            ServiceAction::GiftCode {
                days,
                via_giveaway,
                unclaimed,
                ..
            } => {
                let unit = if *days > 1 { " days" } else { " day" };
                if *unclaimed {
                    format!(
                        "This is an unclaimed Telegram Premium for {days}{unit} prize in a giveaway organized by a channel."
                    )
                } else if *via_giveaway {
                    format!(
                        "You won a Telegram Premium for {days}{unit} prize in a giveaway organized by a channel."
                    )
                } else {
                    format!(
                        "You've received a Telegram Premium for {days}{unit} gift from a channel."
                    )
                }
            }
            ServiceAction::GiveawayLaunch => format!(
                "{service_from} just started a giveaway of Telegram Premium subscriptions to its followers."
            ),
            ServiceAction::GiveawayResults {
                winners,
                unclaimed,
                credits,
            } => {
                if *winners == 0 {
                    "No winners of the giveaway could be selected.".to_string()
                } else if *credits && *unclaimed != 0 {
                    "Some winners of the giveaway were randomly selected by Telegram and received their prize."
                        .to_string()
                } else if !*credits && *unclaimed != 0 {
                    "Some winners of the giveaway were randomly selected by Telegram and received private messages with giftcodes."
                        .to_string()
                } else if *credits {
                    format!(
                        "{winners} of the giveaway was randomly selected by Telegram and received their prize."
                    )
                } else {
                    format!(
                        "{winners} of the giveaway was randomly selected by Telegram and received private messages with giftcodes."
                    )
                }
            }
            ServiceAction::BoostApply { boosts } => format!(
                "{service_from} boosted the group {boosts}{}",
                if *boosts > 1 { " times" } else { " time" }
            ),
            ServiceAction::PaymentRefunded {
                peer_id,
                currency,
                amount,
                ..
            } => format!(
                "{} refunded back {}",
                peers.wrap_peer_name(*peer_id),
                format_money_amount(*amount as i64, currency)
            ),
            ServiceAction::GiftCredits { cost, amount } => {
                if credits_is_empty(*amount) || cost.is_empty() {
                    format!("{service_from} sent you a gift.")
                } else {
                    format!(
                        "{service_from} sent you a gift for {}: {}{}",
                        serialize_html_string(cost),
                        amount.value(),
                        if credits_is_ton(*amount) {
                            " TON."
                        } else {
                            " Telegram Stars."
                        }
                    )
                }
            }
            ServiceAction::PrizeStars {
                peer_id, amount, ..
            } => format!(
                "You won a prize in a giveaway organized by {}.\n Your prize is {amount} Telegram Stars.",
                peers.wrap_peer_name(*peer_id)
            ),
            ServiceAction::StarGift { stars, .. } => {
                format!("{service_from} sent you a gift of {stars} Telegram Stars.")
            }
            ServiceAction::PaidMessagesRefunded { messages, stars } => {
                if message.out {
                    format!(
                        "You refunded {stars} Stars for {messages} messages to {}",
                        peers.wrap_peer_name(dialog.peer_id)
                    )
                } else {
                    format!(
                        "{} refunded {stars} Stars for {messages} messages to you",
                        peers.wrap_peer_name(dialog.peer_id)
                    )
                }
            }
            ServiceAction::PaidMessagesPrice {
                stars,
                broadcast_allowed,
            } => {
                if is_channel {
                    if !*broadcast_allowed {
                        "Direct messages were disabled.".to_string()
                    } else {
                        format!("Price per direct message changed to {stars} Telegram Stars.")
                    }
                } else {
                    format!("Price per message changed to {stars} Telegram Stars.")
                }
            }
            ServiceAction::TodoCompletions {
                completed,
                incompleted,
            } => {
                let comp: Vec<String> = completed.iter().map(ToString::to_string).collect();
                let incomp: Vec<String> = incompleted.iter().map(ToString::to_string).collect();
                if comp.is_empty() && !incomp.is_empty() {
                    format!(
                        "{service_from} marked {} as not done yet in {}.",
                        serialize_list(&incomp),
                        wrap_reply("this todo list")
                    )
                } else if !comp.is_empty() && incomp.is_empty() {
                    format!(
                        "{service_from} marked {} as done in {}.",
                        serialize_list(&comp),
                        wrap_reply("this todo list")
                    )
                } else {
                    format!(
                        "{service_from} marked {} as done and {} as not done yet in {}.",
                        serialize_list(&comp),
                        serialize_list(&incomp),
                        wrap_reply("this todo list")
                    )
                }
            }
            ServiceAction::TodoAppendTasks { items } => {
                let tasks: Vec<String> = items
                    .iter()
                    .map(|task| {
                        format!(
                            "&quot;{}&quot;",
                            format_html_text(&task.text, internal_links_domain, &self.base)
                        )
                    })
                    .collect();
                format!("{service_from} added tasks: {}", tasks.join(", "))
            }
            ServiceAction::PollAppendAnswer { option } => format!(
                "{service_from} added &quot;{}&quot; to the poll.",
                serialize_html_string(option)
            ),
            ServiceAction::PollDeleteAnswer { option } => format!(
                "{service_from} removed &quot;{}&quot; from the poll.",
                serialize_html_string(option)
            ),
            ServiceAction::SuggestedPostApproval {
                reject_comment,
                schedule_date,
                price,
                rejected,
                ..
            } => {
                let mut s = format!(
                    "{service_from}{}your suggested post",
                    if *rejected {
                        " rejected "
                    } else {
                        " approved "
                    }
                );
                if !credits_is_empty(*price) {
                    s.push_str(&format!(
                        ", for {}{}",
                        price.value(),
                        if credits_is_ton(*price) {
                            " TON"
                        } else {
                            " stars"
                        }
                    ));
                }
                if *schedule_date != 0 {
                    s.push_str(&format!(
                        ", {} at {}",
                        format_date_text_with_offset(*schedule_date, self.utc_offset_seconds),
                        format_time_text_with_offset(*schedule_date, self.utc_offset_seconds)
                    ));
                }
                if reject_comment.is_empty() {
                    s.push('.');
                } else {
                    s.push_str(&format!(
                        ", with comment: &quot;{}&quot;",
                        serialize_html_string(reject_comment)
                    ));
                }
                s
            }
            ServiceAction::SuggestedPostSuccess { price } => format!(
                "The paid post was shown for 24 hours and {}{} were transferred to the channel.",
                price.value(),
                if credits_is_ton(*price) {
                    " TON"
                } else {
                    " stars"
                }
            ),
            ServiceAction::SuggestedPostRefund { payer_initiated } => {
                if *payer_initiated {
                    "The user refunded the payment, post was deleted.".to_string()
                } else {
                    "The admin deleted the post early, the payment was refunded.".to_string()
                }
            }
            ServiceAction::SuggestBirthday { birthday } => {
                let month = match birthday.month() {
                    1 => " January",
                    2 => " February",
                    3 => " March",
                    4 => " April",
                    5 => " May",
                    6 => " June",
                    7 => " July",
                    8 => " August",
                    9 => " September",
                    10 => " October",
                    11 => " November",
                    12 => " December",
                    _ => "",
                };
                let year = if birthday.year() != 0 {
                    format!(" {}", birthday.year())
                } else {
                    String::new()
                };
                format!(
                    "{service_from} suggests to add a date of birth: {}{month}{year}",
                    birthday.day()
                )
            }
            ServiceAction::NoForwardsToggle { new_value } => format!(
                "{service_from}{}",
                if *new_value {
                    " disabled sharing in this chat"
                } else {
                    " enabled sharing in this chat"
                }
            ),
            ServiceAction::NoForwardsRequest { .. } => {
                format!("{service_from} requested to enable sharing in this chat")
            }
            ServiceAction::NewCreatorPending { new_creator_id } => format!(
                "{} will become the new main admin in 7 days if {service_from} does not return",
                peers.wrap_user_name(*new_creator_id)
            ),
            ServiceAction::ChangeCreator { new_creator_id } => format!(
                "{service_from} made {} the new main admin of the group",
                peers.wrap_user_name(*new_creator_id)
            ),
            ServiceAction::ManagedBotCreated { bot_id } => {
                format!(
                    "{service_from} created a bot {}",
                    peers.wrap_user_name(*bot_id)
                )
            }
            ServiceAction::GramTransfer {
                amount,
                peer_address,
                transaction_id,
                comment,
                comment_encrypted,
            } => {
                let amt = format_grams_amount(*amount);
                let addr = if peer_address.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", serialize_html_string(peer_address))
                };
                let hidden = dialog.peer_id == PeerId::from_user(UserId(333_000))
                    || dialog.peer_id == PeerId::from_user(UserId(777_000));
                let sender = if hidden { "Someone" } else { service_from };
                let mut res = if message.out {
                    format!(
                        "You sent {amt} to {}{addr}",
                        peers.wrap_peer_name(dialog.peer_id)
                    )
                } else {
                    format!("{sender}{addr} sent you {amt}")
                };
                if !transaction_id.is_empty() {
                    res.push_str(", transaction ");
                    res.push_str(&serialize_html_string(transaction_id));
                }
                if *comment_encrypted {
                    res.push_str(", with an ");
                    res.push_str(&self.push_tag(
                        "span",
                        BTreeMap::from([
                            ("class", "gram_transfer_encrypted_comment".to_string()),
                            ("data-encrypted-comment", comment.clone()),
                            ("inline", String::new()),
                        ]),
                    ));
                    res.push_str("encrypted comment");
                    res.push_str(&self.pop_tag());
                } else if !comment.is_empty() {
                    res.push_str(&format!(
                        ", with comment: &laquo;{}&raquo;",
                        serialize_html_string(comment)
                    ));
                }
                res
            }
            ServiceAction::WalletTonConnectRequest {
                topic,
                accepted,
                declined,
                ..
            } => {
                let topic_str = if topic.is_empty() {
                    String::new()
                } else {
                    format!(" &laquo;{}&raquo;", serialize_html_string(topic))
                };
                if *accepted {
                    format!("You approved a TON Connect request{topic_str}.")
                } else if *declined {
                    format!("You declined a TON Connect request{topic_str}.")
                } else {
                    format!("You received a TON Connect request{topic_str}.")
                }
            }
            ServiceAction::NoPaidMessagesTokenUsed => {
                format!("{service_from} started a chat via a free messages link")
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SavedSection {
    priority: i32,
    label: String,
    kind: String,
    count: i32,
    path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogsMode {
    None,
    Chats,
    Left,
}

/// HTML export writer (port of `Export::Output::HtmlWriter`).
pub struct HtmlWriter {
    settings: Settings,
    environment: Environment,
    saved_sections: Vec<SavedSection>,
    summary: Option<HtmlWrap>,
    summary_need_divider: bool,
    have_sections: bool,
    self_color_index: u8,
    delayed_personal_info: Option<PersonalInfo>,
    userpics_count: i32,
    userpics: Option<HtmlWrap>,
    stories_count: i32,
    stories: Option<HtmlWrap>,
    profile_music_count: i32,
    profile_music: Option<HtmlWrap>,
    dialogs_relative_path: String,
    dialog: DialogInfo,
    dialogs_mode: DialogsMode,
    messages_count: usize,
    last_message_info: Option<MessageInfo>,
    date_message_id: i32,
    chats: Option<HtmlWrap>,
    chat: Option<HtmlWrap>,
    last_message_ids_per_file: Vec<i32>,
    chat_file_empty: bool,
}

impl Default for HtmlWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlWriter {
    /// Creates a new `HtmlWriter`.
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            environment: Environment::default(),
            saved_sections: Vec::new(),
            summary: None,
            summary_need_divider: false,
            have_sections: false,
            self_color_index: 0,
            delayed_personal_info: None,
            userpics_count: 0,
            userpics: None,
            stories_count: 0,
            stories: None,
            profile_music_count: 0,
            profile_music: None,
            dialogs_relative_path: String::new(),
            dialog: DialogInfo::default(),
            dialogs_mode: DialogsMode::None,
            messages_count: 0,
            last_message_info: None,
            date_message_id: 0,
            chats: None,
            chat: None,
            last_message_ids_per_file: Vec::new(),
            chat_file_empty: false,
        }
    }

    fn main_file_relative_path(&self) -> &'static str {
        "export_results.html"
    }

    fn userpics_file_path(&self) -> &'static str {
        "lists/profile_pictures.html"
    }

    fn stories_file_path(&self) -> &'static str {
        "lists/stories.html"
    }

    fn profile_music_file_path(&self) -> &'static str {
        "lists/profile_music.html"
    }

    fn messages_file(index: usize) -> String {
        if index > 0 {
            format!("messages{}.html", index + 1)
        } else {
            "messages.html".to_string()
        }
    }

    fn path_with_relative_path(&self, path: &str) -> String {
        format!("{}{path}", self.settings.path)
    }

    fn file_with_relative_path(&self, path: &str) -> HtmlWrap {
        HtmlWrap::new(
            &self.path_with_relative_path(path),
            &self.settings.path,
            self.environment.utc_offset_seconds,
        )
    }

    fn push_section(&mut self, priority: i32, label: &str, kind: &str, count: i32, path: &str) {
        self.saved_sections.push(SavedSection {
            priority,
            label: label.to_string(),
            kind: kind.to_string(),
            count,
            path: path.to_string(),
        });
    }

    fn write_sections(&mut self, stats: Option<&Stats>) -> OutputResult {
        if self.saved_sections.is_empty() {
            return OutputResult::success();
        }
        let Some(summary) = self.summary.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        if !self.have_sections {
            let class = if self.summary_need_divider {
                "sections with_divider"
            } else {
                "sections"
            };
            let block = summary.push_div(class, "");
            let res = summary.write_block(&block, stats);
            if res.is_error() {
                return res;
            }
            self.have_sections = true;
            self.summary_need_divider = false;
        }
        self.saved_sections.sort_by_key(|s| s.priority);
        let sections = std::mem::take(&mut self.saved_sections);
        let mut block = String::new();
        for section in sections {
            let rel = summary.relative_path(&section.path);
            block.push_str(&summary.push_section(
                &section.label,
                &section.kind,
                section.count,
                &rel,
            ));
        }
        summary.write_block(&block, stats)
    }

    fn write_prepared_personal(
        &mut self,
        data: &PersonalInfo,
        userpic_path: &str,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let info = &data.user.info;
        let mut userpic = UserpicData {
            color_index: self.self_color_index,
            pixel_size: PERSONAL_USERPIC_SIZE,
            large_link: if userpic_path.is_empty() {
                String::new()
            } else {
                self.userpics_file_path().to_string()
            },
            first_name: info.first_name.clone(),
            last_name: info.last_name.clone(),
            ..Default::default()
        };
        userpic.image_link =
            write_userpic_thumb(&self.settings.path, userpic_path, &userpic, "_info");

        let Some(summary) = self.summary.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = summary.push_div("personal_info clearfix", "");
        block.push_str(&summary.push_div("pull_right userpic_wrap", ""));
        block.push_str(&summary.push_userpic(&userpic));
        block.push_str(&summary.pop_tag());

        let push_rows =
            |summary: &mut HtmlWrap, block: &mut String, name: &str, values: &[(&str, String)]| {
                block.push_str(&summary.push_div(&format!("rows {name}"), ""));
                for (key, value) in values {
                    if value.is_empty() {
                        continue;
                    }
                    block.push_str(&summary.push_div("row", ""));
                    block.push_str(&summary.push_div("label details", ""));
                    block.push_str(&serialize_html_string(key));
                    block.push_str(&summary.pop_tag());
                    block.push_str(&summary.push_div("value bold", ""));
                    block.push_str(&serialize_html_string(value));
                    block.push_str(&summary.pop_tag());
                    block.push_str(&summary.pop_tag());
                }
                block.push_str(&summary.pop_tag());
            };

        push_rows(
            summary,
            &mut block,
            "names",
            &[
                ("First name", info.first_name.clone()),
                ("Last name", info.last_name.clone()),
            ],
        );
        push_rows(
            summary,
            &mut block,
            "info",
            &[
                ("Phone number", format_phone_number(&info.phone_number)),
                ("Username", format_username(&data.user.username)),
            ],
        );
        push_rows(summary, &mut block, "bio", &[("Bio", data.bio.clone())]);
        block.push_str(&summary.pop_tag());
        self.summary_need_divider = true;
        summary.write_block(&block, stats)
    }

    fn write_delayed_personal(
        &mut self,
        userpic_path: &str,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let Some(info) = self.delayed_personal_info.take() else {
            return OutputResult::success();
        };
        let res = self.write_prepared_personal(&info, userpic_path, stats);
        if res.is_error() {
            return res;
        }
        if self.userpics_count != 0 {
            self.push_section(
                USERPICS_PRIORITY,
                "Profile pictures",
                "photos",
                self.userpics_count,
                self.userpics_file_path(),
            );
        }
        OutputResult::success()
    }

    fn write_saved_contacts(&mut self, data: &ContactsList, stats: Option<&Stats>) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let filename = "lists/contacts.html";
        let mut file = self.file_with_relative_path(filename);
        let mut block = file.push_header("Contacts", self.main_file_relative_path());
        block.push_str(&file.push_div("page_body list_page", ""));
        block.push_str(&file.push_about(&self.environment.about_contacts, false));
        block.push_str(&file.push_div("entry_list", ""));
        for index in sorted_contacts_indices(data) {
            let contact = &data.list[index];
            let mut userpic = UserpicData {
                color_index: contact_color_index(contact),
                pixel_size: ENTRY_USERPIC_SIZE,
                first_name: contact.first_name.clone(),
                last_name: contact.last_name.clone(),
                ..Default::default()
            };
            if contact.user_id.0 != 0 {
                let raw = contact.user_id.0 & PEER_ID_CHAT_TYPE_MASK;
                userpic.tooltip = format!("ID: {raw}");
            }
            block.push_str(&file.push_list_entry(
                &userpic,
                &compose_name(&userpic, "Deleted Account"),
                &format_phone_number(&contact.phone_number),
                &format_date_time_with_offset(
                    contact.date,
                    self.environment.utc_offset_seconds,
                    false,
                    '.',
                    ':',
                    ' ',
                ),
                "",
            ));
        }
        let res = file.write_block(&block, stats);
        if res.is_error() {
            return res;
        }
        let closed = file.close(stats);
        if closed.is_error() {
            return closed;
        }
        self.push_section(
            CONTACTS_PRIORITY,
            "Contacts",
            "contacts",
            data.list.len() as i32,
            filename,
        );
        OutputResult::success()
    }

    fn write_frequent_contacts(
        &mut self,
        data: &ContactsList,
        stats: Option<&Stats>,
    ) -> OutputResult {
        let size = data.correspondents.len() + data.inline_bots.len() + data.phone_calls.len();
        if size == 0 {
            return OutputResult::success();
        }
        let filename = "lists/frequent.html";
        let mut file = self.file_with_relative_path(filename);
        let mut block = file.push_header("Frequent contacts", self.main_file_relative_path());
        block.push_str(&file.push_div("page_body list_page", ""));
        block.push_str(&file.push_about(&self.environment.about_frequent, false));
        block.push_str(&file.push_div("entry_list", ""));

        let lists: [(&[TopPeer], &str); 3] = [
            (&data.correspondents, "people"),
            (&data.inline_bots, "inline bots"),
            (&data.phone_calls, "calls"),
        ];
        for (peers, category) in lists {
            for top in peers {
                let (name, last_name) = if top.peer.as_chat().is_some() {
                    (top.peer.name(), String::new())
                } else if let Some(u) = top.peer.as_user() {
                    if u.is_self {
                        ("Saved messages".to_string(), String::new())
                    } else {
                        (u.info.first_name.clone(), u.info.last_name.clone())
                    }
                } else {
                    (String::new(), String::new())
                };
                let userpic = UserpicData {
                    color_index: peer_color_index_peer(top.peer.id()),
                    pixel_size: ENTRY_USERPIC_SIZE,
                    first_name: name,
                    last_name,
                    ..Default::default()
                };
                block.push_str(&file.push_list_entry(
                    &userpic,
                    &compose_name(&userpic, "Deleted Account"),
                    &format!("Rating: {}", crate::data::float_to_string(top.rating)),
                    category,
                    "",
                ));
            }
        }
        let res = file.write_block(&block, stats);
        if res.is_error() {
            return res;
        }
        let closed = file.close(stats);
        if closed.is_error() {
            return closed;
        }
        self.push_section(
            FREQUENT_CONTACTS_PRIORITY,
            "Frequent contacts",
            "frequent",
            size as i32,
            filename,
        );
        OutputResult::success()
    }

    fn write_sessions(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let filename = "lists/sessions.html";
        let mut file = self.file_with_relative_path(filename);
        let mut block = file.push_header("Sessions", self.main_file_relative_path());
        block.push_str(&file.push_div("page_body list_page", ""));
        block.push_str(&file.push_about(&self.environment.about_sessions, false));
        block.push_str(&file.push_div("entry_list", ""));
        for session in &data.list {
            let app_name = if session.application_name.is_empty() {
                "Unknown"
            } else {
                session.application_name.as_str()
            };
            let name = format!("{app_name} {}", session.application_version);
            let subname = format!(
                "{}, {} {}",
                session.device_model, session.platform, session.system_version
            );
            let sep = if session.region.is_empty() || session.country.is_empty() {
                ""
            } else {
                ", "
            };
            let details = [
                format!(
                    "{} \u{2013} {}{sep}{}",
                    session.ip, session.region, session.country
                ),
                format!(
                    "Last active: {}",
                    format_date_time_with_offset(
                        session.last_active,
                        self.environment.utc_offset_seconds,
                        false,
                        '.',
                        ':',
                        ' '
                    )
                ),
                format!(
                    "Created: {}",
                    format_date_time_with_offset(
                        session.created,
                        self.environment.utc_offset_seconds,
                        false,
                        '.',
                        ':',
                        ' '
                    )
                ),
            ];
            block.push_str(&file.push_session_list_entry(
                session.application_id,
                &name,
                &subname,
                &details,
                "",
            ));
        }
        let res = file.write_block(&block, stats);
        if res.is_error() {
            return res;
        }
        let closed = file.close(stats);
        if closed.is_error() {
            return closed;
        }
        self.push_section(
            SESSIONS_PRIORITY,
            "Sessions",
            "sessions",
            data.list.len() as i32,
            filename,
        );
        OutputResult::success()
    }

    fn write_web_sessions(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        if data.web_list.is_empty() {
            return OutputResult::success();
        }
        let filename = "lists/web_sessions.html";
        let mut file = self.file_with_relative_path(filename);
        let mut block = file.push_header("Web sessions", self.main_file_relative_path());
        block.push_str(&file.push_div("page_body list_page", ""));
        block.push_str(&file.push_about(&self.environment.about_web_sessions, false));
        block.push_str(&file.push_div("entry_list", ""));
        for session in &data.web_list {
            let domain = if session.domain.is_empty() {
                "Unknown"
            } else {
                session.domain.as_str()
            };
            let subname = format!("{}, {}", session.platform, session.browser);
            let details = [
                format!("{} \u{2013} {}", session.ip, session.region),
                format!(
                    "Last active: {}",
                    format_date_time_with_offset(
                        session.last_active,
                        self.environment.utc_offset_seconds,
                        false,
                        '.',
                        ':',
                        ' '
                    )
                ),
                format!(
                    "Created: {}",
                    format_date_time_with_offset(
                        session.created,
                        self.environment.utc_offset_seconds,
                        false,
                        '.',
                        ':',
                        ' '
                    )
                ),
            ];
            let info = if session.bot_username.is_empty() {
                String::new()
            } else {
                format!("@{}", session.bot_username)
            };
            block.push_str(&file.push_session_list_entry(
                domain_application_id(&session.domain),
                domain,
                &subname,
                &details,
                &info,
            ));
        }
        let res = file.write_block(&block, stats);
        if res.is_error() {
            return res;
        }
        let closed = file.close(stats);
        if closed.is_error() {
            return closed;
        }
        self.push_section(
            WEB_SESSIONS_PRIORITY,
            "Web sessions",
            "web",
            data.web_list.len() as i32,
            filename,
        );
        OutputResult::success()
    }

    fn wrap_message_link(last_ids: &[i32], message_id: i32, text: &str) -> String {
        match last_ids.iter().position(|&max_id| message_id <= max_id) {
            None => format!(
                "<a href=\"#go_to_message{message_id}\" onclick=\"return GoToMessage({message_id})\">{text}</a>"
            ),
            Some(index) => format!(
                "<a href=\"{}#go_to_message{message_id}\">{text}</a>",
                Self::messages_file(index)
            ),
        }
    }

    fn write_dialog_opening(&mut self, index: usize, stats: Option<&Stats>) -> OutputResult {
        let name = if self.dialog.name.is_empty() && self.dialog.last_name.is_empty() {
            "Deleted Account".to_string()
        } else {
            format!("{} {}", self.dialog.name, self.dialog.last_name)
        };
        let back_path = if self.settings.only_single_peer() {
            String::new()
        } else {
            self.dialogs_relative_path.clone()
        };
        let Some(chat) = self.chat.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = chat.push_header(&name, &back_path);
        block.push_str(&chat.push_div("page_body chat_page", ""));
        block.push_str(&chat.push_div("history", ""));
        if index > 0 {
            let prev_path = Self::messages_file(index - 1);
            block.push_str(&chat.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "pagination block_link".to_string()),
                    ("href", prev_path),
                ]),
            ));
            block.push_str("Previous messages");
            block.push_str(&chat.pop_tag());
        }
        chat.write_block(&block, stats)
    }

    fn switch_to_next_chat_file(&mut self, index: usize, stats: Option<&Stats>) -> OutputResult {
        let next_path = Self::messages_file(index);
        if let Some(chat) = self.chat.as_mut() {
            let mut next = chat.push_tag(
                "a",
                BTreeMap::from([
                    ("class", "pagination block_link".to_string()),
                    ("href", next_path.clone()),
                ]),
            );
            next.push_str("Next messages");
            next.push_str(&chat.pop_tag());
            let res = chat.write_block(&next, stats);
            if res.is_error() {
                return res;
            }
            let closed = chat.close(stats);
            if closed.is_error() {
                return closed;
            }
        }
        let next_rel = format!("{}{next_path}", self.dialog.relative_path);
        self.chat = Some(self.file_with_relative_path(&next_rel));
        self.chat_file_empty = true;
        OutputResult::success()
    }

    fn write_empty_single_peer(&mut self, stats: Option<&Stats>) -> OutputResult {
        if !self.settings.only_single_peer() || self.messages_count != 0 {
            return OutputResult::success();
        }
        let res = self.write_dialog_opening(0, stats);
        if res.is_error() {
            return res;
        }
        self.date_message_id -= 1;
        let date_id = self.date_message_id;
        let dialog = self.dialog.clone();
        let base_path = self.settings.path.clone();
        let Some(chat) = self.chat.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let block =
            chat.push_service_message(date_id, &dialog, &base_path, "No exported messages", None);
        chat.write_block(&block, stats)
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
        let Some(chats) = self.chats.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        if self.dialogs_mode != DialogsMode::None {
            let pop = chats.pop_tag();
            let res = chats.write_block(&pop, stats);
            if res.is_error() {
                return res;
            }
        }
        self.dialogs_mode = mode;
        let about = if is_left_channel {
            self.environment.about_left_chats.clone()
        } else {
            self.environment.about_chats.clone()
        };
        let mut block = chats.push_about(&about, false);
        block.push_str(&chats.push_div("entry_list", ""));
        chats.write_block(&block, stats)
    }
}

impl AbstractWriter for HtmlWriter {
    fn format(&self) -> OutputFormat {
        OutputFormat::Html
    }

    fn start(
        &mut self,
        settings: &Settings,
        environment: &Environment,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.settings = settings.clone();
        self.environment = environment.clone();
        self.saved_sections.clear();
        self.summary = None;
        self.summary_need_divider = false;
        self.have_sections = false;
        self.delayed_personal_info = None;
        self.dialogs_mode = DialogsMode::None;

        for asset in EXPORT_HTML_ASSETS {
            let dst = self.path_with_relative_path(asset.relative_path);
            let res = OutputFile::write_bytes(&dst, asset.bytes, stats);
            if res.is_error() {
                return res;
            }
        }

        if self.settings.only_single_peer() {
            return OutputResult::success();
        }
        let mut summary = self.file_with_relative_path(self.main_file_relative_path());
        let mut block = summary.push_header("Exported Data", "");
        block.push_str(&summary.push_div("page_body", ""));
        let res = summary.write_block(&block, stats);
        self.summary = Some(summary);
        res
    }

    fn write_personal(&mut self, data: &PersonalInfo, stats: Option<&Stats>) -> OutputResult {
        self.self_color_index = data.user.info.color_index;
        if self.settings.types.contains(ExportTypes::USERPICS) {
            self.delayed_personal_info = Some(data.clone());
            return OutputResult::success();
        }
        self.write_prepared_personal(data, "", stats)
    }

    fn write_userpics_start(&mut self, data: &UserpicsInfo, stats: Option<&Stats>) -> OutputResult {
        self.userpics_count = data.count;
        if self.userpics_count == 0 {
            return OutputResult::success();
        }
        let mut userpics = self.file_with_relative_path(self.userpics_file_path());
        let mut block = userpics.push_header("Profile pictures", self.main_file_relative_path());
        block.push_str(&userpics.push_div("page_body list_page", ""));
        block.push_str(&userpics.push_div("entry_list", ""));
        let res = userpics.write_block(&block, stats);
        self.userpics = Some(userpics);
        if res.is_error() {
            return res;
        }
        if self.delayed_personal_info.is_none() {
            self.push_section(
                USERPICS_PRIORITY,
                "Profile pictures",
                "photos",
                self.userpics_count,
                self.userpics_file_path(),
            );
        }
        OutputResult::success()
    }

    fn write_userpics_slice(
        &mut self,
        data: &UserpicsSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let first_path = data.list[0].image.file.relative_path.clone();
        let res = self.write_delayed_personal(&first_path, stats);
        if res.is_error() {
            return res;
        }
        let base_path = self.settings.path.clone();
        let utc_offset = self.environment.utc_offset_seconds;
        let self_color = self.self_color_index;
        let Some(userpics) = self.userpics.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = String::new();
        for userpic in &data.list {
            let file = &userpic.image.file;
            let status = match file.skip_reason {
                SkipReason::Unavailable | SkipReason::DateLimits => {
                    "(Photo unavailable, please try again later)".to_string()
                }
                SkipReason::FileSize => {
                    "(Photo exceeds maximum size. Change data exporting settings to download.)"
                        .to_string()
                }
                SkipReason::FileType => {
                    "(Photo not included. Change data exporting settings to download.)".to_string()
                }
                SkipReason::None => format_file_size(file.size),
            };
            let path = &file.relative_path;
            let mut udata = UserpicData {
                color_index: self_color,
                pixel_size: ENTRY_USERPIC_SIZE,
                first_name: path.clone(),
                ..Default::default()
            };
            udata.image_link = write_userpic_thumb(&base_path, path, &udata, "_thumb");
            let date_str = if userpic.date > 0 {
                format_date_time_with_offset(userpic.date, utc_offset, false, '.', ':', ' ')
            } else {
                String::new()
            };
            let label = if path.is_empty() {
                "Photo unavailable"
            } else {
                path.as_str()
            };
            block.push_str(&userpics.push_list_entry(&udata, label, &status, &date_str, path));
        }
        userpics.write_block(&block, stats)
    }

    fn write_userpics_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let res = self.write_delayed_personal("", stats);
        if res.is_error() {
            return res;
        }
        if let Some(mut userpics) = self.userpics.take() {
            return userpics.close(stats);
        }
        OutputResult::success()
    }

    fn write_stories_start(&mut self, data: &StoriesInfo, stats: Option<&Stats>) -> OutputResult {
        self.stories_count = data.count;
        if self.stories_count == 0 {
            return OutputResult::success();
        }
        let mut stories = self.file_with_relative_path(self.stories_file_path());
        let mut block = stories.push_header("Stories archive", self.main_file_relative_path());
        block.push_str(&stories.push_div("page_body list_page", ""));
        block.push_str(&stories.push_div("entry_list", ""));
        let res = stories.write_block(&block, stats);
        self.stories = Some(stories);
        res
    }

    fn write_stories_slice(&mut self, data: &StoriesSlice, stats: Option<&Stats>) -> OutputResult {
        self.stories_count -= data.skipped;
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let base_path = self.settings.path.clone();
        let utc_offset = self.environment.utc_offset_seconds;
        let domain = self.environment.internal_links_domain.clone();
        let Some(stories) = self.stories.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = String::new();
        for story in &data.list {
            let default_file = File::default();
            let file = story.file().unwrap_or(&default_file);
            let mut status = Vec::new();
            if story.pinned {
                status.push("Saved to Profile".to_string());
            }
            if story.expires > 0 {
                status.push(format!(
                    "Expiring: {}",
                    format_date_time_with_offset(story.expires, utc_offset, false, '.', ':', ' ')
                ));
            }
            status.push(match file.skip_reason {
                SkipReason::Unavailable | SkipReason::DateLimits => {
                    "(Story unavailable, please try again later)".to_string()
                }
                SkipReason::FileSize => {
                    "(Story exceeds maximum size. Change data exporting settings to download.)"
                        .to_string()
                }
                SkipReason::FileType => {
                    "(Story not included. Change data exporting settings to download.)".to_string()
                }
                SkipReason::None => format_file_size(file.size),
            });
            let path = &file.relative_path;
            let thumb_source = story
                .thumb()
                .map(|t| t.file.relative_path.as_str())
                .filter(|p| !p.is_empty())
                .unwrap_or(path.as_str());
            let sdata = StoryData {
                image_link: write_image_thumb_exact(
                    &base_path,
                    thumb_source,
                    STORY_THUMB_WIDTH * 2,
                    STORY_THUMB_HEIGHT * 2,
                    "_thumb",
                ),
                link: path.clone(),
            };
            let info = if story.date > 0 {
                format_date_time_with_offset(story.date, utc_offset, false, '.', ':', ' ')
            } else {
                String::new()
            };
            let label = if path.is_empty() {
                "Story unavailable"
            } else {
                path.as_str()
            };
            block.push_str(&stories.push_stories_list_entry(
                &sdata,
                label,
                &status,
                &info,
                &story.caption,
                &domain,
            ));
        }
        stories.write_block(&block, stats)
    }

    fn write_stories_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.push_section(
            STORIES_PRIORITY,
            "Stories archive",
            "stories",
            self.stories_count,
            self.stories_file_path(),
        );
        if let Some(mut stories) = self.stories.take() {
            return stories.close(stats);
        }
        OutputResult::success()
    }

    fn write_profile_music_start(
        &mut self,
        data: &ProfileMusicInfo,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.profile_music_count = data.count;
        if self.profile_music_count == 0 {
            return OutputResult::success();
        }
        let mut music = self.file_with_relative_path(self.profile_music_file_path());
        let mut block = music.push_header("Profile Music", self.main_file_relative_path());
        block.push_str(&music.push_div("page_body list_page", ""));
        block.push_str(&music.push_div("entry_list", ""));
        let res = music.write_block(&block, stats);
        self.profile_music = Some(music);
        res
    }

    fn write_profile_music_slice(
        &mut self,
        data: &ProfileMusicSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.profile_music_count -= data.skipped;
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let Some(music) = self.profile_music.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = String::new();
        for message in &data.list {
            let MediaContent::Document(doc) = &message.media.content else {
                continue;
            };
            if !doc.is_audio_file {
                continue;
            }
            let file = &doc.file;
            let status = vec![match file.skip_reason {
                SkipReason::Unavailable | SkipReason::DateLimits => {
                    "(File unavailable, please try again later)".to_string()
                }
                SkipReason::FileSize => {
                    "(File exceeds maximum size. Change data exporting settings to download.)"
                        .to_string()
                }
                SkipReason::FileType => {
                    "(File not included. Change data exporting settings to download.)".to_string()
                }
                SkipReason::None => format_file_size(file.size),
            }];
            let path = &file.relative_path;
            let title = if !doc.song_title.is_empty() {
                doc.song_title.as_str()
            } else if !doc.name.is_empty() {
                doc.name.as_str()
            } else {
                "Unknown Track"
            };
            let performer = if !doc.song_performer.is_empty() {
                doc.song_performer.as_str()
            } else {
                "Unknown Artist"
            };
            let info = format!("{performer} - {title}");
            let duration = if doc.duration > 0 {
                format_duration(i64::from(doc.duration))
            } else {
                String::new()
            };
            let label = if path.is_empty() {
                "File unavailable"
            } else {
                path.as_str()
            };
            block.push_str(&music.push_audio_entry(label, &info, &status, &duration, path));
        }
        music.write_block(&block, stats)
    }

    fn write_profile_music_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.push_section(
            PROFILE_MUSIC_PRIORITY,
            "Profile Music",
            "music",
            self.profile_music_count,
            self.profile_music_file_path(),
        );
        if let Some(mut music) = self.profile_music.take() {
            return music.close(stats);
        }
        OutputResult::success()
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

    fn write_other_data(&mut self, data: &File, _stats: Option<&Stats>) -> OutputResult {
        self.push_section(
            OTHER_PRIORITY,
            "Other data",
            "other",
            1,
            &data.relative_path,
        );
        OutputResult::success()
    }

    fn write_dialogs_start(&mut self, data: &DialogsInfo, stats: Option<&Stats>) -> OutputResult {
        if (data.chats.is_empty() && data.left.is_empty()) || self.settings.only_single_peer() {
            return OutputResult::success();
        }
        self.dialogs_relative_path = "lists/chats.html".to_string();
        let mut chats = self.file_with_relative_path(&self.dialogs_relative_path);
        let mut block = chats.push_header("Chats", self.main_file_relative_path());
        block.push_str(&chats.push_div("page_body list_page", ""));
        let res = chats.write_block(&block, stats);
        self.chats = Some(chats);
        if res.is_error() {
            return res;
        }
        self.push_section(
            CHATS_PRIORITY,
            "Chats",
            "chats",
            (data.chats.len() + data.left.len()) as i32,
            "lists/chats.html",
        );
        self.write_sections(stats)
    }

    fn write_dialog_start(&mut self, data: &DialogInfo, _stats: Option<&Stats>) -> OutputResult {
        let rel = format!("{}{}", data.relative_path, Self::messages_file(0));
        self.chat = Some(self.file_with_relative_path(&rel));
        self.chat_file_empty = true;
        self.messages_count = 0;
        self.date_message_id = 0;
        self.last_message_info = None;
        self.last_message_ids_per_file.clear();
        self.dialog = data.clone();
        OutputResult::success()
    }

    fn write_dialog_slice(&mut self, data: &MessagesSlice, stats: Option<&Stats>) -> OutputResult {
        if data.list.is_empty() {
            return OutputResult::success();
        }
        let mut old_index = if self.messages_count > 0 {
            (self.messages_count - 1) / MESSAGES_IN_FILE
        } else {
            0
        };
        let mut previous = self.last_message_info.clone();
        let mut block = String::new();
        let peers_map = PeersMap::new(&data.peers);
        let base_path = self.settings.path.clone();
        let domain = self.environment.internal_links_domain.clone();
        let utc_offset = self.environment.utc_offset_seconds;

        for message in &data.list {
            if skip_message_by_date(message, &self.settings) {
                continue;
            }
            let new_index = self.messages_count / MESSAGES_IN_FILE;
            if old_index != new_index {
                if let Some(chat) = self.chat.as_mut() {
                    let res = chat.write_block(&block, stats);
                    if res.is_error() {
                        return res;
                    }
                }
                let next_res = self.switch_to_next_chat_file(new_index, stats);
                if next_res.is_error() {
                    return next_res;
                }
                if let Some(ref prev) = previous {
                    self.last_message_ids_per_file.push(prev.id);
                }
                block.clear();
                self.last_message_info = None;
                previous = None;
                old_index = new_index;
            }
            if self.chat_file_empty {
                let open_res = self.write_dialog_opening(old_index, stats);
                if open_res.is_error() {
                    return open_res;
                }
                self.chat_file_empty = false;
            }
            let prev_date = previous.as_ref().map_or(0, |p| p.date);
            if display_date(message.date, prev_date, utc_offset) {
                self.date_message_id -= 1;
                let date_id = self.date_message_id;
                let date_text = format_date_text_with_offset(message.date, utc_offset);
                if let Some(chat) = self.chat.as_mut() {
                    block.push_str(&chat.push_service_message(
                        date_id,
                        &self.dialog,
                        &base_path,
                        &date_text,
                        None,
                    ));
                }
            }
            let last_ids = &self.last_message_ids_per_file;
            let link_wrapper =
                |msg_id: i32, text: &str| Self::wrap_message_link(last_ids, msg_id, text);
            if let Some(chat) = self.chat.as_mut() {
                let (info, content) = chat.push_message(
                    message,
                    previous.as_ref(),
                    &self.dialog,
                    &base_path,
                    &peers_map,
                    &domain,
                    &link_wrapper,
                );
                block.push_str(&content);
                self.messages_count += 1;
                previous = Some(info);
            }
        }
        self.last_message_info = previous;
        if block.is_empty() {
            OutputResult::success()
        } else if let Some(chat) = self.chat.as_mut() {
            chat.write_block(&block, stats)
        } else {
            OutputResult::fatal_error(self.main_file_path())
        }
    }

    fn write_dialog_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        let res = self.write_empty_single_peer(stats);
        if res.is_error() {
            return res;
        }
        if let Some(mut chat) = self.chat.take() {
            let closed = chat.close(stats);
            if closed.is_error() {
                return closed;
            }
        }
        if self.settings.only_single_peer() {
            return OutputResult::success();
        }
        let type_str = match self.dialog.kind {
            DialogType::Unknown => "unknown",
            DialogType::SelfChat
            | DialogType::Replies
            | DialogType::VerifyCodes
            | DialogType::Personal => "private",
            DialogType::Bot => "bot",
            DialogType::PrivateGroup
            | DialogType::PrivateSupergroup
            | DialogType::PublicSupergroup => "group",
            DialogType::PrivateChannel | DialogType::PublicChannel => "channel",
        };
        let deleted_str = match self.dialog.kind {
            DialogType::Unknown
            | DialogType::SelfChat
            | DialogType::Replies
            | DialogType::VerifyCodes
            | DialogType::Personal
            | DialogType::Bot => "Deleted Account",
            DialogType::PrivateGroup
            | DialogType::PrivateSupergroup
            | DialogType::PublicSupergroup => "Deleted Group",
            DialogType::PrivateChannel | DialogType::PublicChannel => "Deleted Channel",
        };
        let name = match self.dialog.kind {
            DialogType::SelfChat => "Saved messages".to_string(),
            DialogType::Replies => "Replies".to_string(),
            DialogType::VerifyCodes => "Verification Codes".to_string(),
            _ => self.dialog.name.clone(),
        };
        let last_name = if matches!(self.dialog.kind, DialogType::Personal | DialogType::Bot) {
            self.dialog.last_name.clone()
        } else {
            String::new()
        };
        let count_str = match self.messages_count {
            1 => {
                if self.dialog.only_my_messages {
                    "1 outgoing message".to_string()
                } else {
                    "1 message".to_string()
                }
            }
            0 => {
                if self.dialog.only_my_messages {
                    "No outgoing messages".to_string()
                } else {
                    "No messages".to_string()
                }
            }
            n => {
                if self.dialog.only_my_messages {
                    format!("{n} outgoing messages")
                } else {
                    format!("{n} messages")
                }
            }
        };
        let color_index = if matches!(
            self.dialog.kind,
            DialogType::SelfChat | DialogType::Replies | DialogType::VerifyCodes
        ) {
            SAVED_MESSAGES_COLOR_INDEX
        } else {
            peer_color_index_peer(self.dialog.peer_id)
        };
        let userpic = UserpicData {
            color_index,
            pixel_size: ENTRY_USERPIC_SIZE,
            first_name: name,
            last_name,
            ..Default::default()
        };

        let mode_res = self.validate_dialogs_mode(self.dialog.is_left_channel, stats);
        if mode_res.is_error() {
            return mode_res;
        }

        let link = if self.messages_count > 0 {
            format!("{}messages.html", self.dialog.relative_path)
        } else {
            String::new()
        };
        let Some(chats) = self.chats.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let entry = chats.push_list_entry(
            &userpic,
            &compose_name(&userpic, deleted_str),
            &count_str,
            type_str,
            &link,
        );
        chats.write_block(&entry, stats)
    }

    fn write_dialogs_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        if let Some(mut chats) = self.chats.take() {
            return chats.close(stats);
        }
        OutputResult::success()
    }

    fn finish(&mut self, stats: Option<&Stats>) -> OutputResult {
        if self.settings.only_single_peer() {
            return OutputResult::success();
        }
        let res = self.write_sections(stats);
        if res.is_error() {
            return res;
        }
        let about_telegram = self.environment.about_telegram.clone();
        let Some(summary) = self.summary.as_mut() else {
            return OutputResult::fatal_error(self.main_file_path());
        };
        let mut block = String::new();
        if self.have_sections {
            block.push_str(&summary.pop_tag());
            self.summary_need_divider = true;
            self.have_sections = false;
        }
        block.push_str(&summary.push_about(&about_telegram, self.summary_need_divider));
        let write_res = summary.write_block(&block, stats);
        if write_res.is_error() {
            return write_res;
        }
        summary.close(stats)
    }

    fn main_file_path(&self) -> String {
        self.path_with_relative_path(if self.settings.only_single_peer() {
            "messages.html"
        } else {
            self.main_file_relative_path()
        })
    }
}
