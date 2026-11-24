//! Port of `iv/iv_rich_page.h`, `iv/iv_rich_page.cpp`, `iv/iv_rich_message_serializer.h`,
//! `iv/iv_rich_message_serializer.cpp`, and `iv/markdown/iv_markdown_prepare_serialize.cpp`.
//!
//! Defines the editable [`RichPage`] document representation used by the native Markdown
//! article renderer and rich document editor, together with faithful Layer-233 TL
//! `PageBlock` / `RichText` / `RichMessage` / `InputRichMessage` serialization and
//! deserialization, message-limit validation, inline text object encoding, and text
//! flattening helpers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::model::{
    COLLAGE_MAX_ITEMS, DEFAULT_MAP_HEIGHT, DEFAULT_MAP_WIDTH, DateFlags, GeoPoint, ListItemContent,
    MAX_NESTING_DEPTH, Page, PageBlock, PageCaption, PageChannel, PageListItem,
    PageListOrderedItem, PageRelatedArticle, PageTableCell, PageTableRow,
    RichText as ModelRichText, TableAlign, TableValign, TaskState as ModelTaskState,
    normalize_fragment_id, percent_decode_utf8, trim_math_source,
};

/// Maximum block nesting depth allowed while parsing TL `PageBlock` trees (`kMaxParsedBlockDepth`).
pub const MAX_PARSED_BLOCK_DEPTH: usize = MAX_NESTING_DEPTH;

/// Unicode Object Replacement Character (`U+FFFC`) used as the placeholder for inline objects
/// (formulas, inline IV images, inline buttons) inside [`TextWithEntities`].
pub const OBJECT_REPLACEMENT_CHAR: char = '\u{FFFC}';

/// Special text color index for inserted spans in `RichParseMode::DisplayTextDiff`.
pub const TEXT_DIFF_INSERTED_COLOR_INDEX: u16 = 10;

/// Special text color index for deleted spans in `RichParseMode::DisplayTextDiff`.
pub const TEXT_DIFF_DELETED_COLOR_INDEX: u16 = 11;

/// Generator marker matching `Iv::RichExportGeneratorMarker()`.
pub const RICH_EXPORT_GENERATOR_MARKER: &str = "Telegram Desktop rich message export";

/// Inline entity types carried by [`TextWithEntities`], mirroring tdesktop's `EntityType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityType {
    /// Bold (`textBold`).
    Bold,
    /// Italic (`textItalic`).
    Italic,
    /// Underline (`textUnderline`).
    Underline,
    /// Strikethrough (`textStrike`).
    StrikeOut,
    /// Inline monospace code (`textFixed`).
    Code,
    /// Preformatted code block (block-level entity in simple messages).
    Pre,
    /// Blockquote (block-level entity in simple messages).
    Blockquote,
    /// Subscript (`textSubscript`).
    Subscript,
    /// Superscript (`textSuperscript`).
    Superscript,
    /// Highlighted / marked (`textMarked`).
    Marked,
    /// Spoiler (`textSpoiler`).
    Spoiler,
    /// Explicit link (`textUrl`, `mailto:` for `textEmail`, `tel:` for `textPhone`).
    CustomUrl,
    /// Auto-detected URL (`textAutoUrl`).
    Url,
    /// Auto-detected email (`textAutoEmail`).
    Email,
    /// Auto-detected phone (`textAutoPhone`).
    Phone,
    /// `@username` mention (`textMention`).
    Mention,
    /// User mention by ID (`textMentionName`).
    MentionName,
    /// `#hashtag` (`textHashtag`).
    Hashtag,
    /// `/command` (`textBotCommand`).
    BotCommand,
    /// `$TICKER` cashtag (`textCashtag`).
    Cashtag,
    /// Bank card number (`textBankCard`).
    BankCard,
    /// TON address (`textTonAddress`).
    TonAddress,
    /// Custom emoji or serialized inline text object (`iv-markdown:inline-text-object;...`).
    CustomEmoji,
    /// Formatted date/time (`textDate`).
    FormattedDate,
    /// Colorized text (used for `textDiff` display highlights).
    Colorized,
}

impl EntityType {
    /// Returns the deterministic serialization priority matching `EntitySerializationOrder`
    /// in `iv_rich_message_serializer.cpp`.
    #[must_use]
    pub const fn serialization_order(self) -> u8 {
        match self {
            Self::CustomUrl => 0,
            Self::MentionName => 1,
            Self::Bold => 2,
            Self::Italic => 3,
            Self::Underline => 4,
            Self::StrikeOut => 5,
            Self::Code => 6,
            Self::Subscript => 7,
            Self::Superscript => 8,
            Self::Marked => 9,
            Self::Spoiler => 10,
            Self::CustomEmoji => 11,
            Self::FormattedDate => 12,
            Self::Mention => 13,
            Self::Hashtag => 14,
            Self::BotCommand => 15,
            Self::Cashtag => 16,
            Self::Url => 17,
            Self::Email => 18,
            Self::Phone => 19,
            Self::BankCard => 20,
            Self::TonAddress => 21,
            Self::Pre | Self::Blockquote | Self::Colorized => 100,
        }
    }
}

/// A single inline formatting entity spanning `[offset, offset + length)` in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityInText {
    /// Entity kind.
    pub kind: EntityType,
    /// Start offset in UTF-16 code units.
    pub offset: usize,
    /// Span length in UTF-16 code units.
    pub length: usize,
    /// Optional auxiliary payload (URL, language, user ID, inline object payload, etc.).
    pub data: String,
}

impl EntityInText {
    /// Creates a new entity with empty auxiliary `data`.
    #[must_use]
    pub fn new(kind: EntityType, offset: usize, length: usize) -> Self {
        Self {
            kind,
            offset,
            length,
            data: String::new(),
        }
    }

    /// Creates a new entity with explicit auxiliary `data`.
    #[must_use]
    pub fn with_data(
        kind: EntityType,
        offset: usize,
        length: usize,
        data: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            offset,
            length,
            data: data.into(),
        }
    }

    /// Exclusive end offset in UTF-16 code units.
    #[must_use]
    pub const fn end(&self) -> usize {
        self.offset.saturating_add(self.length)
    }
}

/// Plain text paired with UTF-16 entity spans (`TextWithEntities` in tdesktop).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextWithEntities {
    /// UTF-8 text payload.
    pub text: String,
    /// Formatting and semantic entities indexed in UTF-16 code units.
    pub entities: Vec<EntityInText>,
}

impl TextWithEntities {
    /// Creates plain unformatted text.
    #[must_use]
    pub fn simple(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            entities: Vec::new(),
        }
    }

    /// Returns true if the underlying text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns the length of `self.text` in UTF-16 code units.
    #[must_use]
    pub fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    /// Appends plain text without entities.
    pub fn push_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// Appends another [`TextWithEntities`], shifting its entity offsets by `self.utf16_len()`.
    pub fn append(&mut self, other: TextWithEntities) {
        let shift = self.utf16_len();
        self.text.push_str(&other.text);
        self.entities.reserve(other.entities.len());
        for mut entity in other.entities {
            entity.offset = entity.offset.saturating_add(shift);
            self.entities.push(entity);
        }
    }

    /// Sorts entities by ascending offset, descending length, and deterministic entity order.
    pub fn sort_entities(&mut self) {
        let total = self.utf16_len();
        self.entities.retain(|e| e.length > 0 && e.end() <= total);
        self.entities.sort_by(|a, b| {
            a.offset
                .cmp(&b.offset)
                .then_with(|| b.length.cmp(&a.length))
                .then_with(|| {
                    a.kind
                        .serialization_order()
                        .cmp(&b.kind.serialization_order())
                })
        });
    }

    /// Trims leading and trailing whitespace while adjusting entity offsets and lengths.
    pub fn trim_in_place(&mut self) {
        let trimmed_start = self.text.trim_start();
        let leading_bytes = self.text.len() - trimmed_start.len();
        let leading_u16 = self.text[..leading_bytes].encode_utf16().count();
        let trimmed = trimmed_start.trim_end();
        let kept_u16 = trimmed.encode_utf16().count();
        if leading_bytes == 0 && trimmed.len() == self.text.len() {
            return;
        }
        let end_u16 = leading_u16 + kept_u16;
        self.text = trimmed.to_string();
        let mut adjusted = Vec::with_capacity(self.entities.len());
        for mut e in std::mem::take(&mut self.entities) {
            let start = e.offset.max(leading_u16);
            let end = e.end().min(end_u16);
            if end > start {
                e.offset = start - leading_u16;
                e.length = end - start;
                adjusted.push(e);
            }
        }
        self.entities = adjusted;
    }
}

/// Converts a UTF-16 offset into a UTF-8 byte offset in `s`, clamping to `s.len()`.
#[must_use]
pub fn utf16_to_byte_offset(s: &str, utf16_offset: usize) -> usize {
    let mut u16_pos = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if u16_pos >= utf16_offset {
            return byte_idx;
        }
        u16_pos += ch.len_utf16();
    }
    s.len()
}

/// Converts a UTF-8 byte offset into a UTF-16 code-unit offset.
#[must_use]
pub fn byte_to_utf16_offset(s: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(s.len());
    let mut boundary = clamped;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    s[..boundary].encode_utf16().count()
}

/// Slices a [`TextWithEntities`] by UTF-16 range `[from, till)`.
#[must_use]
pub fn slice_text_with_entities(
    text: &TextWithEntities,
    from: usize,
    till: usize,
) -> TextWithEntities {
    let total = text.utf16_len();
    let start = from.min(total);
    let end = till.min(total).max(start);
    let byte_start = utf16_to_byte_offset(&text.text, start);
    let byte_end = utf16_to_byte_offset(&text.text, end);
    let sliced = text.text[byte_start..byte_end].to_string();
    let mut entities = Vec::new();
    for e in &text.entities {
        let e_start = e.offset.max(start);
        let e_end = e.end().min(end);
        if e_end > e_start {
            entities.push(EntityInText {
                kind: e.kind,
                offset: e_start - start,
                length: e_end - e_start,
                data: e.data.clone(),
            });
        }
    }
    TextWithEntities {
        text: sliced,
        entities,
    }
}

// ============================================================================
// Inline Text Object serialization (`iv_markdown_prepare_serialize.cpp`)
// ============================================================================

/// Kind of inline text object stored inside a `CustomEmoji` entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineTextObjectKind {
    /// Inline TeX math formula (`textMath`).
    Formula,
    /// Inline Instant View image (`textImage`).
    IvImage,
    /// Inline rich button (`textButton`).
    Button,
}

/// Payload for an inline math formula object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InlineTextObjectFormulaData {
    /// Delimited source copied to clipboard (e.g. `"$x^2$"`).
    pub copy_source: String,
    /// Trimmed TeX expression without surrounding `$...$`.
    pub trimmed_tex: String,
}

/// Payload for an inline Instant View image object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InlineTextObjectIvImageData {
    /// Telegram document ID.
    pub document_id: u64,
    /// Image width in logical pixels.
    pub width: i32,
    /// Image height in logical pixels.
    pub height: i32,
    /// Fallback plain-text label (usually `"[image]"`).
    pub replacement_text: String,
}

/// Visual color variant for a rich button (`RichButtonStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichButtonColor {
    /// Default neutral button.
    #[default]
    Normal,
    /// Primary accent background (`bg_primary`).
    Primary,
    /// Destructive / danger background (`bg_danger`).
    Danger,
    /// Positive / success background (`bg_success`).
    Success,
}

/// Action kind for a rich message button (`InlineButtonType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichButtonType {
    /// Non-interactive / disabled button (`inlineButtonTypeDisabled`).
    #[default]
    Disabled,
    /// Opens a URL (`inlineButtonTypeUrl`).
    Url,
    /// Copies text to clipboard (`inlineButtonTypeCopy`).
    CopyText,
    /// Opens a user profile (`inlineButtonTypeUserProfile`).
    UserProfile,
    /// Callback button (`inlineButtonTypeCallback`).
    Callback,
    /// Password-gated callback button (`inlineButtonTypeCallback` with `requires_password`).
    CallbackWithPassword,
    /// WebView URL (`inlineButtonTypeWebView`).
    WebView,
    /// URL authorization (`inlineButtonTypeUrlAuth`).
    UrlAuth,
    /// Game launch (`inlineButtonTypeGame`).
    Game,
    /// Payment button (`inlineButtonTypeBuy`).
    Buy,
    /// Switch inline query (`inlineButtonTypeSwitchInline`).
    SwitchInline,
}

/// Payload for an inline rich text button object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InlineTextObjectButtonData {
    /// Button display label.
    pub label: TextWithEntities,
    /// Button action data (URL, copy text, user ID string, callback bytes).
    pub data: Vec<u8>,
    /// Optional button ID (for URL auth).
    pub button_id: i64,
    /// Button action kind.
    pub button_type: RichButtonType,
    /// Button background color style.
    pub color: RichButtonColor,
    /// Peer-type bitmask for switch-inline buttons.
    pub peer_types: u32,
    /// Render inline as a link-style button (`RichButtonStyle::link`).
    pub link: bool,
}

/// Parsed inline text object stored in a [`EntityType::CustomEmoji`] entity payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineTextObjectEntity {
    /// Inline math formula.
    Formula(InlineTextObjectFormulaData),
    /// Inline IV image.
    IvImage(InlineTextObjectIvImageData),
    /// Inline rich button.
    Button(InlineTextObjectButtonData),
}

impl InlineTextObjectEntity {
    /// Returns the object kind discriminant.
    #[must_use]
    pub const fn kind(&self) -> InlineTextObjectKind {
        match self {
            Self::Formula(_) => InlineTextObjectKind::Formula,
            Self::IvImage(_) => InlineTextObjectKind::IvImage,
            Self::Button(_) => InlineTextObjectKind::Button,
        }
    }
}

/// Wraps a trimmed TeX string with `$...$` for clipboard copy (`InlineFormulaCopySource`).
#[must_use]
pub fn inline_formula_copy_source(source: &str) -> String {
    format!("${source}$")
}

fn percent_encode_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

fn percent_decode_bytes(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn serialize_rich_button_label(text: &TextWithEntities) -> String {
    let mut out = percent_encode_bytes(text.text.as_bytes());
    for entity in &text.entities {
        let name = match entity.kind {
            EntityType::CustomEmoji => "custom-emoji",
            EntityType::FormattedDate => "formatted-date",
            _ => continue,
        };
        out.push('|');
        out.push_str(name);
        out.push(',');
        out.push_str(&entity.offset.to_string());
        out.push(',');
        out.push_str(&entity.length.to_string());
        out.push(',');
        out.push_str(&percent_encode_bytes(entity.data.as_bytes()));
    }
    out
}

fn parse_rich_button_label(data: &str) -> TextWithEntities {
    let mut parts = data.split('|');
    let Some(first) = parts.next() else {
        return TextWithEntities::default();
    };
    let text = String::from_utf8_lossy(&percent_decode_bytes(first)).into_owned();
    let mut result = TextWithEntities::simple(text);
    let max_u16 = result.utf16_len();
    for part in parts {
        let fields: Vec<&str> = part.split(',').collect();
        if fields.len() != 4 {
            continue;
        }
        let kind = match fields[0] {
            "custom-emoji" => EntityType::CustomEmoji,
            "formatted-date" => EntityType::FormattedDate,
            _ => continue,
        };
        let Ok(offset) = fields[1].parse::<usize>() else {
            continue;
        };
        let Ok(length) = fields[2].parse::<usize>() else {
            continue;
        };
        if length == 0 || offset.saturating_add(length) > max_u16 {
            continue;
        }
        let decoded_data = String::from_utf8_lossy(&percent_decode_bytes(fields[3])).into_owned();
        result
            .entities
            .push(EntityInText::with_data(kind, offset, length, decoded_data));
    }
    result
}

const fn button_color_to_int(c: RichButtonColor) -> u8 {
    match c {
        RichButtonColor::Normal => 0,
        RichButtonColor::Primary => 1,
        RichButtonColor::Danger => 2,
        RichButtonColor::Success => 3,
    }
}

const fn button_color_from_int(v: u8) -> Option<RichButtonColor> {
    match v {
        0 => Some(RichButtonColor::Normal),
        1 => Some(RichButtonColor::Primary),
        2 => Some(RichButtonColor::Danger),
        3 => Some(RichButtonColor::Success),
        _ => None,
    }
}

const fn button_type_to_int(t: RichButtonType) -> u8 {
    match t {
        RichButtonType::Disabled => 0,
        RichButtonType::Url => 1,
        RichButtonType::CopyText => 2,
        RichButtonType::UserProfile => 3,
        RichButtonType::Callback => 4,
        RichButtonType::CallbackWithPassword => 5,
        RichButtonType::WebView => 6,
        RichButtonType::UrlAuth => 7,
        RichButtonType::Game => 8,
        RichButtonType::Buy => 9,
        RichButtonType::SwitchInline => 10,
    }
}

const fn button_type_from_int(v: u8) -> Option<RichButtonType> {
    match v {
        0 => Some(RichButtonType::Disabled),
        1 => Some(RichButtonType::Url),
        2 => Some(RichButtonType::CopyText),
        3 => Some(RichButtonType::UserProfile),
        4 => Some(RichButtonType::Callback),
        5 => Some(RichButtonType::CallbackWithPassword),
        6 => Some(RichButtonType::WebView),
        7 => Some(RichButtonType::UrlAuth),
        8 => Some(RichButtonType::Game),
        9 => Some(RichButtonType::Buy),
        10 => Some(RichButtonType::SwitchInline),
        _ => None,
    }
}

/// Serializes an [`InlineTextObjectEntity`] to tdesktop's `iv-markdown:inline-text-object;...`
/// string format (`SerializeInlineTextObjectEntity`).
#[must_use]
pub fn serialize_inline_text_object_entity(object: &InlineTextObjectEntity) -> String {
    match object {
        InlineTextObjectEntity::Formula(data) => format!(
            "iv-markdown:inline-text-object;formula;{};{}",
            percent_encode_bytes(data.copy_source.as_bytes()),
            percent_encode_bytes(data.trimmed_tex.as_bytes()),
        ),
        InlineTextObjectEntity::IvImage(data) => format!(
            "iv-markdown:inline-text-object;iv-image;{};{};{};{}",
            data.document_id,
            data.width,
            data.height,
            percent_encode_bytes(data.replacement_text.as_bytes()),
        ),
        InlineTextObjectEntity::Button(data) => {
            let label = serialize_rich_button_label(&data.label);
            format!(
                "iv-markdown:inline-text-object;button;{};{};{};{};{};{};{}",
                percent_encode_bytes(label.as_bytes()),
                button_color_to_int(data.color),
                button_type_to_int(data.button_type),
                if data.link { 1 } else { 0 },
                percent_encode_bytes(&data.data),
                data.button_id,
                data.peer_types,
            )
        }
    }
}

/// Parses an `iv-markdown:inline-text-object;...` entity payload (`ParseInlineTextObjectEntity`).
#[must_use]
pub fn parse_inline_text_object_entity(data: &str) -> Option<InlineTextObjectEntity> {
    let parts: Vec<&str> = data.split(';').collect();
    if parts.len() < 2 || parts[0] != "iv-markdown:inline-text-object" {
        return None;
    }
    match parts[1] {
        "formula" => {
            if parts.len() != 4 {
                return None;
            }
            Some(InlineTextObjectEntity::Formula(
                InlineTextObjectFormulaData {
                    copy_source: String::from_utf8_lossy(&percent_decode_bytes(parts[2]))
                        .into_owned(),
                    trimmed_tex: String::from_utf8_lossy(&percent_decode_bytes(parts[3]))
                        .into_owned(),
                },
            ))
        }
        "iv-image" => {
            if parts.len() != 6 {
                return None;
            }
            let document_id = parts[2].parse::<u64>().ok()?;
            let width = parts[3].parse::<i32>().ok()?;
            let height = parts[4].parse::<i32>().ok()?;
            Some(InlineTextObjectEntity::IvImage(
                InlineTextObjectIvImageData {
                    document_id,
                    width,
                    height,
                    replacement_text: String::from_utf8_lossy(&percent_decode_bytes(parts[5]))
                        .into_owned(),
                },
            ))
        }
        "button" => {
            if parts.len() != 9 {
                return None;
            }
            let color = button_color_from_int(parts[3].parse::<u8>().ok()?)?;
            let button_type = button_type_from_int(parts[4].parse::<u8>().ok()?)?;
            let link = parts[5] == "1";
            let raw_data = percent_decode_bytes(parts[6]);
            let button_id = parts[7].parse::<i64>().ok()?;
            let peer_types = parts[8].parse::<u32>().ok()?;
            let label_str = String::from_utf8_lossy(&percent_decode_bytes(parts[2])).into_owned();
            Some(InlineTextObjectEntity::Button(InlineTextObjectButtonData {
                label: parse_rich_button_label(&label_str),
                data: raw_data,
                button_id,
                button_type,
                color,
                peer_types,
                link,
            }))
        }
        _ => None,
    }
}

/// Expands inline text objects (`\u{FFFC}` placeholders) in `text` to their plain-text
/// replacements (`trimmed_tex`, `replacement_text`, or button label text), mirroring
/// `ExpandInlineTextObjects` in `iv_markdown_prepare_serialize.cpp`.
pub fn expand_inline_text_objects(text: &mut TextWithEntities) {
    let mut i = 0;
    while i < text.entities.len() {
        if text.entities[i].kind != EntityType::CustomEmoji {
            i += 1;
            continue;
        }
        let Some(object) = parse_inline_text_object_entity(&text.entities[i].data) else {
            i += 1;
            continue;
        };
        let (replacement, nested) = match object {
            InlineTextObjectEntity::Formula(f) => (f.trimmed_tex, Vec::new()),
            InlineTextObjectEntity::IvImage(img) => (img.replacement_text, Vec::new()),
            InlineTextObjectEntity::Button(btn) => (btn.label.text, btn.label.entities),
        };
        let offset = text.entities[i].offset;
        let old_len = text.entities[i].length;
        let new_len = replacement.encode_utf16().count();
        let byte_start = utf16_to_byte_offset(&text.text, offset);
        let byte_end = utf16_to_byte_offset(&text.text, offset + old_len);
        text.text.replace_range(byte_start..byte_end, &replacement);
        text.entities.remove(i);

        let delta = new_len as isize - old_len as isize;
        for entity in &mut text.entities {
            if entity.offset > offset {
                entity.offset = (entity.offset as isize + delta).max(0) as usize;
            } else if entity.end() > offset {
                entity.length = (entity.length as isize + delta).max(0) as usize;
            }
        }
        for (idx, mut n) in nested.into_iter().enumerate() {
            n.offset += offset;
            text.entities.insert(i + idx, n);
        }
        i += 0; // re-check shifted entities
    }
    text.entities.retain(|e| e.length > 0);
}

// ============================================================================
// RichPage data model (`iv/iv_rich_page.h`)
// ============================================================================

/// Rich text leaf with optional anchor IDs (`Iv::RichPage::RichText`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RichTextNode {
    /// Formatted text with UTF-16 entity spans.
    pub text: TextWithEntities,
    /// Primary normalized anchor ID, if any.
    pub anchor_id: String,
    /// Additional anchor IDs attached to this leaf.
    pub anchor_ids: Vec<String>,
}

impl RichTextNode {
    /// Creates a plain unformatted [`RichTextNode`].
    #[must_use]
    pub fn plain(s: impl Into<String>) -> Self {
        Self {
            text: TextWithEntities::simple(s),
            anchor_id: String::new(),
            anchor_ids: Vec::new(),
        }
    }

    /// Creates a [`RichTextNode`] from [`TextWithEntities`].
    #[must_use]
    pub fn from_text(text: TextWithEntities) -> Self {
        Self {
            text,
            anchor_id: String::new(),
            anchor_ids: Vec::new(),
        }
    }

    /// Returns true if the text has no non-whitespace characters and no anchor IDs.
    #[must_use]
    pub fn is_structurally_empty(&self) -> bool {
        self.text.text.trim().is_empty() && !self.has_anchor_payload()
    }

    /// Returns true if the text contains at least one non-whitespace character.
    #[must_use]
    pub fn has_visible_text(&self) -> bool {
        !self.text.text.trim().is_empty()
    }

    /// Returns true if any anchor ID is attached.
    #[must_use]
    pub fn has_anchor_payload(&self) -> bool {
        !self.anchor_id.is_empty() || !self.anchor_ids.is_empty()
    }

    /// Returns true if either text or anchor payload is present.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.text.is_empty() || self.has_anchor_payload()
    }

    /// Adds a deduplicated anchor ID (`AddRichAnchor`).
    pub fn add_anchor(&mut self, anchor_id: String) {
        if anchor_id.is_empty() {
            return;
        }
        if self.anchor_id.is_empty() {
            self.anchor_id = anchor_id;
        } else if self.anchor_id != anchor_id && !self.anchor_ids.contains(&anchor_id) {
            self.anchor_ids.push(anchor_id);
        }
    }
}

/// Discriminant for every block kind in [`RichPage`] (`Iv::RichPage::BlockKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RichBlockKind {
    /// Unsupported block (`pageBlockUnsupported`).
    #[default]
    Unsupported,
    /// Heading levels 1..=6 (`pageBlockHeading1..6`, `Title`, `Subtitle`, `Header`, `Subheader`, `Kicker`).
    Heading,
    /// Body paragraph (`pageBlockParagraph`).
    Paragraph,
    /// Footer text (`pageBlockFooter`).
    Footer,
    /// Reasoning / thinking callout (`pageBlockThinking`).
    Thinking,
    /// Author & publication date (`pageBlockAuthorDate`).
    AuthorDate,
    /// Preformatted code block (`pageBlockPreformatted`).
    Code,
    /// Horizontal divider (`pageBlockDivider`).
    Divider,
    /// Standalone named anchor (`pageBlockAnchor`).
    Anchor,
    /// Interactive button row (`pageBlockButtonRow`).
    ButtonRow,
    /// Bullet, task, or ordered list (`pageBlockList`, `pageBlockOrderedList`).
    List,
    /// Blockquote or pullquote (`pageBlockBlockquote`, `pageBlockBlockquoteBlocks`, `pageBlockPullquote`).
    Quote,
    /// Single photo (`pageBlockPhoto`).
    Photo,
    /// Single video (`pageBlockVideo`).
    Video,
    /// External embed (`pageBlockEmbed`).
    Embed,
    /// Embedded social post (`pageBlockEmbedPost`).
    EmbedPost,
    /// Collage or slideshow media group (`pageBlockCollage`, `pageBlockSlideshow`).
    GroupedMedia,
    /// Channel card (`pageBlockChannel`).
    Channel,
    /// Audio track (`pageBlockAudio`).
    Audio,
    /// Generic downloadable document (`pageBlockDocument`).
    File,
    /// Display TeX math block (`pageBlockMath`).
    Math,
    /// Structured table (`pageBlockTable`).
    Table,
    /// Collapsible details section (`pageBlockDetails`).
    Details,
    /// Related articles list (`pageBlockRelatedArticles`).
    RelatedArticles,
    /// Geographic map (`pageBlockMap`, `inputPageBlockMap`).
    Map,
}

/// List numbering kind (`Iv::RichPage::ListKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichListKind {
    /// Unordered bullet or task list.
    #[default]
    Bullet,
    /// Ordered numbered/alphabetic/Roman list.
    Ordered,
}

/// Task checkbox state (`Iv::RichPage::TaskState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichTaskState {
    /// Regular list item without a checkbox.
    #[default]
    None,
    /// Unchecked task checkbox.
    Unchecked,
    /// Checked task checkbox.
    Checked,
}

impl RichTaskState {
    /// Builds a [`RichTaskState`] from TL `checkbox` and `checked` flags.
    #[must_use]
    pub const fn from_flags(checkbox: bool, checked: bool) -> Self {
        if !checkbox {
            Self::None
        } else if checked {
            Self::Checked
        } else {
            Self::Unchecked
        }
    }
}

/// Ordered list container metadata (`Iv::RichPage::OrderedListData`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OrderedListData {
    /// Reverse numbering flag.
    pub reversed: bool,
    /// Optional explicit start number.
    pub start: Option<i32>,
    /// Optional list marker type (`"1"`, `"a"`, `"A"`, `"i"`, `"I"`).
    pub list_type: Option<String>,
}

/// Per-item ordered list numbering override (`Iv::RichPage::OrderedListItemData`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OrderedListItemData {
    /// Raw server-supplied marker text (e.g. `"1"`, `"a"`).
    pub num: Option<String>,
    /// Explicit numeric item value.
    pub value: Option<i32>,
    /// Explicit marker type override.
    pub list_type: Option<String>,
}

impl OrderedListItemData {
    /// Returns true if a non-empty raw marker string is present.
    #[must_use]
    pub fn has_raw_text(&self) -> bool {
        self.num.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// Grouping intent for a [`RichBlockKind::GroupedMedia`] block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GroupedMediaIntent {
    /// Multi-photo/video collage grid (`pageBlockCollage`).
    #[default]
    Collage,
    /// Interactive slideshow carousel (`pageBlockSlideshow`).
    Slideshow,
}

/// Horizontal alignment for table cells (`Iv::RichPage::TableAlignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichTableAlignment {
    /// Left-aligned.
    #[default]
    Left,
    /// Center-aligned.
    Center,
    /// Right-aligned.
    Right,
}

/// Vertical alignment for table cells (`Iv::RichPage::TableVerticalAlignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichTableVerticalAlignment {
    /// Top-aligned.
    #[default]
    Top,
    /// Vertically centered.
    Middle,
    /// Bottom-aligned.
    Bottom,
}

/// Alignment for a [`RichBlockKind::ButtonRow`] (`Iv::RichPage::ButtonAlignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RichButtonAlignment {
    /// Stretch buttons across row width.
    #[default]
    Stretch,
    /// Align buttons to the left.
    Left,
    /// Center buttons.
    Center,
    /// Align buttons to the right.
    Right,
}

/// A single item inside a [`RichBlockKind::List`] (`Iv::RichPage::ListItem`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RichListItem {
    /// Optional task checkbox state.
    pub task_state: RichTaskState,
    /// Ordered list marker overrides.
    pub number: OrderedListItemData,
    /// Optional anchor ID.
    pub anchor_id: String,
    /// Inline rich text body (when item is a single text line).
    pub text: RichTextNode,
    /// Nested child blocks (for multi-block list items).
    pub blocks: Vec<RichBlock>,
}

/// One photo or video item inside a [`RichBlockKind::GroupedMedia`] (`Iv::RichPage::GroupedMediaItem`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GroupedMediaItem {
    /// Must be [`RichBlockKind::Photo`] or [`RichBlockKind::Video`].
    pub kind: RichBlockKind,
    /// Photo ID (when `kind == Photo`).
    pub photo_id: u64,
    /// Document ID (when `kind == Video`).
    pub document_id: u64,
    /// Intrinsic width in pixels.
    pub width: i32,
    /// Intrinsic height in pixels.
    pub height: i32,
    /// Autoplay flag for videos.
    pub autoplay: bool,
    /// Loop flag for videos.
    pub loop_video: bool,
    /// Spoiler blur flag.
    pub spoiler: bool,
}

/// A single cell in a [`RichTableRow`] (`Iv::RichPage::TableCell`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichTableCell {
    /// Cell rich text.
    pub text: RichTextNode,
    /// Column span (`>= 1`).
    pub colspan: i32,
    /// Row span (`>= 1`).
    pub rowspan: i32,
    /// Header cell flag (`<th>`).
    pub header: bool,
    /// Horizontal text alignment.
    pub alignment: RichTableAlignment,
    /// Vertical alignment.
    pub vertical_alignment: RichTableVerticalAlignment,
}

impl Default for RichTableCell {
    fn default() -> Self {
        Self {
            text: RichTextNode::default(),
            colspan: 1,
            rowspan: 1,
            header: false,
            alignment: RichTableAlignment::Left,
            vertical_alignment: RichTableVerticalAlignment::Top,
        }
    }
}

/// A row in a [`RichBlockKind::Table`] (`Iv::RichPage::TableRow`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RichTableRow {
    /// Cells in this row.
    pub cells: Vec<RichTableCell>,
}

/// Related article card inside [`RichBlockKind::RelatedArticles`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RichRelatedArticle {
    /// Target URL.
    pub url: String,
    /// Instant View webpage ID.
    pub webpage_id: u64,
    /// Preview photo ID.
    pub photo_id: u64,
    /// Card title.
    pub title: String,
    /// Card description snippet.
    pub description: String,
    /// Author name.
    pub author: String,
    /// Publication timestamp (Unix seconds).
    pub published_date: i32,
}

/// A button inside a [`RichBlockKind::ButtonRow`] (`Iv::RichPage::Button`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RichButton {
    /// Button display label.
    pub text: RichTextNode,
    /// Action kind.
    pub button_type: RichButtonType,
    /// Background color variant.
    pub color: RichButtonColor,
    /// Raw action data (URL, copy text, user ID string, callback bytes).
    pub data: Vec<u8>,
    /// Optional button ID.
    pub button_id: i64,
    /// Peer-type bitmask for switch-inline buttons.
    pub peer_types: u32,
}

/// One block in a [`RichPage`] (`Iv::RichPage::Block`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RichBlock {
    /// Block kind.
    pub kind: RichBlockKind,
    /// Block-level normalized anchor ID.
    pub anchor_id: String,
    /// Primary rich text (paragraph, heading, footer, code body, table title, details summary, quote text).
    pub text: RichTextNode,
    /// Secondary caption/attribution text (quotes, media, maps, grouped media).
    pub caption: RichTextNode,
    /// Code block language tag.
    pub language: String,
    /// Display math TeX source.
    pub formula: String,
    /// URL (photo click-through, embed, embed post).
    pub url: String,
    /// Raw embed HTML bytes.
    pub html: Vec<u8>,
    /// Author name (embed post).
    pub author: String,
    /// Channel username (without `@`).
    pub username: String,
    /// Channel display title.
    pub channel_title: String,
    /// Audio track title.
    pub audio_title: String,
    /// Audio performer.
    pub audio_performer: String,
    /// Document file name.
    pub file_name: String,
    /// Timestamp (Unix seconds).
    pub date: i32,
    /// Audio duration in seconds.
    pub audio_duration: i32,
    /// Heading level `1..=6` (when `kind == Heading`).
    pub heading_level: u8,
    /// Intrinsic width in pixels.
    pub width: i32,
    /// Intrinsic height in pixels.
    pub height: i32,
    /// Map zoom level (`1..=20`).
    pub zoom: i32,
    /// Photo ID.
    pub photo_id: u64,
    /// Document ID.
    pub document_id: u64,
    /// Channel ID.
    pub channel_id: u64,
    /// Full-width embed flag.
    pub full_width: bool,
    /// Fixed-height embed flag.
    pub fixed_height: bool,
    /// Allow scrolling in embed.
    pub allow_scrolling: bool,
    /// Autoplay video flag.
    pub autoplay: bool,
    /// Loop video flag.
    pub loop_video: bool,
    /// Spoiler blur flag.
    pub spoiler: bool,
    /// Details expanded-by-default flag.
    pub open: bool,
    /// Table bordered flag.
    pub bordered: bool,
    /// Table striped flag.
    pub striped: bool,
    /// Table compact flag.
    pub compact: bool,
    /// Pullquote flag (for `kind == Quote`).
    pub pullquote: bool,
    /// Collapsed blockquote flag.
    pub collapsed: bool,
    /// List numbering kind (for `kind == List`).
    pub list_kind: RichListKind,
    /// Ordered list metadata.
    pub ordered_list: OrderedListData,
    /// Grouped media intent (collage vs slideshow).
    pub media_intent: GroupedMediaIntent,
    /// Button row alignment.
    pub button_alignment: RichButtonAlignment,
    /// Map latitude in degrees.
    pub latitude: f64,
    /// Map longitude in degrees.
    pub longitude: f64,
    /// Geo access hash.
    pub access_hash: u64,
    /// Nested child blocks (quotes, details, embed posts).
    pub blocks: Vec<RichBlock>,
    /// List items (when `kind == List`).
    pub list_items: Vec<RichListItem>,
    /// Grouped media items (when `kind == GroupedMedia`).
    pub media_items: Vec<GroupedMediaItem>,
    /// Table rows (when `kind == Table`).
    pub table_rows: Vec<RichTableRow>,
    /// Related articles (when `kind == RelatedArticles`).
    pub related_articles: Vec<RichRelatedArticle>,
    /// Row buttons (when `kind == ButtonRow`).
    pub buttons: Vec<RichButton>,
}

impl RichBlock {
    /// Creates a blank block of the given [`RichBlockKind`].
    #[must_use]
    pub fn new(kind: RichBlockKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    /// Creates a paragraph block with plain text.
    #[must_use]
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self {
            kind: RichBlockKind::Paragraph,
            text: RichTextNode::plain(text),
            ..Self::default()
        }
    }

    /// Creates a heading block clamped to `1..=6`.
    #[must_use]
    pub fn heading(level: u8, text: impl Into<String>) -> Self {
        Self {
            kind: RichBlockKind::Heading,
            heading_level: level.clamp(1, 6),
            text: RichTextNode::plain(text),
            ..Self::default()
        }
    }

    /// Creates a display math block with TeX source.
    #[must_use]
    pub fn math(formula: impl Into<String>) -> Self {
        Self {
            kind: RichBlockKind::Math,
            formula: formula.into(),
            ..Self::default()
        }
    }

    /// Creates a code block with optional language tag.
    #[must_use]
    pub fn code(language: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            kind: RichBlockKind::Code,
            language: language.into(),
            text: RichTextNode::plain(source),
            ..Self::default()
        }
    }
}

/// Top-level rich document (`Iv::RichPage`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RichPage {
    /// Canonical page URL (empty for rich messages).
    pub url: String,
    /// True when the page is right-to-left.
    pub rtl: bool,
    /// True if only a partial preview was returned.
    pub part: bool,
    /// View counter (for Instant View pages).
    pub views: i32,
    /// Top-level blocks.
    pub blocks: Vec<RichBlock>,
}

/// Server-enforced limits for authoring rich messages (`Iv::RichMessageLimits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichMessageLimits {
    /// Maximum total text length across all blocks (UTF-16 code units). Default `32768`.
    pub length_limit: usize,
    /// Maximum total block + list-item + table-row count. Default `500`.
    pub max_blocks: usize,
    /// Maximum block nesting depth. Default `16`.
    pub max_depth: usize,
    /// Maximum total media items. Default `50`.
    pub max_media: usize,
    /// Maximum effective columns in any table. Default `20`.
    pub max_table_cols: usize,
    /// Maximum buttons in a single button row. Default `8`.
    pub max_buttons: usize,
}

impl Default for RichMessageLimits {
    fn default() -> Self {
        Self {
            length_limit: 32_768,
            max_blocks: 500,
            max_depth: 16,
            max_media: 50,
            max_table_cols: 20,
            max_buttons: 8,
        }
    }
}

/// Which limit was exceeded by [`validate_rich_message`] (`Iv::RichMessageLimitError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RichMessageLimitError {
    /// Total text length exceeded `length_limit`.
    Length,
    /// Total block count exceeded `max_blocks`.
    Blocks,
    /// Nesting depth exceeded `max_depth`.
    Depth,
    /// Media item count exceeded `max_media`.
    Media,
    /// Table column count exceeded `max_table_cols`.
    TableColumns,
    /// Button count in a row exceeded `max_buttons`.
    Buttons,
}

/// Decoded Instant View wrapped link (`Iv::RichPageLinkUrl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichPageLinkUrl {
    /// Target URL.
    pub url: String,
    /// Instant View webpage ID.
    pub webpage_id: u64,
}

/// Parse mode for TL `RichMessage` (`Iv::RichParseMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RichParseMode {
    /// `textDiff` resolves to the updated text only.
    #[default]
    Normal,
    /// `textDiff` renders old text (strikethrough + deleted color) followed by new text (underline + inserted color).
    DisplayTextDiff,
}

// ============================================================================
// RichPage utility functions (`iv/iv_rich_page.cpp`)
// ============================================================================

/// Returns true if Roman numeral formatting is supported for `value` (`1..=9999`),
/// matching `Iv::OrderedRomanSupported` (`iv_rich_page.cpp:3309`).
#[must_use]
pub const fn ordered_roman_supported(value: i32) -> bool {
    value > 0 && value <= 9999
}

/// Returns true if a quote block can be collapsed (`Iv::RichBlockquoteIsCollapsible`).
#[must_use]
pub fn rich_blockquote_is_collapsible(block: &RichBlock) -> bool {
    block.kind == RichBlockKind::Quote && !block.pullquote && block.blocks.is_empty()
}

/// Splits a collage [`RichBlock`] with more than [`COLLAGE_MAX_ITEMS`] (10) items
/// into consecutive chunks of at most 10 items, attaching caption and anchor to the last chunk
/// (`Iv::SplitGroupedMediaBlock`).
#[must_use]
pub fn split_grouped_media_block(mut block: RichBlock) -> Vec<RichBlock> {
    if block.kind != RichBlockKind::GroupedMedia
        || block.media_intent != GroupedMediaIntent::Collage
        || block.media_items.len() <= COLLAGE_MAX_ITEMS
    {
        return vec![block];
    }
    let items = std::mem::take(&mut block.media_items);
    let caption = std::mem::take(&mut block.caption);
    let anchor_id = std::mem::take(&mut block.anchor_id);
    let mut result = Vec::with_capacity(items.len().div_ceil(COLLAGE_MAX_ITEMS));
    let mut iter = items.into_iter();
    loop {
        let chunk: Vec<GroupedMediaItem> = iter.by_ref().take(COLLAGE_MAX_ITEMS).collect();
        if chunk.is_empty() {
            break;
        }
        let mut slice = RichBlock::new(RichBlockKind::GroupedMedia);
        slice.media_intent = GroupedMediaIntent::Collage;
        slice.media_items = chunk;
        result.push(slice);
    }
    if let Some(last) = result.last_mut() {
        last.caption = caption;
        last.anchor_id = anchor_id;
    }
    result
}

/// Encodes an Instant View link with its `webpage_id` (`Iv::EncodeRichPageLinkUrl`).
#[must_use]
pub fn encode_rich_page_link_url(url: &str, webpage_id: u64) -> String {
    format!(
        "internal:wrapped?url={}&context=iv&webpage_id={webpage_id}",
        percent_encode_bytes(url.as_bytes())
    )
}

/// Decodes a link produced by [`encode_rich_page_link_url`] (`Iv::DecodeRichPageLinkUrl`).
#[must_use]
pub fn decode_rich_page_link_url(data: &str) -> Option<RichPageLinkUrl> {
    let query = data.strip_prefix("internal:wrapped?")?;
    let mut context_iv = false;
    let mut url: Option<String> = None;
    let mut webpage_id = 0u64;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        let decoded_v = percent_decode_utf8(v);
        match k {
            "context" => context_iv = decoded_v == "iv",
            "url" => {
                if !decoded_v.is_empty() {
                    url = Some(decoded_v);
                }
            }
            "webpage_id" if !decoded_v.is_empty() => {
                webpage_id = decoded_v.parse::<u64>().ok()?;
            }
            _ => {}
        }
    }
    if !context_iv {
        return None;
    }
    Some(RichPageLinkUrl {
        url: url?,
        webpage_id,
    })
}

#[derive(Default)]
struct RichMessageMetrics {
    text_length: usize,
    block_count: usize,
    max_depth: usize,
    media_count: usize,
    max_table_columns: usize,
    max_row_buttons: usize,
    table_col_measurement_limit: usize,
}

const fn is_media_kind(kind: RichBlockKind) -> bool {
    matches!(
        kind,
        RichBlockKind::Photo | RichBlockKind::Video | RichBlockKind::Audio | RichBlockKind::File
    )
}

fn effective_table_columns(rows: &[RichTableRow], max_columns: usize) -> usize {
    if rows.is_empty() || max_columns == 0 {
        return 0;
    }
    let row_count = rows.len();
    let mut occupancy: Vec<Vec<bool>> = vec![Vec::new(); row_count];
    for (row_idx, row) in rows.iter().enumerate() {
        for cell in &row.cells {
            let norm_colspan = cell.colspan.max(1) as usize;
            let rowspan = (cell.rowspan.max(1) as usize).min(row_count - row_idx);
            if rowspan == 0 {
                continue;
            }
            let mut chosen_col = None;
            for col in 0..max_columns {
                let eff_colspan = norm_colspan.min(max_columns - col);
                if eff_colspan == 0 {
                    continue;
                }
                let mut free = true;
                'check: for occ_row in occupancy.iter().skip(row_idx).take(rowspan) {
                    let lim = (col + eff_colspan).min(occ_row.len());
                    for &used in occ_row.iter().take(lim).skip(col) {
                        if used {
                            free = false;
                            break 'check;
                        }
                    }
                }
                if free {
                    chosen_col = Some(col);
                    break;
                }
            }
            let Some(col) = chosen_col else {
                return max_columns;
            };
            let eff_colspan = norm_colspan.min(max_columns - col);
            if eff_colspan != norm_colspan {
                return max_columns;
            }
            for occ_row in occupancy.iter_mut().skip(row_idx).take(rowspan) {
                if col + eff_colspan > occ_row.len() {
                    occ_row.resize(col + eff_colspan, false);
                }
                for slot in occ_row.iter_mut().skip(col).take(eff_colspan) {
                    *slot = true;
                }
            }
        }
    }
    occupancy.iter().map(Vec::len).max().unwrap_or(0)
}

fn accumulate_block_metrics(metrics: &mut RichMessageMetrics, block: &RichBlock, depth: usize) {
    metrics.block_count += 1;
    metrics.max_depth = metrics.max_depth.max(depth);
    metrics.text_length += block.text.text.utf16_len();
    metrics.text_length += block.caption.text.utf16_len();
    metrics.text_length += block.formula.encode_utf16().count();
    for btn in &block.buttons {
        metrics.text_length += btn.text.text.utf16_len();
    }
    metrics.max_row_buttons = metrics.max_row_buttons.max(block.buttons.len());
    if is_media_kind(block.kind) {
        metrics.media_count += 1;
    }
    for child in &block.blocks {
        accumulate_block_metrics(metrics, child, depth + 1);
    }
    for item in &block.list_items {
        metrics.block_count += 1;
        metrics.text_length += item.text.text.utf16_len();
        for child in &item.blocks {
            accumulate_block_metrics(metrics, child, depth + 1);
        }
    }
    for item in &block.media_items {
        if is_media_kind(item.kind) {
            metrics.media_count += 1;
        }
    }
    for row in &block.table_rows {
        metrics.block_count += 1;
        for cell in &row.cells {
            metrics.text_length += cell.text.text.utf16_len();
        }
    }
    metrics.max_table_columns = metrics.max_table_columns.max(effective_table_columns(
        &block.table_rows,
        metrics.table_col_measurement_limit,
    ));
}

/// Validates a [`RichPage`] against `limits` (`Iv::ValidateRichMessage`).
#[must_use]
pub fn validate_rich_message(
    page: &RichPage,
    limits: &RichMessageLimits,
) -> Option<RichMessageLimitError> {
    let mut metrics = RichMessageMetrics {
        table_col_measurement_limit: limits.max_table_cols.saturating_add(1).max(1),
        ..RichMessageMetrics::default()
    };
    for block in &page.blocks {
        accumulate_block_metrics(&mut metrics, block, 1);
    }
    if metrics.text_length > limits.length_limit {
        Some(RichMessageLimitError::Length)
    } else if metrics.block_count > limits.max_blocks {
        Some(RichMessageLimitError::Blocks)
    } else if metrics.max_depth > limits.max_depth {
        Some(RichMessageLimitError::Depth)
    } else if metrics.media_count > limits.max_media {
        Some(RichMessageLimitError::Media)
    } else if metrics.max_table_columns > limits.max_table_cols {
        Some(RichMessageLimitError::TableColumns)
    } else if metrics.max_row_buttons > limits.max_buttons {
        Some(RichMessageLimitError::Buttons)
    } else {
        None
    }
}

/// Counts total blocks (including nested blocks, list items, and table rows) in `page`
/// (`Iv::CountRichPageBlocks`).
#[must_use]
pub fn count_rich_page_blocks(page: &RichPage) -> usize {
    let mut metrics = RichMessageMetrics {
        table_col_measurement_limit: 1,
        ..RichMessageMetrics::default()
    };
    for block in &page.blocks {
        accumulate_block_metrics(&mut metrics, block, 1);
    }
    metrics.block_count
}

fn is_strong_rtl_char(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        0x0590..=0x05FF
            | 0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x07C0..=0x07FF
            | 0x08A0..=0x08FF
            | 0xFB1D..=0xFB4F
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x200F
    )
}

fn is_strong_ltr_char(ch: char) -> bool {
    let c = ch as u32;
    ch.is_ascii_alphabetic()
        || matches!(
            c,
            0x00C0..=0x02AF
                | 0x0370..=0x052F
                | 0x1E00..=0x1FFF
                | 0x200E
                | 0x3040..=0x30FF
                | 0x4E00..=0x9FFF
                | 0xAC00..=0xD7AF
        )
}

fn text_direction_rtl(s: &str) -> Option<bool> {
    if s.trim().is_empty() {
        return None;
    }
    for ch in s.chars() {
        if is_strong_rtl_char(ch) {
            return Some(true);
        }
        if is_strong_ltr_char(ch) {
            return Some(false);
        }
    }
    Some(false)
}

fn blocks_text_rtl(blocks: &[RichBlock]) -> Option<bool> {
    for block in blocks {
        if let Some(rtl) = text_direction_rtl(&block.text.text.text) {
            return Some(rtl);
        }
        if let Some(rtl) = blocks_text_rtl(&block.blocks) {
            return Some(rtl);
        }
        for item in &block.list_items {
            if let Some(rtl) = text_direction_rtl(&item.text.text.text) {
                return Some(rtl);
            }
            if let Some(rtl) = blocks_text_rtl(&item.blocks) {
                return Some(rtl);
            }
        }
    }
    None
}

/// Determines whether the page's primary text direction is RTL (`Iv::DetermineRichPageRtl`).
#[must_use]
pub fn determine_rich_page_rtl(page: &RichPage) -> bool {
    blocks_text_rtl(&page.blocks).unwrap_or(false)
}

fn rich_block_is_flatten_safe(block: &RichBlock) -> bool {
    if matches!(
        block.kind,
        RichBlockKind::Table
            | RichBlockKind::Math
            | RichBlockKind::Photo
            | RichBlockKind::Video
            | RichBlockKind::Audio
            | RichBlockKind::File
            | RichBlockKind::GroupedMedia
            | RichBlockKind::Map
            | RichBlockKind::ButtonRow
    ) {
        return false;
    }
    if block.media_items.iter().any(|item| {
        matches!(
            item.kind,
            RichBlockKind::Photo | RichBlockKind::Video | RichBlockKind::Audio
        )
    }) {
        return false;
    }
    if !block.blocks.iter().all(rich_block_is_flatten_safe) {
        return false;
    }
    block
        .list_items
        .iter()
        .all(|item| item.blocks.iter().all(rich_block_is_flatten_safe))
}

/// Returns true if `page` can be flattened to a plain text message without losing rich
/// media/table/math/button blocks (`Iv::RichPageIsFlattenSafe`).
#[must_use]
pub fn rich_page_is_flatten_safe(page: &RichPage) -> bool {
    page.blocks.iter().all(rich_block_is_flatten_safe)
}

/// Splits a simple [`TextWithEntities`] containing `Pre` / `Blockquote` entities into a
/// multi-block [`RichPage`] (`Iv::SplitTextIntoRichPage`).
#[must_use]
pub fn split_text_into_rich_page(text: TextWithEntities) -> RichPage {
    let is_block_entity =
        |e: &EntityInText| matches!(e.kind, EntityType::Pre | EntityType::Blockquote);
    let mut segments: Vec<EntityInText> = text
        .entities
        .iter()
        .filter(|e| is_block_entity(e) && e.length > 0)
        .cloned()
        .collect();
    segments.sort_by_key(|e| e.offset);

    let mut page = RichPage::default();
    let emit_paragraph = |page: &mut RichPage, from: usize, to: usize| {
        if to <= from {
            return;
        }
        let mut part = slice_text_with_entities(&text, from, to);
        part.entities.retain(|e| !is_block_entity(e));
        part.trim_in_place();
        if !part.is_empty() {
            page.blocks.push(RichBlock {
                kind: RichBlockKind::Paragraph,
                text: RichTextNode::from_text(part),
                ..RichBlock::default()
            });
        }
    };

    let mut cursor = 0usize;
    let total = text.utf16_len();
    for seg in segments {
        if seg.offset < cursor {
            continue;
        }
        emit_paragraph(&mut page, cursor, seg.offset);
        let end = seg.end().min(total);
        let mut body = slice_text_with_entities(&text, seg.offset, end);
        body.trim_in_place();
        body.entities.retain(|e| !is_block_entity(e));
        cursor = end;
        if body.is_empty() {
            continue;
        }
        if seg.kind == EntityType::Pre {
            page.blocks.push(RichBlock {
                kind: RichBlockKind::Code,
                text: RichTextNode::from_text(body),
                language: seg.data,
                ..RichBlock::default()
            });
        } else {
            page.blocks.push(RichBlock {
                kind: RichBlockKind::Quote,
                text: RichTextNode::from_text(body),
                collapsed: !seg.data.is_empty(),
                ..RichBlock::default()
            });
        }
    }
    emit_paragraph(&mut page, cursor, total);
    page
}

fn append_summary_line(out: &mut TextWithEntities, mut line: TextWithEntities) {
    expand_inline_text_objects(&mut line);
    line.trim_in_place();
    if line.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str("\n");
    }
    out.append(line);
}

fn append_summary_block(out: &mut TextWithEntities, block: &RichBlock) {
    match block.kind {
        RichBlockKind::Math => {
            let trimmed = trim_math_source(&block.formula);
            if !trimmed.is_empty() {
                append_summary_line(out, TextWithEntities::simple(trimmed));
            }
        }
        RichBlockKind::Divider | RichBlockKind::Anchor | RichBlockKind::Unsupported => {}
        _ => {
            append_summary_line(out, block.text.text.clone());
            for child in &block.blocks {
                append_summary_block(out, child);
            }
            for item in &block.list_items {
                append_summary_line(out, item.text.text.clone());
                for child in &item.blocks {
                    append_summary_block(out, child);
                }
            }
            for row in &block.table_rows {
                let mut row_text = TextWithEntities::default();
                for cell in &row.cells {
                    let mut c = cell.text.text.clone();
                    expand_inline_text_objects(&mut c);
                    c.trim_in_place();
                    if !c.is_empty() {
                        if !row_text.is_empty() {
                            row_text.push_str(" | ");
                        }
                        row_text.append(c);
                    }
                }
                append_summary_line(out, row_text);
            }
            append_summary_line(out, block.caption.text.clone());
        }
    }
}

/// Flattens a [`RichPage`] into a concise summary for dialog previews (`Iv::FlattenRichPageSummary`).
#[must_use]
pub fn flatten_rich_page_summary(page: &RichPage, empty_fallback: bool) -> TextWithEntities {
    let mut out = TextWithEntities::default();
    for block in &page.blocks {
        append_summary_block(&mut out, block);
    }
    out.trim_in_place();
    if out.is_empty() && empty_fallback {
        out = TextWithEntities::simple("Empty message");
    }
    out
}

/// Flattens a [`RichPage`] into a simple message with `Pre` and `Blockquote` entities
/// (`Iv::FlattenRichPageToSimpleText`).
#[must_use]
pub fn flatten_rich_page_to_simple_text(page: &RichPage) -> TextWithEntities {
    let mut out = TextWithEntities::default();
    for block in &page.blocks {
        match block.kind {
            RichBlockKind::Code => {
                let mut inner = block.text.text.clone();
                expand_inline_text_objects(&mut inner);
                inner.entities.clear();
                inner.trim_in_place();
                if !inner.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n");
                    }
                    let start = out.utf16_len();
                    let len = inner.utf16_len();
                    out.push_str(&inner.text);
                    out.entities.push(EntityInText::with_data(
                        EntityType::Pre,
                        start,
                        len,
                        block.language.clone(),
                    ));
                }
            }
            RichBlockKind::Quote => {
                let mut inner = TextWithEntities::default();
                append_summary_line(&mut inner, block.text.text.clone());
                for child in &block.blocks {
                    append_summary_block(&mut inner, child);
                }
                append_summary_line(&mut inner, block.caption.text.clone());
                inner.entities.retain(|e| {
                    !matches!(
                        e.kind,
                        EntityType::Subscript | EntityType::Superscript | EntityType::Marked
                    )
                });
                if !inner.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n");
                    }
                    let start = out.utf16_len();
                    let len = inner.utf16_len();
                    out.append(inner);
                    out.entities
                        .push(EntityInText::new(EntityType::Blockquote, start, len));
                }
            }
            _ => {
                let mut part = TextWithEntities::default();
                append_summary_block(&mut part, block);
                part.entities.retain(|e| {
                    !matches!(
                        e.kind,
                        EntityType::Subscript | EntityType::Superscript | EntityType::Marked
                    )
                });
                if !part.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n");
                    }
                    out.append(part);
                }
            }
        }
    }
    out.trim_in_place();
    out
}

// ============================================================================
// TL Deserialization: `tg_tl` -> `RichPage` (`iv/iv_rich_page.cpp`)
// ============================================================================

#[derive(Default)]
struct TlParseContext {
    is_rich_message: bool,
    display_text_diff: bool,
    block_depth: usize,
    photo_sizes: BTreeMap<u64, (i32, i32)>,
    document_infos: BTreeMap<u64, TlDocumentInfo>,
}

#[derive(Default, Clone)]
struct TlDocumentInfo {
    width: i32,
    height: i32,
    file_name: String,
    title: String,
    performer: String,
    duration: i32,
}

fn remember_tl_photo(ctx: &mut TlParseContext, photo: &tl_enums::Photo) {
    if let tl_enums::Photo::Photo(p) = photo {
        let mut best_rank = usize::MAX;
        let mut best_size = (0, 0);
        const LEVELS: &str = "ydxcwmbsa";
        for sz in &p.sizes {
            let (ty, w, h) = match sz {
                tl_enums::PhotoSize::PhotoSize(s) => (s.r#type.as_str(), s.w, s.h),
                tl_enums::PhotoSize::PhotoCachedSize(s) => (s.r#type.as_str(), s.w, s.h),
                tl_enums::PhotoSize::Progressive(s) => (s.r#type.as_str(), s.w, s.h),
                _ => continue,
            };
            if let Some(first) = ty.chars().next()
                && let Some(rank) = LEVELS.find(first)
                && rank < best_rank
            {
                best_rank = rank;
                best_size = (w, h);
            }
        }
        ctx.photo_sizes.insert(p.id as u64, best_size);
    }
}

fn remember_tl_document(ctx: &mut TlParseContext, doc: &tl_enums::Document) {
    if let tl_enums::Document::Document(d) = doc {
        let mut info = TlDocumentInfo::default();
        for attr in &d.attributes {
            match attr {
                tl_enums::DocumentAttribute::Audio(a) => {
                    info.duration = a.duration;
                    info.title = a.title.clone().unwrap_or_default();
                    info.performer = a.performer.clone().unwrap_or_default();
                }
                tl_enums::DocumentAttribute::Filename(f) => {
                    info.file_name = f.file_name.clone();
                }
                tl_enums::DocumentAttribute::ImageSize(s) => {
                    if info.width <= 0 || info.height <= 0 {
                        info.width = s.w;
                        info.height = s.h;
                    }
                }
                tl_enums::DocumentAttribute::Video(v) => {
                    info.width = v.w;
                    info.height = v.h;
                }
                _ => {}
            }
        }
        ctx.document_infos.insert(d.id as u64, info);
    }
}

fn serialize_date_flags_data(date: i32, flags: DateFlags) -> String {
    format!("{date}:{}", flags.format_code())
}

fn deserialize_date_flags_data(data: &str) -> (i32, DateFlags) {
    let Some((d_str, code)) = data.split_once(':') else {
        return (data.parse::<i32>().unwrap_or(0), DateFlags::default());
    };
    let date = d_str.parse::<i32>().unwrap_or(0);
    let mut flags = DateFlags::default();
    for ch in code.chars() {
        match ch {
            'r' => flags.relative = true,
            't' => flags.short_time = true,
            'T' => flags.long_time = true,
            'd' => flags.short_date = true,
            'D' => flags.long_date = true,
            'w' => flags.day_of_week = true,
            _ => {}
        }
    }
    (date, flags)
}

fn parse_tl_button_style(style: Option<&tl_enums::RichButtonStyle>) -> (RichButtonColor, bool) {
    let Some(tl_enums::RichButtonStyle::RichButtonStyle(s)) = style else {
        return (RichButtonColor::Normal, false);
    };
    let color = if s.bg_primary {
        RichButtonColor::Primary
    } else if s.bg_danger {
        RichButtonColor::Danger
    } else if s.bg_success {
        RichButtonColor::Success
    } else {
        RichButtonColor::Normal
    };
    (color, s.link)
}

fn parse_tl_inline_button_type(ty: &tl_enums::InlineButtonType) -> (RichButtonType, Vec<u8>, i64) {
    match ty {
        tl_enums::InlineButtonType::Url(u) => (RichButtonType::Url, u.url.as_bytes().to_vec(), 0),
        tl_enums::InlineButtonType::Copy(c) => {
            (RichButtonType::CopyText, c.copy_text.as_bytes().to_vec(), 0)
        }
        tl_enums::InlineButtonType::UserProfile(u) => (
            RichButtonType::UserProfile,
            u.user_id.to_string().into_bytes(),
            0,
        ),
        tl_enums::InlineButtonType::Callback(c) => (
            if c.requires_password {
                RichButtonType::CallbackWithPassword
            } else {
                RichButtonType::Callback
            },
            c.data.clone(),
            0,
        ),
        tl_enums::InlineButtonType::WebView(w) => {
            (RichButtonType::WebView, w.url.as_bytes().to_vec(), 0)
        }
        tl_enums::InlineButtonType::UrlAuth(a) => (
            RichButtonType::UrlAuth,
            a.url.as_bytes().to_vec(),
            i64::from(a.button_id),
        ),
        tl_enums::InlineButtonType::Game => (RichButtonType::Game, Vec::new(), 0),
        tl_enums::InlineButtonType::Buy => (RichButtonType::Buy, Vec::new(), 0),
        tl_enums::InlineButtonType::SwitchInline(s) => {
            (RichButtonType::SwitchInline, s.query.as_bytes().to_vec(), 0)
        }
        tl_enums::InlineButtonType::Disabled
        | tl_enums::InlineButtonType::InputInlineButtonTypeUrlAuth(_)
        | tl_enums::InlineButtonType::InputInlineButtonTypeUserProfile(_) => {
            (RichButtonType::Disabled, Vec::new(), 0)
        }
    }
}

fn append_tl_rich_text(
    tl: &tl_enums::RichText,
    out: &mut RichTextNode,
    ctx: &TlParseContext,
    depth: usize,
) {
    if depth >= MAX_PARSED_BLOCK_DEPTH {
        return;
    }
    let next = depth + 1;
    let add_entity = |out: &mut RichTextNode, from: usize, kind: EntityType, data: String| {
        let len = out.text.utf16_len().saturating_sub(from);
        if len > 0 {
            out.text
                .entities
                .push(EntityInText::with_data(kind, from, len, data));
        }
    };
    match tl {
        tl_enums::RichText::TextEmpty => {}
        tl_enums::RichText::TextPlain(t) => out.text.push_str(&t.text),
        tl_enums::RichText::TextConcat(c) => {
            for part in &c.texts {
                append_tl_rich_text(part, out, ctx, next);
            }
        }
        tl_enums::RichText::TextBold(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Bold, String::new());
        }
        tl_enums::RichText::TextItalic(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Italic, String::new());
        }
        tl_enums::RichText::TextUnderline(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Underline, String::new());
        }
        tl_enums::RichText::TextStrike(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::StrikeOut, String::new());
        }
        tl_enums::RichText::TextFixed(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Code, String::new());
        }
        tl_enums::RichText::TextSubscript(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Subscript, String::new());
        }
        tl_enums::RichText::TextSuperscript(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Superscript, String::new());
        }
        tl_enums::RichText::TextMarked(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Marked, String::new());
        }
        tl_enums::RichText::TextSpoiler(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Spoiler, String::new());
        }
        tl_enums::RichText::TextUrl(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            if out.text.utf16_len() == from {
                out.text.push_str(&t.url);
            }
            let data = if !ctx.is_rich_message && t.webpage_id != 0 && !t.url.is_empty() {
                encode_rich_page_link_url(&t.url, t.webpage_id as u64)
            } else {
                t.url.clone()
            };
            add_entity(out, from, EntityType::CustomUrl, data);
        }
        tl_enums::RichText::TextEmail(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            if out.text.utf16_len() == from {
                out.text.push_str(&t.email);
            }
            add_entity(
                out,
                from,
                EntityType::CustomUrl,
                format!("mailto:{}", t.email),
            );
        }
        tl_enums::RichText::TextPhone(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            if out.text.utf16_len() == from {
                out.text.push_str(&t.phone);
            }
            add_entity(out, from, EntityType::CustomUrl, format!("tel:{}", t.phone));
        }
        tl_enums::RichText::TextAnchor(t) => {
            out.add_anchor(normalize_fragment_id(&t.name));
            append_tl_rich_text(&t.text, out, ctx, next);
        }
        tl_enums::RichText::TextMath(t) => {
            let source = trim_math_source(&t.source).to_string();
            let from = out.text.utf16_len();
            let payload = serialize_inline_text_object_entity(&InlineTextObjectEntity::Formula(
                InlineTextObjectFormulaData {
                    copy_source: inline_formula_copy_source(&source),
                    trimmed_tex: source,
                },
            ));
            out.text.text.push(OBJECT_REPLACEMENT_CHAR);
            add_entity(out, from, EntityType::CustomEmoji, payload);
        }
        tl_enums::RichText::TextImage(t) => {
            if ctx.is_rich_message || t.document_id == 0 || t.w <= 0 || t.h <= 0 {
                out.text.push_str("[image]");
            } else {
                let from = out.text.utf16_len();
                let payload = serialize_inline_text_object_entity(
                    &InlineTextObjectEntity::IvImage(InlineTextObjectIvImageData {
                        document_id: t.document_id as u64,
                        width: t.w,
                        height: t.h,
                        replacement_text: "[image]".to_string(),
                    }),
                );
                out.text.text.push(OBJECT_REPLACEMENT_CHAR);
                add_entity(out, from, EntityType::CustomEmoji, payload);
            }
        }
        tl_enums::RichText::TextCustomEmoji(t) => {
            let from = out.text.utf16_len();
            let alt = if t.alt.is_empty() { "@" } else { &t.alt };
            out.text.push_str(alt);
            add_entity(
                out,
                from,
                EntityType::CustomEmoji,
                t.document_id.to_string(),
            );
        }
        tl_enums::RichText::TextMention(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Mention, String::new());
        }
        tl_enums::RichText::TextHashtag(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Hashtag, String::new());
        }
        tl_enums::RichText::TextBotCommand(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::BotCommand, String::new());
        }
        tl_enums::RichText::TextCashtag(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Cashtag, String::new());
        }
        tl_enums::RichText::TextAutoUrl(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Url, String::new());
        }
        tl_enums::RichText::TextAutoEmail(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Email, String::new());
        }
        tl_enums::RichText::TextAutoPhone(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::Phone, String::new());
        }
        tl_enums::RichText::TextBankCard(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::BankCard, String::new());
        }
        tl_enums::RichText::TextTonAddress(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::TonAddress, String::new());
        }
        tl_enums::RichText::TextMentionName(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            add_entity(out, from, EntityType::MentionName, t.user_id.to_string());
        }
        tl_enums::RichText::TextDate(t) => {
            let from = out.text.utf16_len();
            append_tl_rich_text(&t.text, out, ctx, next);
            let flags = DateFlags {
                relative: t.relative,
                short_time: t.short_time,
                long_time: t.long_time,
                short_date: t.short_date,
                long_date: t.long_date,
                day_of_week: t.day_of_week,
            };
            add_entity(
                out,
                from,
                EntityType::FormattedDate,
                serialize_date_flags_data(t.date, flags),
            );
        }
        tl_enums::RichText::TextDiff(t) => {
            if !ctx.display_text_diff {
                append_tl_rich_text(&t.text, out, ctx, next);
            } else {
                let del_from = out.text.utf16_len();
                append_tl_rich_text(&t.old_text, out, ctx, next);
                add_entity(out, del_from, EntityType::StrikeOut, String::new());
                add_entity(
                    out,
                    del_from,
                    EntityType::Colorized,
                    TEXT_DIFF_DELETED_COLOR_INDEX.to_string(),
                );
                let ins_from = out.text.utf16_len();
                append_tl_rich_text(&t.text, out, ctx, next);
                add_entity(out, ins_from, EntityType::Underline, String::new());
                add_entity(
                    out,
                    ins_from,
                    EntityType::Colorized,
                    TEXT_DIFF_INSERTED_COLOR_INDEX.to_string(),
                );
            }
        }
        tl_enums::RichText::TextButton(t) => {
            let mut label_node = RichTextNode::default();
            append_tl_rich_text(&t.text, &mut label_node, ctx, next);
            label_node.text.trim_in_place();
            if label_node.text.is_empty() {
                return;
            }
            let (button_type, data, button_id) = parse_tl_inline_button_type(&t.r#type);
            let (color, link) = parse_tl_button_style(t.style.as_ref());
            let from = out.text.utf16_len();
            let payload = serialize_inline_text_object_entity(&InlineTextObjectEntity::Button(
                InlineTextObjectButtonData {
                    label: label_node.text,
                    data,
                    button_id,
                    button_type,
                    color,
                    peer_types: 0,
                    link,
                },
            ));
            out.text.text.push(OBJECT_REPLACEMENT_CHAR);
            add_entity(out, from, EntityType::CustomEmoji, payload);
        }
    }
}

fn parse_tl_rich_text(tl: &tl_enums::RichText, ctx: &TlParseContext) -> RichTextNode {
    let mut node = RichTextNode::default();
    append_tl_rich_text(tl, &mut node, ctx, 0);
    node.text.sort_entities();
    node
}

fn parse_tl_caption(caption: &tl_enums::PageCaption, ctx: &TlParseContext) -> RichTextNode {
    let tl_enums::PageCaption::PageCaption(c) = caption;
    let mut out = parse_tl_rich_text(&c.text, ctx);
    let credit = parse_tl_rich_text(&c.credit, ctx);
    if !credit.text.is_empty() {
        if !out.text.is_empty() {
            out.text.push_str("\n");
        }
        out.text.append(credit.text);
        out.add_anchor(credit.anchor_id);
        for a in credit.anchor_ids {
            out.add_anchor(a);
        }
    }
    out
}

fn adopt_anchor(block_anchor: &mut String, text: &mut RichTextNode) {
    if block_anchor.is_empty() && !text.anchor_id.is_empty() {
        *block_anchor = std::mem::take(&mut text.anchor_id);
    }
}

fn adopt_leading_paragraph_list_item(item: &mut RichListItem) {
    if item.blocks.len() == 1 && item.blocks[0].kind == RichBlockKind::Paragraph {
        let first = item.blocks.remove(0);
        item.text = first.text;
        item.anchor_id = first.anchor_id;
    }
}

fn append_tl_page_block(
    tl: &tl_enums::PageBlock,
    out: &mut Vec<RichBlock>,
    ctx: &mut TlParseContext,
) {
    if ctx.block_depth >= MAX_PARSED_BLOCK_DEPTH {
        return;
    }
    ctx.block_depth += 1;
    match tl {
        tl_enums::PageBlock::Unsupported => {
            out.push(RichBlock::new(RichBlockKind::Unsupported));
        }
        tl_enums::PageBlock::Title(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 1;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Subtitle(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 2;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Header(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 2;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Subheader(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 3;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Kicker(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 6;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading1(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 1;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading2(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 2;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading3(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 3;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading4(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 4;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading5(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 5;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Heading6(b) => {
            let mut block = RichBlock::new(RichBlockKind::Heading);
            block.heading_level = 6;
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::AuthorDate(b) => {
            let mut block = RichBlock::new(RichBlockKind::AuthorDate);
            block.text = parse_tl_rich_text(&b.author, ctx);
            block.date = b.published_date;
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Paragraph(b) => {
            let mut block = RichBlock::new(RichBlockKind::Paragraph);
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Preformatted(b) => {
            let mut block = RichBlock::new(RichBlockKind::Code);
            block.text = parse_tl_rich_text(&b.text, ctx);
            block.language = b.language.clone();
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Footer(b) => {
            let mut block = RichBlock::new(RichBlockKind::Footer);
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Divider => {
            out.push(RichBlock::new(RichBlockKind::Divider));
        }
        tl_enums::PageBlock::Anchor(b) => {
            let mut block = RichBlock::new(RichBlockKind::Anchor);
            block.anchor_id = normalize_fragment_id(&b.name);
            out.push(block);
        }
        tl_enums::PageBlock::List(b) => {
            let mut block = RichBlock::new(RichBlockKind::List);
            block.list_kind = RichListKind::Bullet;
            for item in &b.items {
                let mut parsed = RichListItem::default();
                match item {
                    tl_enums::PageListItem::Text(t) => {
                        parsed.task_state = RichTaskState::from_flags(t.checkbox, t.checked);
                        parsed.text = parse_tl_rich_text(&t.text, ctx);
                        adopt_anchor(&mut parsed.anchor_id, &mut parsed.text);
                    }
                    tl_enums::PageListItem::Blocks(bl) => {
                        parsed.task_state = RichTaskState::from_flags(bl.checkbox, bl.checked);
                        for sub in &bl.blocks {
                            append_tl_page_block(sub, &mut parsed.blocks, ctx);
                        }
                        adopt_leading_paragraph_list_item(&mut parsed);
                    }
                }
                block.list_items.push(parsed);
            }
            out.push(block);
        }
        tl_enums::PageBlock::OrderedList(b) => {
            let mut block = RichBlock::new(RichBlockKind::List);
            block.list_kind = RichListKind::Ordered;
            block.ordered_list = OrderedListData {
                reversed: b.reversed,
                start: b.start,
                list_type: b.r#type.clone(),
            };
            for item in &b.items {
                let mut parsed = RichListItem::default();
                match item {
                    tl_enums::PageListOrderedItem::Text(t) => {
                        parsed.task_state = RichTaskState::from_flags(t.checkbox, t.checked);
                        parsed.number = OrderedListItemData {
                            num: t.num.clone(),
                            value: t.value,
                            list_type: t.r#type.clone(),
                        };
                        parsed.text = parse_tl_rich_text(&t.text, ctx);
                        adopt_anchor(&mut parsed.anchor_id, &mut parsed.text);
                    }
                    tl_enums::PageListOrderedItem::Blocks(bl) => {
                        parsed.task_state = RichTaskState::from_flags(bl.checkbox, bl.checked);
                        parsed.number = OrderedListItemData {
                            num: bl.num.clone(),
                            value: bl.value,
                            list_type: bl.r#type.clone(),
                        };
                        for sub in &bl.blocks {
                            append_tl_page_block(sub, &mut parsed.blocks, ctx);
                        }
                        adopt_leading_paragraph_list_item(&mut parsed);
                    }
                }
                block.list_items.push(parsed);
            }
            out.push(block);
        }
        tl_enums::PageBlock::Blockquote(b) => {
            let mut block = RichBlock::new(RichBlockKind::Quote);
            block.text = parse_tl_rich_text(&b.text, ctx);
            block.caption = parse_tl_rich_text(&b.caption, ctx);
            block.collapsed = b.collapsed;
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::BlockquoteBlocks(b) => {
            let mut block = RichBlock::new(RichBlockKind::Quote);
            for sub in &b.blocks {
                append_tl_page_block(sub, &mut block.blocks, ctx);
            }
            block.caption = parse_tl_rich_text(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Pullquote(b) => {
            let mut block = RichBlock::new(RichBlockKind::Quote);
            block.pullquote = true;
            block.text = parse_tl_rich_text(&b.text, ctx);
            block.caption = parse_tl_rich_text(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::Photo(b) => {
            let mut block = RichBlock::new(RichBlockKind::Photo);
            block.photo_id = b.photo_id as u64;
            if let Some(&(w, h)) = ctx.photo_sizes.get(&block.photo_id) {
                block.width = w;
                block.height = h;
            }
            block.caption = parse_tl_caption(&b.caption, ctx);
            block.url = b.url.clone().unwrap_or_default();
            block.spoiler = b.spoiler;
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Video(b) => {
            let mut block = RichBlock::new(RichBlockKind::Video);
            block.document_id = b.video_id as u64;
            if let Some(info) = ctx.document_infos.get(&block.document_id) {
                block.width = info.width;
                block.height = info.height;
                block.file_name = info.file_name.clone();
            }
            block.caption = parse_tl_caption(&b.caption, ctx);
            block.autoplay = b.autoplay;
            block.loop_video = b.r#loop;
            block.spoiler = b.spoiler;
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Cover(b) => {
            append_tl_page_block(&b.cover, out, ctx);
        }
        tl_enums::PageBlock::Embed(b) => {
            let mut block = RichBlock::new(RichBlockKind::Embed);
            block.url = b.url.clone().unwrap_or_default();
            block.html = b.html.as_deref().unwrap_or_default().as_bytes().to_vec();
            block.photo_id = b.poster_photo_id.unwrap_or(0) as u64;
            block.width = b.w.unwrap_or(0);
            block.height = b.h.unwrap_or(0);
            block.full_width = b.full_width;
            block.allow_scrolling = b.allow_scrolling;
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::EmbedPost(b) => {
            let mut block = RichBlock::new(RichBlockKind::EmbedPost);
            block.url = b.url.clone();
            block.photo_id = b.author_photo_id as u64;
            block.author = b.author.clone();
            block.date = b.date;
            for sub in &b.blocks {
                append_tl_page_block(sub, &mut block.blocks, ctx);
            }
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Collage(b) => {
            let block =
                parse_tl_grouped_media(&b.items, &b.caption, GroupedMediaIntent::Collage, ctx);
            out.extend(split_grouped_media_block(block));
        }
        tl_enums::PageBlock::Slideshow(b) => {
            let block =
                parse_tl_grouped_media(&b.items, &b.caption, GroupedMediaIntent::Slideshow, ctx);
            out.push(block);
        }
        tl_enums::PageBlock::Channel(b) => {
            let mut block = RichBlock::new(RichBlockKind::Channel);
            match &b.channel {
                tl_enums::Chat::Channel(ch) => {
                    block.channel_id = ch.id as u64;
                    block.channel_title = ch.title.clone();
                    block.username = ch.username.clone().unwrap_or_default();
                }
                tl_enums::Chat::ChannelForbidden(ch) => {
                    block.channel_id = ch.id as u64;
                    block.channel_title = ch.title.clone();
                }
                tl_enums::Chat::Chat(c) => {
                    block.channel_id = c.id as u64;
                    block.channel_title = c.title.clone();
                }
                _ => {}
            }
            out.push(block);
        }
        tl_enums::PageBlock::Audio(b) => {
            let mut block = RichBlock::new(RichBlockKind::Audio);
            block.document_id = b.audio_id as u64;
            if let Some(info) = ctx.document_infos.get(&block.document_id) {
                block.audio_title = info.title.clone();
                block.audio_performer = info.performer.clone();
                block.audio_duration = info.duration;
                block.file_name = info.file_name.clone();
            }
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Document(b) => {
            let mut block = RichBlock::new(RichBlockKind::File);
            block.document_id = b.document_id as u64;
            if let Some(info) = ctx.document_infos.get(&block.document_id) {
                block.audio_title = info.title.clone();
                block.audio_performer = info.performer.clone();
                block.audio_duration = info.duration;
                block.file_name = info.file_name.clone();
            }
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Table(b) => {
            let mut block = RichBlock::new(RichBlockKind::Table);
            block.bordered = b.bordered;
            block.striped = b.striped;
            block.compact = b.compact;
            block.text = parse_tl_rich_text(&b.title, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            for row in &b.rows {
                let tl_enums::PageTableRow::PageTableRow(r) = row;
                let mut cells = Vec::with_capacity(r.cells.len());
                for cell in &r.cells {
                    let tl_enums::PageTableCell::PageTableCell(c) = cell;
                    let alignment = if c.align_right {
                        RichTableAlignment::Right
                    } else if c.align_center {
                        RichTableAlignment::Center
                    } else {
                        RichTableAlignment::Left
                    };
                    let vertical_alignment = if c.valign_bottom {
                        RichTableVerticalAlignment::Bottom
                    } else if c.valign_middle {
                        RichTableVerticalAlignment::Middle
                    } else {
                        RichTableVerticalAlignment::Top
                    };
                    cells.push(RichTableCell {
                        text: c
                            .text
                            .as_ref()
                            .map(|t| parse_tl_rich_text(t, ctx))
                            .unwrap_or_default(),
                        colspan: c.colspan.unwrap_or(1).max(1),
                        rowspan: c.rowspan.unwrap_or(1).max(1),
                        header: c.header,
                        alignment,
                        vertical_alignment,
                    });
                }
                block.table_rows.push(RichTableRow { cells });
            }
            out.push(block);
        }
        tl_enums::PageBlock::Details(b) => {
            let mut block = RichBlock::new(RichBlockKind::Details);
            block.open = b.open;
            block.text = parse_tl_rich_text(&b.title, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            for sub in &b.blocks {
                append_tl_page_block(sub, &mut block.blocks, ctx);
            }
            out.push(block);
        }
        tl_enums::PageBlock::RelatedArticles(b) => {
            let mut block = RichBlock::new(RichBlockKind::RelatedArticles);
            block.text = parse_tl_rich_text(&b.title, ctx);
            for art in &b.articles {
                let tl_enums::PageRelatedArticle::PageRelatedArticle(a) = art;
                block.related_articles.push(RichRelatedArticle {
                    url: a.url.clone(),
                    webpage_id: a.webpage_id as u64,
                    photo_id: a.photo_id.unwrap_or(0) as u64,
                    title: a.title.clone().unwrap_or_default(),
                    description: a.description.clone().unwrap_or_default(),
                    author: a.author.clone().unwrap_or_default(),
                    published_date: a.published_date.unwrap_or(0),
                });
            }
            out.push(block);
        }
        tl_enums::PageBlock::Map(b) => {
            let mut block = RichBlock::new(RichBlockKind::Map);
            if let tl_enums::GeoPoint::GeoPoint(g) = &b.geo {
                block.latitude = g.lat;
                block.longitude = g.long;
                block.access_hash = g.access_hash as u64;
            }
            block.zoom = b.zoom;
            block.width = if b.w > 0 { b.w } else { DEFAULT_MAP_WIDTH };
            block.height = if b.h > 0 { b.h } else { DEFAULT_MAP_HEIGHT };
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::InputPageBlockMap(b) => {
            let mut block = RichBlock::new(RichBlockKind::Map);
            if let tl_enums::InputGeoPoint::InputGeoPoint(g) = &b.geo {
                block.latitude = g.lat;
                block.longitude = g.long;
            }
            block.zoom = b.zoom;
            block.width = if b.w > 0 { b.w } else { DEFAULT_MAP_WIDTH };
            block.height = if b.h > 0 { b.h } else { DEFAULT_MAP_HEIGHT };
            block.caption = parse_tl_caption(&b.caption, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.caption);
            out.push(block);
        }
        tl_enums::PageBlock::Math(b) => {
            let mut block = RichBlock::new(RichBlockKind::Math);
            block.formula = trim_math_source(&b.source).to_string();
            out.push(block);
        }
        tl_enums::PageBlock::Thinking(b) => {
            let mut block = RichBlock::new(RichBlockKind::Thinking);
            block.text = parse_tl_rich_text(&b.text, ctx);
            adopt_anchor(&mut block.anchor_id, &mut block.text);
            out.push(block);
        }
        tl_enums::PageBlock::ButtonRow(b) => {
            let mut block = RichBlock::new(RichBlockKind::ButtonRow);
            block.button_alignment = if b.align_left {
                RichButtonAlignment::Left
            } else if b.align_center {
                RichButtonAlignment::Center
            } else if b.align_right {
                RichButtonAlignment::Right
            } else {
                RichButtonAlignment::Stretch
            };
            for btn in &b.buttons {
                let tl_enums::PageButton::PageButton(pb) = btn;
                let (button_type, data, button_id) = parse_tl_inline_button_type(&pb.r#type);
                let (color, _) = parse_tl_button_style(pb.style.as_ref());
                let text = parse_tl_rich_text(&pb.text, ctx);
                block.buttons.push(RichButton {
                    text,
                    button_type,
                    color,
                    data,
                    button_id,
                    peer_types: 0,
                });
            }
            out.push(block);
        }
    }
    ctx.block_depth -= 1;
}

fn parse_tl_grouped_media(
    items: &[tl_enums::PageBlock],
    caption: &tl_enums::PageCaption,
    intent: GroupedMediaIntent,
    ctx: &TlParseContext,
) -> RichBlock {
    let mut block = RichBlock::new(RichBlockKind::GroupedMedia);
    block.media_intent = intent;
    for item in items {
        match item {
            tl_enums::PageBlock::Photo(p) => {
                let photo_id = p.photo_id as u64;
                let (width, height) = ctx.photo_sizes.get(&photo_id).copied().unwrap_or((0, 0));
                block.media_items.push(GroupedMediaItem {
                    kind: RichBlockKind::Photo,
                    photo_id,
                    document_id: 0,
                    width,
                    height,
                    autoplay: false,
                    loop_video: false,
                    spoiler: p.spoiler,
                });
            }
            tl_enums::PageBlock::Video(v) => {
                let document_id = v.video_id as u64;
                let (width, height) = ctx
                    .document_infos
                    .get(&document_id)
                    .map(|i| (i.width, i.height))
                    .unwrap_or((0, 0));
                block.media_items.push(GroupedMediaItem {
                    kind: RichBlockKind::Video,
                    photo_id: 0,
                    document_id,
                    width,
                    height,
                    autoplay: v.autoplay,
                    loop_video: v.r#loop,
                    spoiler: v.spoiler,
                });
            }
            _ => {}
        }
    }
    block.caption = parse_tl_caption(caption, ctx);
    adopt_anchor(&mut block.anchor_id, &mut block.caption);
    block
}

/// Parses a slice of Layer-233 TL `PageBlock`s into a [`RichPage`].
#[must_use]
pub fn rich_page_from_tl_blocks(blocks: &[tl_enums::PageBlock], mode: RichParseMode) -> RichPage {
    let mut ctx = TlParseContext {
        is_rich_message: true,
        display_text_diff: mode == RichParseMode::DisplayTextDiff,
        ..TlParseContext::default()
    };
    let mut page = RichPage::default();
    for block in blocks {
        append_tl_page_block(block, &mut page.blocks, &mut ctx);
    }
    page
}

/// Parses a Layer-233 TL `RichMessage` into a [`RichPage`] (`Iv::ParseRichPage(MTPRichMessage)`).
#[must_use]
pub fn rich_page_from_tl_rich_message(
    msg: &tl_types::RichMessage,
    mode: RichParseMode,
) -> RichPage {
    let mut ctx = TlParseContext {
        is_rich_message: true,
        display_text_diff: mode == RichParseMode::DisplayTextDiff,
        ..TlParseContext::default()
    };
    for photo in &msg.photos {
        remember_tl_photo(&mut ctx, photo);
    }
    for doc in &msg.documents {
        remember_tl_document(&mut ctx, doc);
    }
    let mut page = RichPage {
        url: String::new(),
        rtl: msg.rtl,
        part: msg.part,
        views: 0,
        blocks: Vec::new(),
    };
    for block in &msg.blocks {
        append_tl_page_block(block, &mut page.blocks, &mut ctx);
    }
    page
}

/// Parses a Layer-233 TL Instant View `Page` into a [`RichPage`] (`Iv::ParseRichPage(MTPPage)`).
#[must_use]
pub fn rich_page_from_tl_page(tl_page: &tl_types::Page) -> RichPage {
    let mut ctx = TlParseContext {
        is_rich_message: false,
        ..TlParseContext::default()
    };
    for photo in &tl_page.photos {
        remember_tl_photo(&mut ctx, photo);
    }
    for doc in &tl_page.documents {
        remember_tl_document(&mut ctx, doc);
    }
    let mut page = RichPage {
        url: tl_page.url.clone(),
        rtl: tl_page.rtl,
        part: tl_page.part,
        views: tl_page.views.unwrap_or(0),
        blocks: Vec::new(),
    };
    for block in &tl_page.blocks {
        append_tl_page_block(block, &mut page.blocks, &mut ctx);
    }
    page
}

// ============================================================================
// TL Serialization: `RichPage` -> `tg_tl` (`iv/iv_rich_message_serializer.cpp`)
// ============================================================================

/// Serialization mode matching `Iv::SerializeInputRichMessageMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeRichMessageMode {
    /// Draft mode: skips unuploaded media instead of failing and preserves trailing empty blocks.
    Draft,
    /// Final submit mode: normalizes and trims empty blocks/edges; fails if content is empty or media missing.
    FinalSubmit,
}

/// Status of [`serialize_input_rich_message`] (`Iv::SerializeInputRichMessageStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeRichMessageStatus {
    /// Serialization succeeded and produced a non-empty `InputRichMessage`.
    Success,
    /// Page contained no meaningful content after normalization.
    EmptyContent,
    /// Serialization failed (unsupported block kind, missing media handle, or invalid map/button).
    Failed,
}

/// Output of [`serialize_input_rich_message`] (`Iv::SerializeInputRichMessageResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct SerializeRichMessageResult {
    /// Outcome status.
    pub status: SerializeRichMessageStatus,
    /// Populated when `status == SerializeRichMessageStatus::Success`.
    pub value: Option<tl_types::InputRichMessage>,
}

/// Resolves numeric photo, document, and user IDs into TL input handles during serialization.
pub trait RichMediaResolver {
    /// Resolves a photo ID to a TL `InputPhoto` (returning `None` if unuploaded / unavailable).
    fn resolve_photo(&self, photo_id: u64) -> Option<tl_enums::InputPhoto>;
    /// Resolves a document ID to a TL `InputDocument`.
    fn resolve_document(&self, document_id: u64) -> Option<tl_enums::InputDocument>;
    /// Returns true if the document is audio/voice (`pageBlockAudio` vs `pageBlockDocument`).
    fn is_audio_document(&self, document_id: u64) -> bool;
    /// Resolves a user ID to a TL `InputUser` (for mentions and user-profile buttons).
    fn resolve_user(&self, user_id: u64) -> Option<tl_enums::InputUser>;
}

/// A resolver that synthesizes `InputPhoto`/`InputDocument`/`InputUser` handles directly from
/// non-zero IDs (useful for round-trip tests and local serialization without a live session).
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectIdMediaResolver;

impl RichMediaResolver for DirectIdMediaResolver {
    fn resolve_photo(&self, photo_id: u64) -> Option<tl_enums::InputPhoto> {
        if photo_id == 0 {
            return None;
        }
        Some(tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
            id: photo_id as i64,
            access_hash: 0,
            file_reference: Vec::new(),
        }))
    }

    fn resolve_document(&self, document_id: u64) -> Option<tl_enums::InputDocument> {
        if document_id == 0 {
            return None;
        }
        Some(tl_enums::InputDocument::InputDocument(
            tl_types::InputDocument {
                id: document_id as i64,
                access_hash: 0,
                file_reference: Vec::new(),
            },
        ))
    }

    fn is_audio_document(&self, _document_id: u64) -> bool {
        false
    }

    fn resolve_user(&self, user_id: u64) -> Option<tl_enums::InputUser> {
        if user_id == 0 {
            return None;
        }
        Some(tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: user_id as i64,
            access_hash: 0,
        }))
    }
}

/// In-memory lookup table implementing [`RichMediaResolver`] for real sessions and tests.
#[derive(Debug, Default, Clone)]
pub struct InMemoryMediaResolver {
    /// Registered `InputPhoto` handles by photo ID.
    pub photos: BTreeMap<u64, tl_enums::InputPhoto>,
    /// Registered `InputDocument` handles by document ID.
    pub documents: BTreeMap<u64, tl_enums::InputDocument>,
    /// Document IDs that should serialize as `pageBlockAudio` instead of `pageBlockDocument`.
    pub audio_documents: BTreeMap<u64, bool>,
    /// Registered `InputUser` handles by user ID.
    pub users: BTreeMap<u64, tl_enums::InputUser>,
}

impl RichMediaResolver for InMemoryMediaResolver {
    fn resolve_photo(&self, photo_id: u64) -> Option<tl_enums::InputPhoto> {
        self.photos.get(&photo_id).cloned()
    }

    fn resolve_document(&self, document_id: u64) -> Option<tl_enums::InputDocument> {
        self.documents.get(&document_id).cloned()
    }

    fn is_audio_document(&self, document_id: u64) -> bool {
        self.audio_documents
            .get(&document_id)
            .copied()
            .unwrap_or(false)
    }

    fn resolve_user(&self, user_id: u64) -> Option<tl_enums::InputUser> {
        self.users.get(&user_id).cloned()
    }
}

struct TlSerializeContext<'a> {
    resolver: &'a dyn RichMediaResolver,
    skip_unuploaded_media: bool,
    photos: BTreeMap<u64, tl_enums::InputPhoto>,
    documents: BTreeMap<u64, tl_enums::InputDocument>,
    users: BTreeMap<u64, tl_enums::InputUser>,
}

impl<'a> TlSerializeContext<'a> {
    fn collect_photo(&mut self, photo_id: u64) -> Option<u64> {
        let input = self.resolver.resolve_photo(photo_id)?;
        let id = match &input {
            tl_enums::InputPhoto::InputPhoto(p) => p.id as u64,
            tl_enums::InputPhoto::Empty => return None,
        };
        self.photos.entry(id).or_insert(input);
        Some(id)
    }

    fn collect_document(&mut self, document_id: u64) -> Option<u64> {
        let input = self.resolver.resolve_document(document_id)?;
        let id = match &input {
            tl_enums::InputDocument::InputDocument(d) => d.id as u64,
            tl_enums::InputDocument::Empty => return None,
        };
        self.documents.entry(id).or_insert(input);
        Some(id)
    }

    fn collect_user(&mut self, user_id: u64) -> bool {
        if user_id == 0 {
            return false;
        }
        if self.users.contains_key(&user_id) {
            return true;
        }
        if let Some(input) = self.resolver.resolve_user(user_id) {
            self.users.insert(user_id, input);
            true
        } else {
            false
        }
    }
}

fn make_tl_plain(s: &str) -> tl_enums::RichText {
    if s.is_empty() {
        tl_enums::RichText::TextEmpty
    } else {
        tl_enums::RichText::TextPlain(tl_types::TextPlain {
            text: s.to_string(),
        })
    }
}

fn join_tl_rich_parts(mut parts: Vec<tl_enums::RichText>) -> tl_enums::RichText {
    match parts.len() {
        0 => tl_enums::RichText::TextEmpty,
        1 => parts.remove(0),
        _ => tl_enums::RichText::TextConcat(tl_types::TextConcat { texts: parts }),
    }
}

fn wrap_tl_anchor(text: tl_enums::RichText, anchor_id: &str) -> tl_enums::RichText {
    if anchor_id.is_empty() {
        text
    } else {
        tl_enums::RichText::TextAnchor(tl_types::TextAnchor {
            text: Box::new(text),
            name: anchor_id.to_string(),
        })
    }
}

fn wrap_tl_anchors(
    mut text: tl_enums::RichText,
    node: &RichTextNode,
    block_anchor: &str,
) -> tl_enums::RichText {
    for extra in node.anchor_ids.iter().rev() {
        text = wrap_tl_anchor(text, extra);
    }
    text = wrap_tl_anchor(text, &node.anchor_id);
    wrap_tl_anchor(text, block_anchor)
}

fn skip_entity_for_range(entities: &[EntityInText], index: usize, skip: Option<usize>) -> bool {
    let Some(skip_idx) = skip else {
        return false;
    };
    if index == skip_idx {
        return true;
    }
    index < skip_idx
        && entities[index].offset == entities[skip_idx].offset
        && entities[index].length == entities[skip_idx].length
}

fn find_outer_entity_at(
    entities: &[EntityInText],
    pos: usize,
    till: usize,
    skip: Option<usize>,
) -> Option<usize> {
    for (idx, entity) in entities.iter().enumerate() {
        if skip_entity_for_range(entities, idx, skip) {
            continue;
        }
        if entity.offset == pos && entity.end() <= till {
            return Some(idx);
        }
        if entity.offset > pos {
            break;
        }
    }
    None
}

fn serialize_tl_button_style(
    color: RichButtonColor,
    link: bool,
) -> Option<tl_enums::RichButtonStyle> {
    if color == RichButtonColor::Normal && !link {
        return None;
    }
    Some(tl_enums::RichButtonStyle::RichButtonStyle(
        tl_types::RichButtonStyle {
            bg_primary: color == RichButtonColor::Primary,
            bg_danger: color == RichButtonColor::Danger,
            bg_success: color == RichButtonColor::Success,
            link,
        },
    ))
}

fn serialize_tl_inline_button_type(
    button_type: RichButtonType,
    data: &[u8],
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::InlineButtonType> {
    match button_type {
        RichButtonType::Url => Some(tl_enums::InlineButtonType::Url(
            tl_types::InlineButtonTypeUrl {
                url: String::from_utf8_lossy(data).into_owned(),
            },
        )),
        RichButtonType::CopyText => Some(tl_enums::InlineButtonType::Copy(
            tl_types::InlineButtonTypeCopy {
                copy_text: String::from_utf8_lossy(data).into_owned(),
            },
        )),
        RichButtonType::Disabled => Some(tl_enums::InlineButtonType::Disabled),
        RichButtonType::UserProfile => {
            let user_id = std::str::from_utf8(data).ok()?.parse::<u64>().ok()?;
            if user_id == 0 {
                return None;
            }
            ctx.collect_user(user_id);
            Some(tl_enums::InlineButtonType::UserProfile(
                tl_types::InlineButtonTypeUserProfile {
                    user_id: user_id as i64,
                },
            ))
        }
        RichButtonType::Callback | RichButtonType::CallbackWithPassword => Some(
            tl_enums::InlineButtonType::Callback(tl_types::InlineButtonTypeCallback {
                requires_password: button_type == RichButtonType::CallbackWithPassword,
                data: data.to_vec(),
            }),
        ),
        RichButtonType::WebView => Some(tl_enums::InlineButtonType::WebView(
            tl_types::InlineButtonTypeWebView {
                url: String::from_utf8_lossy(data).into_owned(),
            },
        )),
        RichButtonType::Game => Some(tl_enums::InlineButtonType::Game),
        RichButtonType::Buy => Some(tl_enums::InlineButtonType::Buy),
        RichButtonType::SwitchInline => Some(tl_enums::InlineButtonType::SwitchInline(
            tl_types::InlineButtonTypeSwitchInline {
                same_peer: false,
                query: String::from_utf8_lossy(data).into_owned(),
                peer_types: None,
            },
        )),
        RichButtonType::UrlAuth => Some(tl_enums::InlineButtonType::UrlAuth(
            tl_types::InlineButtonTypeUrlAuth {
                fwd_text: None,
                url: String::from_utf8_lossy(data).into_owned(),
                button_id: 0,
            },
        )),
    }
}

fn serialize_tl_rich_text_entity(
    text: &str,
    entities: &[EntityInText],
    idx: usize,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::RichText> {
    let entity = &entities[idx];
    let from = entity.offset;
    let end = entity.end();
    let byte_from = utf16_to_byte_offset(text, from);
    let byte_end = utf16_to_byte_offset(text, end);
    let segment = &text[byte_from..byte_end];
    let inner = serialize_tl_rich_text_range(text, entities, from, end, ctx, Some(idx))?;

    match entity.kind {
        EntityType::Bold => Some(tl_enums::RichText::TextBold(tl_types::TextBold {
            text: Box::new(inner),
        })),
        EntityType::Italic => Some(tl_enums::RichText::TextItalic(tl_types::TextItalic {
            text: Box::new(inner),
        })),
        EntityType::Underline => Some(tl_enums::RichText::TextUnderline(tl_types::TextUnderline {
            text: Box::new(inner),
        })),
        EntityType::StrikeOut => Some(tl_enums::RichText::TextStrike(tl_types::TextStrike {
            text: Box::new(inner),
        })),
        EntityType::Code => Some(tl_enums::RichText::TextFixed(tl_types::TextFixed {
            text: Box::new(inner),
        })),
        EntityType::Subscript => Some(tl_enums::RichText::TextSubscript(tl_types::TextSubscript {
            text: Box::new(inner),
        })),
        EntityType::Superscript => Some(tl_enums::RichText::TextSuperscript(
            tl_types::TextSuperscript {
                text: Box::new(inner),
            },
        )),
        EntityType::Marked => Some(tl_enums::RichText::TextMarked(tl_types::TextMarked {
            text: Box::new(inner),
        })),
        EntityType::Spoiler => Some(tl_enums::RichText::TextSpoiler(tl_types::TextSpoiler {
            text: Box::new(inner),
        })),
        EntityType::CustomUrl => {
            if let Some(email) = entity.data.strip_prefix("mailto:") {
                Some(tl_enums::RichText::TextEmail(tl_types::TextEmail {
                    text: Box::new(inner),
                    email: email.to_string(),
                }))
            } else if let Some(phone) = entity.data.strip_prefix("tel:") {
                Some(tl_enums::RichText::TextPhone(tl_types::TextPhone {
                    text: Box::new(inner),
                    phone: phone.to_string(),
                }))
            } else {
                let (url, webpage_id) = match decode_rich_page_link_url(&entity.data) {
                    Some(decoded) => (decoded.url, decoded.webpage_id as i64),
                    None => (entity.data.clone(), 0),
                };
                Some(tl_enums::RichText::TextUrl(tl_types::TextUrl {
                    text: Box::new(inner),
                    url,
                    webpage_id,
                }))
            }
        }
        EntityType::MentionName => {
            if let Ok(uid) = entity.data.parse::<u64>()
                && ctx.collect_user(uid)
            {
                Some(tl_enums::RichText::TextMentionName(
                    tl_types::TextMentionName {
                        text: Box::new(inner),
                        user_id: uid as i64,
                    },
                ))
            } else {
                Some(inner)
            }
        }
        EntityType::CustomEmoji => {
            if let Some(obj) = parse_inline_text_object_entity(&entity.data) {
                match obj {
                    InlineTextObjectEntity::Formula(f) => {
                        let source = if !f.trimmed_tex.is_empty() {
                            f.trimmed_tex
                        } else {
                            trim_math_source(&f.copy_source).to_string()
                        };
                        if source.is_empty() {
                            Some(make_tl_plain(segment))
                        } else {
                            Some(tl_enums::RichText::TextMath(tl_types::TextMath { source }))
                        }
                    }
                    InlineTextObjectEntity::IvImage(img) => {
                        Some(make_tl_plain(if img.replacement_text.is_empty() {
                            "[image]"
                        } else {
                            &img.replacement_text
                        }))
                    }
                    InlineTextObjectEntity::Button(btn) => {
                        if btn.label.text.is_empty() {
                            return Some(make_tl_plain(segment));
                        }
                        let ty = serialize_tl_inline_button_type(btn.button_type, &btn.data, ctx)?;
                        let label = serialize_tl_rich_text_value(&btn.label, ctx)?;
                        let style = serialize_tl_button_style(btn.color, btn.link);
                        Some(tl_enums::RichText::TextButton(tl_types::TextButton {
                            text: Box::new(label),
                            r#type: ty,
                            style,
                        }))
                    }
                }
            } else if let Ok(doc_id) = entity.data.parse::<u64>()
                && let Some(collected) = ctx.collect_document(doc_id)
            {
                Some(tl_enums::RichText::TextCustomEmoji(
                    tl_types::TextCustomEmoji {
                        document_id: collected as i64,
                        alt: if segment.is_empty() {
                            "@".to_string()
                        } else {
                            segment.to_string()
                        },
                    },
                ))
            } else {
                Some(make_tl_plain(segment))
            }
        }
        EntityType::FormattedDate => {
            let (date, flags) = deserialize_date_flags_data(&entity.data);
            if date == 0 {
                return Some(inner);
            }
            Some(tl_enums::RichText::TextDate(tl_types::TextDate {
                relative: flags.relative,
                short_time: flags.short_time,
                long_time: flags.long_time,
                short_date: flags.short_date,
                long_date: flags.long_date,
                day_of_week: flags.day_of_week,
                text: Box::new(inner),
                date,
            }))
        }
        _ => Some(inner),
    }
}

fn serialize_tl_rich_text_range(
    text: &str,
    entities: &[EntityInText],
    from: usize,
    till: usize,
    ctx: &mut TlSerializeContext<'_>,
    skip: Option<usize>,
) -> Option<tl_enums::RichText> {
    let mut parts = Vec::new();
    let mut pos = from;
    while pos < till {
        let mut next_entity_start = till;
        for (idx, entity) in entities.iter().enumerate() {
            if skip_entity_for_range(entities, idx, skip) {
                continue;
            }
            if entity.offset >= pos && entity.end() <= till {
                next_entity_start = entity.offset;
                break;
            }
        }
        if next_entity_start > pos {
            let b0 = utf16_to_byte_offset(text, pos);
            let b1 = utf16_to_byte_offset(text, next_entity_start);
            parts.push(make_tl_plain(&text[b0..b1]));
            pos = next_entity_start;
            continue;
        }
        let Some(entity_idx) = find_outer_entity_at(entities, pos, till, skip) else {
            let b0 = utf16_to_byte_offset(text, pos);
            let ch = text[b0..].chars().next()?;
            let b1 = b0 + ch.len_utf8();
            parts.push(make_tl_plain(&text[b0..b1]));
            pos += ch.len_utf16();
            continue;
        };
        let wrapped = serialize_tl_rich_text_entity(text, entities, entity_idx, ctx)?;
        parts.push(wrapped);
        pos = entities[entity_idx].end();
    }
    Some(join_tl_rich_parts(parts))
}

fn serialize_tl_rich_text_value(
    text: &TextWithEntities,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::RichText> {
    let mut sorted = text.clone();
    sorted.sort_entities();
    let total = sorted.utf16_len();
    serialize_tl_rich_text_range(&sorted.text, &sorted.entities, 0, total, ctx, None)
}

fn serialize_tl_rich_text_with_anchor(
    node: &RichTextNode,
    block_anchor: &str,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::RichText> {
    let base = serialize_tl_rich_text_value(&node.text, ctx)?;
    Some(wrap_tl_anchors(base, node, block_anchor))
}

fn serialize_tl_caption(
    caption: &RichTextNode,
    block_anchor: &str,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::PageCaption> {
    let text = serialize_tl_rich_text_with_anchor(caption, block_anchor, ctx)?;
    Some(tl_enums::PageCaption::PageCaption(tl_types::PageCaption {
        text,
        credit: tl_enums::RichText::TextEmpty,
    }))
}

// ---- FinalSubmit normalization (`iv_rich_message_serializer.cpp:905-1320`) ----

struct NormalizedBlocks {
    blocks: Vec<RichBlock>,
    has_meaningful: bool,
    has_real: bool,
}

fn paragraph_has_trimmable_edges(block: &RichBlock) -> bool {
    block.kind == RichBlockKind::Paragraph
        && block.anchor_id.is_empty()
        && block.text.is_structurally_empty()
}

fn trim_empty_paragraph_edges(blocks: &mut Vec<RichBlock>) {
    let mut from = 0;
    let mut till = blocks.len();
    while from < till && paragraph_has_trimmable_edges(&blocks[from]) {
        from += 1;
    }
    while till > from && paragraph_has_trimmable_edges(&blocks[till - 1]) {
        till -= 1;
    }
    if till < blocks.len() {
        blocks.truncate(till);
    }
    if from > 0 {
        blocks.drain(0..from);
    }
}

fn block_media_fully_uploaded(block: &RichBlock, resolver: &dyn RichMediaResolver) -> bool {
    match block.kind {
        RichBlockKind::Photo => resolver.resolve_photo(block.photo_id).is_some(),
        RichBlockKind::Video | RichBlockKind::Audio | RichBlockKind::File => {
            resolver.resolve_document(block.document_id).is_some()
        }
        RichBlockKind::GroupedMedia => block.media_items.iter().all(|item| match item.kind {
            RichBlockKind::Photo => resolver.resolve_photo(item.photo_id).is_some(),
            RichBlockKind::Video => resolver.resolve_document(item.document_id).is_some(),
            _ => false,
        }),
        _ => true,
    }
}

fn block_has_own_meaningful_content(block: &RichBlock, resolver: &dyn RichMediaResolver) -> bool {
    match block.kind {
        RichBlockKind::Heading
        | RichBlockKind::Paragraph
        | RichBlockKind::Footer
        | RichBlockKind::Thinking
        | RichBlockKind::Code => block.text.has_visible_text(),
        RichBlockKind::Quote => block.text.has_visible_text() || block.caption.has_visible_text(),
        RichBlockKind::Photo => resolver.resolve_photo(block.photo_id).is_some(),
        RichBlockKind::Video | RichBlockKind::Audio | RichBlockKind::File => {
            resolver.resolve_document(block.document_id).is_some()
        }
        RichBlockKind::Math => !block.formula.trim().is_empty(),
        RichBlockKind::Table | RichBlockKind::Details => block.text.has_visible_text(),
        RichBlockKind::GroupedMedia => {
            !block.media_items.is_empty() && block_media_fully_uploaded(block, resolver)
        }
        RichBlockKind::Map => block.zoom > 0,
        RichBlockKind::ButtonRow => !block.buttons.is_empty(),
        RichBlockKind::AuthorDate => block.text.has_visible_text() || block.date != 0,
        _ => false,
    }
}

fn normalize_final_submit_block(
    mut block: RichBlock,
    resolver: &dyn RichMediaResolver,
) -> (Option<RichBlock>, bool, bool) {
    match block.kind {
        RichBlockKind::Heading
        | RichBlockKind::Footer
        | RichBlockKind::Code
        | RichBlockKind::Thinking => {
            if !block.text.has_visible_text() {
                return (None, false, false);
            }
            (Some(block), true, true)
        }
        RichBlockKind::Math => {
            if block.formula.trim().is_empty() {
                return (None, false, false);
            }
            (Some(block), true, true)
        }
        RichBlockKind::Quote => {
            let mut meaningful = false;
            let mut real = false;
            if !block.blocks.is_empty() {
                let norm = normalize_final_submit_blocks(
                    std::mem::take(&mut block.blocks),
                    resolver,
                    true,
                );
                block.blocks = norm.blocks;
                meaningful |= norm.has_meaningful;
                real |= norm.has_real;
            }
            meaningful |= block.text.has_visible_text() || block.caption.has_visible_text();
            real |= block.text.has_visible_text();
            if block.blocks.is_empty() && !real {
                (None, meaningful, real)
            } else {
                (Some(block), meaningful, real)
            }
        }
        RichBlockKind::List => {
            let mut meaningful = false;
            let mut real = false;
            let mut items = Vec::with_capacity(block.list_items.len());
            for mut item in std::mem::take(&mut block.list_items) {
                if item.blocks.is_empty() {
                    let vis = item.text.has_visible_text();
                    meaningful |= vis;
                    real |= vis;
                } else {
                    let norm = normalize_final_submit_blocks(
                        std::mem::take(&mut item.blocks),
                        resolver,
                        true,
                    );
                    item.blocks = norm.blocks;
                    let vis = item.text.has_visible_text();
                    meaningful |= vis || norm.has_meaningful;
                    real |= vis || norm.has_real;
                }
                items.push(item);
            }
            block.list_items = items;
            if block.list_items.is_empty() {
                (None, meaningful, real)
            } else {
                (Some(block), meaningful, real)
            }
        }
        RichBlockKind::Table => {
            let mut meaningful = false;
            let mut real = false;
            let mut rows = Vec::with_capacity(block.table_rows.len());
            for row in std::mem::take(&mut block.table_rows) {
                if row.cells.is_empty() {
                    continue;
                }
                for cell in &row.cells {
                    if cell.text.has_visible_text() {
                        meaningful = true;
                        real = true;
                    }
                }
                rows.push(row);
            }
            block.table_rows = rows;
            if !meaningful {
                (None, false, false)
            } else {
                (Some(block), true, real)
            }
        }
        RichBlockKind::Details => {
            let norm =
                normalize_final_submit_blocks(std::mem::take(&mut block.blocks), resolver, true);
            block.blocks = norm.blocks;
            if block.blocks.is_empty() {
                (None, norm.has_meaningful, norm.has_real)
            } else {
                (Some(block), norm.has_meaningful, norm.has_real)
            }
        }
        _ => {
            let own = block_has_own_meaningful_content(&block, resolver);
            (Some(block), own, own)
        }
    }
}

fn normalize_final_submit_blocks(
    blocks: Vec<RichBlock>,
    resolver: &dyn RichMediaResolver,
    trim_edges: bool,
) -> NormalizedBlocks {
    let mut out = Vec::with_capacity(blocks.len());
    let mut has_meaningful = false;
    let mut has_real = false;
    for block in blocks {
        let (kept, m, r) = normalize_final_submit_block(block, resolver);
        has_meaningful |= m;
        has_real |= r;
        if let Some(b) = kept {
            out.push(b);
        }
    }
    if trim_edges {
        trim_empty_paragraph_edges(&mut out);
    }
    NormalizedBlocks {
        blocks: out,
        has_meaningful,
        has_real,
    }
}

fn serialize_tl_table_cell(
    cell: &RichTableCell,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::PageTableCell> {
    let colspan = cell.colspan.max(1);
    let rowspan = cell.rowspan.max(1);
    let text = if cell.text.has_content() {
        Some(serialize_tl_rich_text_with_anchor(&cell.text, "", ctx)?)
    } else {
        None
    };
    Some(tl_enums::PageTableCell::PageTableCell(
        tl_types::PageTableCell {
            header: cell.header,
            align_center: cell.alignment == RichTableAlignment::Center,
            align_right: cell.alignment == RichTableAlignment::Right,
            valign_middle: cell.vertical_alignment == RichTableVerticalAlignment::Middle,
            valign_bottom: cell.vertical_alignment == RichTableVerticalAlignment::Bottom,
            text,
            colspan: (colspan != 1).then_some(colspan),
            rowspan: (rowspan != 1).then_some(rowspan),
        },
    ))
}

fn serialize_tl_block(
    block: &RichBlock,
    ctx: &mut TlSerializeContext<'_>,
) -> Option<tl_enums::PageBlock> {
    match block.kind {
        RichBlockKind::Heading => {
            let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            Some(match block.heading_level.clamp(1, 6) {
                1 => tl_enums::PageBlock::Heading1(tl_types::PageBlockHeading1 { text }),
                2 => tl_enums::PageBlock::Heading2(tl_types::PageBlockHeading2 { text }),
                3 => tl_enums::PageBlock::Heading3(tl_types::PageBlockHeading3 { text }),
                4 => tl_enums::PageBlock::Heading4(tl_types::PageBlockHeading4 { text }),
                5 => tl_enums::PageBlock::Heading5(tl_types::PageBlockHeading5 { text }),
                _ => tl_enums::PageBlock::Heading6(tl_types::PageBlockHeading6 { text }),
            })
        }
        RichBlockKind::Paragraph => {
            let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Paragraph(
                tl_types::PageBlockParagraph { text },
            ))
        }
        RichBlockKind::Footer => {
            let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Footer(tl_types::PageBlockFooter {
                text,
            }))
        }
        RichBlockKind::Divider => Some(tl_enums::PageBlock::Divider),
        RichBlockKind::Anchor => {
            if block.anchor_id.is_empty() {
                return None;
            }
            Some(tl_enums::PageBlock::Anchor(tl_types::PageBlockAnchor {
                name: block.anchor_id.clone(),
            }))
        }
        RichBlockKind::Quote => {
            if block.pullquote {
                if !block.blocks.is_empty() {
                    return None;
                }
                let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
                let caption = serialize_tl_rich_text_with_anchor(&block.caption, "", ctx)?;
                return Some(tl_enums::PageBlock::Pullquote(
                    tl_types::PageBlockPullquote { text, caption },
                ));
            }
            if block.blocks.is_empty() {
                let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
                let caption = serialize_tl_rich_text_with_anchor(&block.caption, "", ctx)?;
                return Some(tl_enums::PageBlock::Blockquote(
                    tl_types::PageBlockBlockquote {
                        collapsed: block.collapsed,
                        text,
                        caption,
                    },
                ));
            }
            let mut nested = Vec::new();
            if block.text.has_content() {
                let text = serialize_tl_rich_text_with_anchor(&block.text, "", ctx)?;
                nested.push(tl_enums::PageBlock::Paragraph(
                    tl_types::PageBlockParagraph { text },
                ));
            }
            nested.extend(serialize_tl_blocks(&block.blocks, ctx)?);
            let caption =
                serialize_tl_rich_text_with_anchor(&block.caption, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::BlockquoteBlocks(
                tl_types::PageBlockBlockquoteBlocks {
                    blocks: nested,
                    caption,
                },
            ))
        }
        RichBlockKind::List => {
            if block.list_kind == RichListKind::Ordered {
                let mut items = Vec::with_capacity(block.list_items.len());
                for item in &block.list_items {
                    let checkbox = item.task_state != RichTaskState::None;
                    let checked = item.task_state == RichTaskState::Checked;
                    if !item.blocks.is_empty() {
                        let mut blocks = Vec::new();
                        if item.text.has_content() || !item.anchor_id.is_empty() {
                            let text = serialize_tl_rich_text_with_anchor(
                                &item.text,
                                &item.anchor_id,
                                ctx,
                            )?;
                            blocks.push(tl_enums::PageBlock::Paragraph(
                                tl_types::PageBlockParagraph { text },
                            ));
                        }
                        blocks.extend(serialize_tl_blocks(&item.blocks, ctx)?);
                        items.push(tl_enums::PageListOrderedItem::Blocks(
                            tl_types::PageListOrderedItemBlocks {
                                checkbox,
                                checked,
                                num: item.number.num.clone(),
                                blocks,
                                value: item.number.value,
                                r#type: item.number.list_type.clone(),
                            },
                        ));
                    } else {
                        let text =
                            serialize_tl_rich_text_with_anchor(&item.text, &item.anchor_id, ctx)?;
                        items.push(tl_enums::PageListOrderedItem::Text(
                            tl_types::PageListOrderedItemText {
                                checkbox,
                                checked,
                                num: item.number.num.clone(),
                                text,
                                value: item.number.value,
                                r#type: item.number.list_type.clone(),
                            },
                        ));
                    }
                }
                return Some(tl_enums::PageBlock::OrderedList(
                    tl_types::PageBlockOrderedList {
                        reversed: block.ordered_list.reversed,
                        items,
                        start: block.ordered_list.start,
                        r#type: block.ordered_list.list_type.clone(),
                    },
                ));
            }
            let mut items = Vec::with_capacity(block.list_items.len());
            for item in &block.list_items {
                let checkbox = item.task_state != RichTaskState::None;
                let checked = item.task_state == RichTaskState::Checked;
                if !item.blocks.is_empty() {
                    let mut blocks = Vec::new();
                    if item.text.has_content() || !item.anchor_id.is_empty() {
                        let text =
                            serialize_tl_rich_text_with_anchor(&item.text, &item.anchor_id, ctx)?;
                        blocks.push(tl_enums::PageBlock::Paragraph(
                            tl_types::PageBlockParagraph { text },
                        ));
                    }
                    blocks.extend(serialize_tl_blocks(&item.blocks, ctx)?);
                    items.push(tl_enums::PageListItem::Blocks(
                        tl_types::PageListItemBlocks {
                            checkbox,
                            checked,
                            blocks,
                        },
                    ));
                } else {
                    let text =
                        serialize_tl_rich_text_with_anchor(&item.text, &item.anchor_id, ctx)?;
                    items.push(tl_enums::PageListItem::Text(tl_types::PageListItemText {
                        checkbox,
                        checked,
                        text,
                    }));
                }
            }
            Some(tl_enums::PageBlock::List(tl_types::PageBlockList { items }))
        }
        RichBlockKind::Photo => {
            let photo_id = ctx.collect_photo(block.photo_id)?;
            let caption = serialize_tl_caption(&block.caption, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Photo(tl_types::PageBlockPhoto {
                spoiler: block.spoiler,
                photo_id: photo_id as i64,
                caption,
                url: None,
                webpage_id: None,
            }))
        }
        RichBlockKind::Video => {
            let document_id = ctx.collect_document(block.document_id)?;
            let caption = serialize_tl_caption(&block.caption, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Video(tl_types::PageBlockVideo {
                autoplay: block.autoplay,
                r#loop: block.loop_video,
                spoiler: block.spoiler,
                video_id: document_id as i64,
                caption,
            }))
        }
        RichBlockKind::Audio | RichBlockKind::File => {
            let document_id = ctx.collect_document(block.document_id)?;
            let caption = serialize_tl_caption(&block.caption, &block.anchor_id, ctx)?;
            if block.kind == RichBlockKind::Audio || ctx.resolver.is_audio_document(document_id) {
                Some(tl_enums::PageBlock::Audio(tl_types::PageBlockAudio {
                    audio_id: document_id as i64,
                    caption,
                }))
            } else {
                Some(tl_enums::PageBlock::Document(tl_types::PageBlockDocument {
                    document_id: document_id as i64,
                    caption,
                }))
            }
        }
        RichBlockKind::Math => Some(tl_enums::PageBlock::Math(tl_types::PageBlockMath {
            source: block.formula.clone(),
        })),
        RichBlockKind::Table => {
            let title = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            let mut rows = Vec::with_capacity(block.table_rows.len());
            for row in &block.table_rows {
                let mut cells = Vec::with_capacity(row.cells.len());
                for cell in &row.cells {
                    cells.push(serialize_tl_table_cell(cell, ctx)?);
                }
                rows.push(tl_enums::PageTableRow::PageTableRow(
                    tl_types::PageTableRow { cells },
                ));
            }
            Some(tl_enums::PageBlock::Table(tl_types::PageBlockTable {
                bordered: block.bordered,
                striped: block.striped,
                compact: block.compact,
                title,
                rows,
            }))
        }
        RichBlockKind::Details => {
            let title = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            let blocks = serialize_tl_blocks(&block.blocks, ctx)?;
            Some(tl_enums::PageBlock::Details(tl_types::PageBlockDetails {
                open: block.open,
                blocks,
                title,
            }))
        }
        RichBlockKind::Map => {
            if block.zoom <= 0 {
                return None;
            }
            let w = if block.width > 0 {
                block.width
            } else {
                DEFAULT_MAP_WIDTH
            };
            let h = if block.height > 0 {
                block.height
            } else {
                DEFAULT_MAP_HEIGHT
            };
            let caption = serialize_tl_caption(&block.caption, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::InputPageBlockMap(
                tl_types::InputPageBlockMap {
                    geo: tl_enums::InputGeoPoint::InputGeoPoint(tl_types::InputGeoPoint {
                        lat: block.latitude,
                        long: block.longitude,
                        accuracy_radius: None,
                    }),
                    zoom: block.zoom,
                    w,
                    h,
                    caption,
                },
            ))
        }
        RichBlockKind::Code => {
            if !block.blocks.is_empty() {
                return None;
            }
            let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Preformatted(
                tl_types::PageBlockPreformatted {
                    text,
                    language: block.language.clone(),
                },
            ))
        }
        RichBlockKind::Thinking => {
            let text = serialize_tl_rich_text_with_anchor(&block.text, &block.anchor_id, ctx)?;
            Some(tl_enums::PageBlock::Thinking(tl_types::PageBlockThinking {
                text,
            }))
        }
        RichBlockKind::GroupedMedia => {
            let mut items = Vec::with_capacity(block.media_items.len());
            let empty_caption = serialize_tl_caption(&RichTextNode::default(), "", ctx)?;
            for item in &block.media_items {
                match item.kind {
                    RichBlockKind::Photo => {
                        let photo_id = ctx.collect_photo(item.photo_id)?;
                        items.push(tl_enums::PageBlock::Photo(tl_types::PageBlockPhoto {
                            spoiler: item.spoiler,
                            photo_id: photo_id as i64,
                            caption: empty_caption.clone(),
                            url: None,
                            webpage_id: None,
                        }));
                    }
                    RichBlockKind::Video => {
                        let document_id = ctx.collect_document(item.document_id)?;
                        items.push(tl_enums::PageBlock::Video(tl_types::PageBlockVideo {
                            autoplay: item.autoplay,
                            r#loop: item.loop_video,
                            spoiler: item.spoiler,
                            video_id: document_id as i64,
                            caption: empty_caption.clone(),
                        }));
                    }
                    _ => return None,
                }
            }
            if items.is_empty() {
                return None;
            }
            let caption = serialize_tl_caption(&block.caption, &block.anchor_id, ctx)?;
            Some(match block.media_intent {
                GroupedMediaIntent::Slideshow => {
                    tl_enums::PageBlock::Slideshow(tl_types::PageBlockSlideshow { items, caption })
                }
                GroupedMediaIntent::Collage => {
                    tl_enums::PageBlock::Collage(tl_types::PageBlockCollage { items, caption })
                }
            })
        }
        RichBlockKind::ButtonRow => {
            if block.buttons.is_empty() {
                return None;
            }
            let mut buttons = Vec::with_capacity(block.buttons.len());
            for btn in &block.buttons {
                let text = serialize_tl_rich_text_value(&btn.text.text, ctx)?;
                let ty = serialize_tl_inline_button_type(btn.button_type, &btn.data, ctx)?;
                let style = serialize_tl_button_style(btn.color, false);
                buttons.push(tl_enums::PageButton::PageButton(tl_types::PageButton {
                    text,
                    r#type: ty,
                    style,
                }));
            }
            Some(tl_enums::PageBlock::ButtonRow(
                tl_types::PageBlockButtonRow {
                    align_left: block.button_alignment == RichButtonAlignment::Left,
                    align_center: block.button_alignment == RichButtonAlignment::Center,
                    align_right: block.button_alignment == RichButtonAlignment::Right,
                    buttons,
                },
            ))
        }
        RichBlockKind::Unsupported
        | RichBlockKind::AuthorDate
        | RichBlockKind::Embed
        | RichBlockKind::EmbedPost
        | RichBlockKind::Channel
        | RichBlockKind::RelatedArticles => None,
    }
}

fn serialize_tl_blocks(
    blocks: &[RichBlock],
    ctx: &mut TlSerializeContext<'_>,
) -> Option<Vec<tl_enums::PageBlock>> {
    let mut result = Vec::with_capacity(blocks.len());
    for block in blocks {
        if ctx.skip_unuploaded_media && !block_media_fully_uploaded(block, ctx.resolver) {
            continue;
        }
        result.push(serialize_tl_block(block, ctx)?);
    }
    Some(result)
}

/// Serializes a [`RichPage`] to Layer-233 TL `InputRichMessage` (`Iv::SerializeInputRichMessage`).
#[must_use]
pub fn serialize_input_rich_message(
    page: &RichPage,
    mode: SerializeRichMessageMode,
    resolver: &dyn RichMediaResolver,
) -> SerializeRichMessageResult {
    let mut ctx = TlSerializeContext {
        resolver,
        skip_unuploaded_media: mode == SerializeRichMessageMode::Draft,
        photos: BTreeMap::new(),
        documents: BTreeMap::new(),
        users: BTreeMap::new(),
    };
    let normalized;
    let source_blocks: &[RichBlock] = if mode == SerializeRichMessageMode::FinalSubmit {
        normalized = normalize_final_submit_blocks(page.blocks.clone(), resolver, true);
        &normalized.blocks
    } else {
        normalized = NormalizedBlocks {
            blocks: Vec::new(),
            has_meaningful: true,
            has_real: true,
        };
        &page.blocks
    };

    let Some(blocks) = serialize_tl_blocks(source_blocks, &mut ctx) else {
        return SerializeRichMessageResult {
            status: SerializeRichMessageStatus::Failed,
            value: None,
        };
    };
    if (mode == SerializeRichMessageMode::FinalSubmit && !normalized.has_real)
        || (mode == SerializeRichMessageMode::Draft && blocks.is_empty())
    {
        return SerializeRichMessageResult {
            status: SerializeRichMessageStatus::EmptyContent,
            value: None,
        };
    }

    let photos: Vec<tl_enums::InputPhoto> = ctx.photos.into_values().collect();
    let documents: Vec<tl_enums::InputDocument> = ctx.documents.into_values().collect();
    let users: Vec<tl_enums::InputUser> = ctx.users.into_values().collect();

    SerializeRichMessageResult {
        status: SerializeRichMessageStatus::Success,
        value: Some(tl_types::InputRichMessage {
            rtl: determine_rich_page_rtl(page),
            noautolink: false,
            blocks,
            photos: (!photos.is_empty()).then_some(photos),
            documents: (!documents.is_empty()).then_some(documents),
            users: (!users.is_empty()).then_some(users),
        }),
    }
}

/// Serializes a [`RichPage`] directly to Layer-233 TL `PageBlock`s.
pub fn rich_page_to_tl_blocks(
    page: &RichPage,
    mode: SerializeRichMessageMode,
    resolver: &dyn RichMediaResolver,
) -> Result<Vec<tl_enums::PageBlock>, SerializeRichMessageStatus> {
    let res = serialize_input_rich_message(page, mode, resolver);
    match res.value {
        Some(msg) => Ok(msg.blocks),
        None => Err(res.status),
    }
}

// ============================================================================
// Bridge to/from `crate::model::Page` (HTML Instant View pipeline)
// ============================================================================

fn tl_rich_text_to_model(tl: &tl_enums::RichText, depth: usize) -> ModelRichText {
    if depth >= MAX_PARSED_BLOCK_DEPTH {
        return ModelRichText::Empty;
    }
    let next = depth + 1;
    match tl {
        tl_enums::RichText::TextEmpty => ModelRichText::Empty,
        tl_enums::RichText::TextPlain(t) => ModelRichText::Plain(t.text.clone()),
        tl_enums::RichText::TextBold(t) => {
            ModelRichText::Bold(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextItalic(t) => {
            ModelRichText::Italic(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextUnderline(t) => {
            ModelRichText::Underline(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextStrike(t) => {
            ModelRichText::Strike(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextFixed(t) => {
            ModelRichText::Fixed(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextUrl(t) => ModelRichText::Url {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            url: t.url.clone(),
            webpage_id: t.webpage_id,
        },
        tl_enums::RichText::TextEmail(t) => ModelRichText::Email {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            email: t.email.clone(),
        },
        tl_enums::RichText::TextPhone(t) => ModelRichText::Phone {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            phone: t.phone.clone(),
        },
        tl_enums::RichText::TextSubscript(t) => {
            ModelRichText::Subscript(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextSuperscript(t) => {
            ModelRichText::Superscript(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextMarked(t) => {
            ModelRichText::Marked(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextImage(t) => ModelRichText::Image {
            document_id: t.document_id,
            width: t.w,
            height: t.h,
        },
        tl_enums::RichText::TextAnchor(t) => ModelRichText::Anchor {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            name: t.name.clone(),
        },
        tl_enums::RichText::TextConcat(t) => ModelRichText::Concat(
            t.texts
                .iter()
                .map(|p| tl_rich_text_to_model(p, next))
                .collect(),
        ),
        tl_enums::RichText::TextSpoiler(t) => {
            ModelRichText::Spoiler(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextMath(t) => ModelRichText::Math {
            source: t.source.clone(),
        },
        tl_enums::RichText::TextCustomEmoji(t) => ModelRichText::CustomEmoji {
            document_id: t.document_id,
            alt: t.alt.clone(),
        },
        tl_enums::RichText::TextMention(t) => {
            ModelRichText::Mention(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextHashtag(t) => {
            ModelRichText::Hashtag(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextBotCommand(t) => {
            ModelRichText::BotCommand(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextCashtag(t) => {
            ModelRichText::Cashtag(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextAutoUrl(t) => {
            ModelRichText::AutoUrl(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextAutoEmail(t) => {
            ModelRichText::AutoEmail(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextAutoPhone(t) => {
            ModelRichText::AutoPhone(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextBankCard(t) => {
            ModelRichText::BankCard(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextTonAddress(t) => {
            ModelRichText::TonAddress(Box::new(tl_rich_text_to_model(&t.text, next)))
        }
        tl_enums::RichText::TextMentionName(t) => ModelRichText::MentionName {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            user_id: t.user_id,
        },
        tl_enums::RichText::TextDate(t) => ModelRichText::Date {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            date: t.date,
            flags: DateFlags {
                relative: t.relative,
                short_time: t.short_time,
                long_time: t.long_time,
                short_date: t.short_date,
                long_date: t.long_date,
                day_of_week: t.day_of_week,
            },
        },
        tl_enums::RichText::TextDiff(t) => ModelRichText::Diff {
            text: Box::new(tl_rich_text_to_model(&t.text, next)),
            old_text: Box::new(tl_rich_text_to_model(&t.old_text, next)),
        },
        tl_enums::RichText::TextButton(t) => tl_rich_text_to_model(&t.text, next),
    }
}

fn rich_text_node_to_model(node: &RichTextNode, block_anchor: &str) -> ModelRichText {
    let mut ctx = TlSerializeContext {
        resolver: &DirectIdMediaResolver,
        skip_unuploaded_media: false,
        photos: BTreeMap::new(),
        documents: BTreeMap::new(),
        users: BTreeMap::new(),
    };
    let tl = serialize_tl_rich_text_with_anchor(node, block_anchor, &mut ctx)
        .unwrap_or(tl_enums::RichText::TextEmpty);
    tl_rich_text_to_model(&tl, 0)
}

fn rich_block_to_model(block: &RichBlock) -> PageBlock {
    match block.kind {
        RichBlockKind::Unsupported | RichBlockKind::ButtonRow => PageBlock::Unsupported,
        RichBlockKind::Heading => PageBlock::Heading {
            level: block.heading_level.clamp(1, 6),
            text: rich_text_node_to_model(&block.text, &block.anchor_id),
        },
        RichBlockKind::Paragraph => {
            PageBlock::Paragraph(rich_text_node_to_model(&block.text, &block.anchor_id))
        }
        RichBlockKind::Footer => {
            PageBlock::Footer(rich_text_node_to_model(&block.text, &block.anchor_id))
        }
        RichBlockKind::Thinking => {
            PageBlock::Thinking(rich_text_node_to_model(&block.text, &block.anchor_id))
        }
        RichBlockKind::AuthorDate => PageBlock::AuthorDate {
            author: rich_text_node_to_model(&block.text, &block.anchor_id),
            published_date: block.date,
        },
        RichBlockKind::Code => PageBlock::Preformatted {
            text: rich_text_node_to_model(&block.text, &block.anchor_id),
            language: block.language.clone(),
        },
        RichBlockKind::Divider => PageBlock::Divider,
        RichBlockKind::Anchor => PageBlock::Anchor {
            name: block.anchor_id.clone(),
        },
        RichBlockKind::List => {
            let map_task = |t: RichTaskState| match t {
                RichTaskState::None => ModelTaskState::None,
                RichTaskState::Unchecked => ModelTaskState::Unchecked,
                RichTaskState::Checked => ModelTaskState::Checked,
            };
            if block.list_kind == RichListKind::Ordered {
                PageBlock::OrderedList {
                    items: block
                        .list_items
                        .iter()
                        .map(|item| PageListOrderedItem {
                            task_state: map_task(item.task_state),
                            num: item.number.num.clone(),
                            value: item.number.value,
                            list_type: item.number.list_type.clone(),
                            content: if item.blocks.is_empty() {
                                ListItemContent::Text(rich_text_node_to_model(
                                    &item.text,
                                    &item.anchor_id,
                                ))
                            } else {
                                ListItemContent::Blocks(
                                    item.blocks.iter().map(rich_block_to_model).collect(),
                                )
                            },
                        })
                        .collect(),
                    reversed: block.ordered_list.reversed,
                    start: block.ordered_list.start,
                    list_type: block.ordered_list.list_type.clone(),
                }
            } else {
                PageBlock::List {
                    items: block
                        .list_items
                        .iter()
                        .map(|item| PageListItem {
                            task_state: map_task(item.task_state),
                            content: if item.blocks.is_empty() {
                                ListItemContent::Text(rich_text_node_to_model(
                                    &item.text,
                                    &item.anchor_id,
                                ))
                            } else {
                                ListItemContent::Blocks(
                                    item.blocks.iter().map(rich_block_to_model).collect(),
                                )
                            },
                        })
                        .collect(),
                }
            }
        }
        RichBlockKind::Quote => {
            if block.pullquote {
                PageBlock::Pullquote {
                    text: rich_text_node_to_model(&block.text, &block.anchor_id),
                    caption: rich_text_node_to_model(&block.caption, ""),
                }
            } else if block.blocks.is_empty() {
                PageBlock::Blockquote {
                    text: rich_text_node_to_model(&block.text, &block.anchor_id),
                    caption: rich_text_node_to_model(&block.caption, ""),
                    collapsed: block.collapsed,
                }
            } else {
                PageBlock::BlockquoteBlocks {
                    blocks: block.blocks.iter().map(rich_block_to_model).collect(),
                    caption: rich_text_node_to_model(&block.caption, &block.anchor_id),
                }
            }
        }
        RichBlockKind::Photo => PageBlock::Photo {
            photo_id: block.photo_id as i64,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
            url: (!block.url.is_empty()).then(|| block.url.clone()),
            webpage_id: None,
            spoiler: block.spoiler,
        },
        RichBlockKind::Video => PageBlock::Video {
            video_id: block.document_id as i64,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
            autoplay: block.autoplay,
            loop_video: block.loop_video,
            spoiler: block.spoiler,
        },
        RichBlockKind::Audio => PageBlock::Audio {
            audio_id: block.document_id as i64,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
        },
        RichBlockKind::File => PageBlock::Document {
            document_id: block.document_id as i64,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
        },
        RichBlockKind::Math => PageBlock::Math {
            source: block.formula.clone(),
        },
        RichBlockKind::Table => PageBlock::Table {
            title: rich_text_node_to_model(&block.text, &block.anchor_id),
            rows: block
                .table_rows
                .iter()
                .map(|r| PageTableRow {
                    cells: r
                        .cells
                        .iter()
                        .map(|c| PageTableCell {
                            text: c
                                .text
                                .has_content()
                                .then(|| rich_text_node_to_model(&c.text, "")),
                            header: c.header,
                            colspan: c.colspan,
                            rowspan: c.rowspan,
                            align: match c.alignment {
                                RichTableAlignment::Left => TableAlign::Left,
                                RichTableAlignment::Center => TableAlign::Center,
                                RichTableAlignment::Right => TableAlign::Right,
                            },
                            valign: match c.vertical_alignment {
                                RichTableVerticalAlignment::Top => TableValign::Top,
                                RichTableVerticalAlignment::Middle => TableValign::Middle,
                                RichTableVerticalAlignment::Bottom => TableValign::Bottom,
                            },
                        })
                        .collect(),
                })
                .collect(),
            bordered: block.bordered,
            striped: block.striped,
            compact: block.compact,
        },
        RichBlockKind::Details => PageBlock::Details {
            title: rich_text_node_to_model(&block.text, &block.anchor_id),
            blocks: block.blocks.iter().map(rich_block_to_model).collect(),
            open: block.open,
        },
        RichBlockKind::Map => PageBlock::Map {
            geo: GeoPoint {
                lat: block.latitude,
                lon: block.longitude,
                access_hash: block.access_hash as i64,
            },
            zoom: block.zoom,
            width: block.width,
            height: block.height,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
        },
        RichBlockKind::Channel => PageBlock::Channel(PageChannel {
            id: block.channel_id as i64,
            title: block.channel_title.clone(),
            username: (!block.username.is_empty()).then(|| block.username.clone()),
            joined: false,
        }),
        RichBlockKind::RelatedArticles => PageBlock::RelatedArticles {
            title: rich_text_node_to_model(&block.text, &block.anchor_id),
            articles: block
                .related_articles
                .iter()
                .map(|a| PageRelatedArticle {
                    url: a.url.clone(),
                    webpage_id: a.webpage_id as i64,
                    title: (!a.title.is_empty()).then(|| a.title.clone()),
                    description: (!a.description.is_empty()).then(|| a.description.clone()),
                    photo_id: (a.photo_id != 0).then_some(a.photo_id as i64),
                    author: (!a.author.is_empty()).then(|| a.author.clone()),
                    published_date: (a.published_date != 0).then_some(a.published_date),
                })
                .collect(),
        },
        RichBlockKind::Embed => PageBlock::Embed {
            url: (!block.url.is_empty()).then(|| block.url.clone()),
            html: (!block.html.is_empty())
                .then(|| String::from_utf8_lossy(&block.html).into_owned()),
            poster_photo_id: (block.photo_id != 0).then_some(block.photo_id as i64),
            width: (block.width > 0).then_some(block.width),
            height: (block.height > 0).then_some(block.height),
            full_width: block.full_width,
            allow_scrolling: block.allow_scrolling,
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
        },
        RichBlockKind::EmbedPost => PageBlock::EmbedPost {
            url: block.url.clone(),
            webpage_id: 0,
            author_photo_id: block.photo_id as i64,
            author: block.author.clone(),
            date: block.date,
            blocks: block.blocks.iter().map(rich_block_to_model).collect(),
            caption: PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            },
        },
        RichBlockKind::GroupedMedia => {
            let items = block
                .media_items
                .iter()
                .filter_map(|item| match item.kind {
                    RichBlockKind::Photo => Some(PageBlock::Photo {
                        photo_id: item.photo_id as i64,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: item.spoiler,
                    }),
                    RichBlockKind::Video => Some(PageBlock::Video {
                        video_id: item.document_id as i64,
                        caption: PageCaption::default(),
                        autoplay: item.autoplay,
                        loop_video: item.loop_video,
                        spoiler: item.spoiler,
                    }),
                    _ => None,
                })
                .collect();
            let caption = PageCaption {
                text: rich_text_node_to_model(&block.caption, &block.anchor_id),
                credit: ModelRichText::Empty,
            };
            match block.media_intent {
                GroupedMediaIntent::Slideshow => PageBlock::Slideshow { items, caption },
                GroupedMediaIntent::Collage => PageBlock::Collage { items, caption },
            }
        }
    }
}

/// Converts a [`RichPage`] into [`model::Page`] for rendering with [`crate::prepare::prepare_page`].
#[must_use]
pub fn rich_page_to_model_page(page: &RichPage) -> Page {
    Page {
        id: 0,
        hash: 0,
        url: page.url.clone(),
        site_name: None,
        part: page.part,
        rtl: page.rtl || determine_rich_page_rtl(page),
        v2: true,
        views: (page.views > 0).then_some(page.views),
        blocks: page.blocks.iter().map(rich_block_to_model).collect(),
        photos: Vec::new(),
        documents: Vec::new(),
    }
}

/// Converts a [`model::Page`] into an editable [`RichPage`].
#[must_use]
pub fn rich_page_from_model_page(page: &Page) -> RichPage {
    let mut tl_blocks = Vec::with_capacity(page.blocks.len());
    for block in &page.blocks {
        tl_blocks.push(model_block_to_tl(block, 0));
    }
    let mut out = rich_page_from_tl_blocks(&tl_blocks, RichParseMode::Normal);
    out.url = page.url.clone();
    out.rtl = page.rtl;
    out.part = page.part;
    out.views = page.views.unwrap_or(0);
    out
}

fn model_rich_text_to_tl(rt: &ModelRichText, depth: usize) -> tl_enums::RichText {
    if depth >= MAX_PARSED_BLOCK_DEPTH {
        return tl_enums::RichText::TextEmpty;
    }
    let next = depth + 1;
    match rt {
        ModelRichText::Empty => tl_enums::RichText::TextEmpty,
        ModelRichText::Plain(s) => make_tl_plain(s),
        ModelRichText::Bold(t) => tl_enums::RichText::TextBold(tl_types::TextBold {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Italic(t) => tl_enums::RichText::TextItalic(tl_types::TextItalic {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Underline(t) => tl_enums::RichText::TextUnderline(tl_types::TextUnderline {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Strike(t) => tl_enums::RichText::TextStrike(tl_types::TextStrike {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Fixed(t) => tl_enums::RichText::TextFixed(tl_types::TextFixed {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Url {
            text,
            url,
            webpage_id,
        } => tl_enums::RichText::TextUrl(tl_types::TextUrl {
            text: Box::new(model_rich_text_to_tl(text, next)),
            url: url.clone(),
            webpage_id: *webpage_id,
        }),
        ModelRichText::Email { text, email } => {
            tl_enums::RichText::TextEmail(tl_types::TextEmail {
                text: Box::new(model_rich_text_to_tl(text, next)),
                email: email.clone(),
            })
        }
        ModelRichText::Phone { text, phone } => {
            tl_enums::RichText::TextPhone(tl_types::TextPhone {
                text: Box::new(model_rich_text_to_tl(text, next)),
                phone: phone.clone(),
            })
        }
        ModelRichText::Subscript(t) => tl_enums::RichText::TextSubscript(tl_types::TextSubscript {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Superscript(t) => {
            tl_enums::RichText::TextSuperscript(tl_types::TextSuperscript {
                text: Box::new(model_rich_text_to_tl(t, next)),
            })
        }
        ModelRichText::Marked(t) => tl_enums::RichText::TextMarked(tl_types::TextMarked {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Image {
            document_id,
            width,
            height,
        } => tl_enums::RichText::TextImage(tl_types::TextImage {
            document_id: *document_id,
            w: *width,
            h: *height,
        }),
        ModelRichText::Anchor { text, name } => {
            tl_enums::RichText::TextAnchor(tl_types::TextAnchor {
                text: Box::new(model_rich_text_to_tl(text, next)),
                name: name.clone(),
            })
        }
        ModelRichText::Concat(parts) => tl_enums::RichText::TextConcat(tl_types::TextConcat {
            texts: parts
                .iter()
                .map(|p| model_rich_text_to_tl(p, next))
                .collect(),
        }),
        ModelRichText::Spoiler(t) => tl_enums::RichText::TextSpoiler(tl_types::TextSpoiler {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Math { source } => tl_enums::RichText::TextMath(tl_types::TextMath {
            source: source.clone(),
        }),
        ModelRichText::CustomEmoji { document_id, alt } => {
            tl_enums::RichText::TextCustomEmoji(tl_types::TextCustomEmoji {
                document_id: *document_id,
                alt: alt.clone(),
            })
        }
        ModelRichText::Mention(t) => tl_enums::RichText::TextMention(tl_types::TextMention {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::Hashtag(t) => tl_enums::RichText::TextHashtag(tl_types::TextHashtag {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::BotCommand(t) => {
            tl_enums::RichText::TextBotCommand(tl_types::TextBotCommand {
                text: Box::new(model_rich_text_to_tl(t, next)),
            })
        }
        ModelRichText::Cashtag(t) => tl_enums::RichText::TextCashtag(tl_types::TextCashtag {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::AutoUrl(t) => tl_enums::RichText::TextAutoUrl(tl_types::TextAutoUrl {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::AutoEmail(t) => tl_enums::RichText::TextAutoEmail(tl_types::TextAutoEmail {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::AutoPhone(t) => tl_enums::RichText::TextAutoPhone(tl_types::TextAutoPhone {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::BankCard(t) => tl_enums::RichText::TextBankCard(tl_types::TextBankCard {
            text: Box::new(model_rich_text_to_tl(t, next)),
        }),
        ModelRichText::TonAddress(t) => {
            tl_enums::RichText::TextTonAddress(tl_types::TextTonAddress {
                text: Box::new(model_rich_text_to_tl(t, next)),
            })
        }
        ModelRichText::MentionName { text, user_id } => {
            tl_enums::RichText::TextMentionName(tl_types::TextMentionName {
                text: Box::new(model_rich_text_to_tl(text, next)),
                user_id: *user_id,
            })
        }
        ModelRichText::Date { text, date, flags } => {
            tl_enums::RichText::TextDate(tl_types::TextDate {
                relative: flags.relative,
                short_time: flags.short_time,
                long_time: flags.long_time,
                short_date: flags.short_date,
                long_date: flags.long_date,
                day_of_week: flags.day_of_week,
                text: Box::new(model_rich_text_to_tl(text, next)),
                date: *date,
            })
        }
        ModelRichText::Diff { text, old_text } => {
            tl_enums::RichText::TextDiff(tl_types::TextDiff {
                text: Box::new(model_rich_text_to_tl(text, next)),
                old_text: Box::new(model_rich_text_to_tl(old_text, next)),
            })
        }
    }
}

fn model_caption_to_tl(c: &PageCaption, depth: usize) -> tl_enums::PageCaption {
    tl_enums::PageCaption::PageCaption(tl_types::PageCaption {
        text: model_rich_text_to_tl(&c.text, depth),
        credit: model_rich_text_to_tl(&c.credit, depth),
    })
}

fn model_block_to_tl(block: &PageBlock, depth: usize) -> tl_enums::PageBlock {
    if depth >= MAX_PARSED_BLOCK_DEPTH {
        return tl_enums::PageBlock::Unsupported;
    }
    let next = depth + 1;
    match block {
        PageBlock::Unsupported => tl_enums::PageBlock::Unsupported,
        PageBlock::Title(t) => tl_enums::PageBlock::Title(tl_types::PageBlockTitle {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Subtitle(t) => tl_enums::PageBlock::Subtitle(tl_types::PageBlockSubtitle {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::AuthorDate {
            author,
            published_date,
        } => tl_enums::PageBlock::AuthorDate(tl_types::PageBlockAuthorDate {
            author: model_rich_text_to_tl(author, next),
            published_date: *published_date,
        }),
        PageBlock::Header(t) => tl_enums::PageBlock::Header(tl_types::PageBlockHeader {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Subheader(t) => tl_enums::PageBlock::Subheader(tl_types::PageBlockSubheader {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Heading { level, text } => {
            let t = model_rich_text_to_tl(text, next);
            match level.clamp(&1, &6) {
                1 => tl_enums::PageBlock::Heading1(tl_types::PageBlockHeading1 { text: t }),
                2 => tl_enums::PageBlock::Heading2(tl_types::PageBlockHeading2 { text: t }),
                3 => tl_enums::PageBlock::Heading3(tl_types::PageBlockHeading3 { text: t }),
                4 => tl_enums::PageBlock::Heading4(tl_types::PageBlockHeading4 { text: t }),
                5 => tl_enums::PageBlock::Heading5(tl_types::PageBlockHeading5 { text: t }),
                _ => tl_enums::PageBlock::Heading6(tl_types::PageBlockHeading6 { text: t }),
            }
        }
        PageBlock::Paragraph(t) => tl_enums::PageBlock::Paragraph(tl_types::PageBlockParagraph {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Preformatted { text, language } => {
            tl_enums::PageBlock::Preformatted(tl_types::PageBlockPreformatted {
                text: model_rich_text_to_tl(text, next),
                language: language.clone(),
            })
        }
        PageBlock::Footer(t) => tl_enums::PageBlock::Footer(tl_types::PageBlockFooter {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Divider => tl_enums::PageBlock::Divider,
        PageBlock::Anchor { name } => {
            tl_enums::PageBlock::Anchor(tl_types::PageBlockAnchor { name: name.clone() })
        }
        PageBlock::List { items } => tl_enums::PageBlock::List(tl_types::PageBlockList {
            items: items
                .iter()
                .map(|i| {
                    let checkbox = i.task_state != ModelTaskState::None;
                    let checked = i.task_state == ModelTaskState::Checked;
                    match &i.content {
                        ListItemContent::Text(t) => {
                            tl_enums::PageListItem::Text(tl_types::PageListItemText {
                                checkbox,
                                checked,
                                text: model_rich_text_to_tl(t, next),
                            })
                        }
                        ListItemContent::Blocks(bl) => {
                            tl_enums::PageListItem::Blocks(tl_types::PageListItemBlocks {
                                checkbox,
                                checked,
                                blocks: bl.iter().map(|b| model_block_to_tl(b, next)).collect(),
                            })
                        }
                    }
                })
                .collect(),
        }),
        PageBlock::OrderedList {
            items,
            reversed,
            start,
            list_type,
        } => tl_enums::PageBlock::OrderedList(tl_types::PageBlockOrderedList {
            reversed: *reversed,
            items: items
                .iter()
                .map(|i| {
                    let checkbox = i.task_state != ModelTaskState::None;
                    let checked = i.task_state == ModelTaskState::Checked;
                    match &i.content {
                        ListItemContent::Text(t) => {
                            tl_enums::PageListOrderedItem::Text(tl_types::PageListOrderedItemText {
                                checkbox,
                                checked,
                                num: i.num.clone(),
                                text: model_rich_text_to_tl(t, next),
                                value: i.value,
                                r#type: i.list_type.clone(),
                            })
                        }
                        ListItemContent::Blocks(bl) => tl_enums::PageListOrderedItem::Blocks(
                            tl_types::PageListOrderedItemBlocks {
                                checkbox,
                                checked,
                                num: i.num.clone(),
                                blocks: bl.iter().map(|b| model_block_to_tl(b, next)).collect(),
                                value: i.value,
                                r#type: i.list_type.clone(),
                            },
                        ),
                    }
                })
                .collect(),
            start: *start,
            r#type: list_type.clone(),
        }),
        PageBlock::Blockquote {
            text,
            caption,
            collapsed,
        } => tl_enums::PageBlock::Blockquote(tl_types::PageBlockBlockquote {
            collapsed: *collapsed,
            text: model_rich_text_to_tl(text, next),
            caption: model_rich_text_to_tl(caption, next),
        }),
        PageBlock::BlockquoteBlocks { blocks, caption } => {
            tl_enums::PageBlock::BlockquoteBlocks(tl_types::PageBlockBlockquoteBlocks {
                blocks: blocks.iter().map(|b| model_block_to_tl(b, next)).collect(),
                caption: model_rich_text_to_tl(caption, next),
            })
        }
        PageBlock::Pullquote { text, caption } => {
            tl_enums::PageBlock::Pullquote(tl_types::PageBlockPullquote {
                text: model_rich_text_to_tl(text, next),
                caption: model_rich_text_to_tl(caption, next),
            })
        }
        PageBlock::Photo {
            photo_id,
            caption,
            url,
            webpage_id,
            spoiler,
        } => tl_enums::PageBlock::Photo(tl_types::PageBlockPhoto {
            spoiler: *spoiler,
            photo_id: *photo_id,
            caption: model_caption_to_tl(caption, next),
            url: url.clone(),
            webpage_id: *webpage_id,
        }),
        PageBlock::Video {
            video_id,
            caption,
            autoplay,
            loop_video,
            spoiler,
        } => tl_enums::PageBlock::Video(tl_types::PageBlockVideo {
            autoplay: *autoplay,
            r#loop: *loop_video,
            spoiler: *spoiler,
            video_id: *video_id,
            caption: model_caption_to_tl(caption, next),
        }),
        PageBlock::Cover(c) => tl_enums::PageBlock::Cover(tl_types::PageBlockCover {
            cover: Box::new(model_block_to_tl(c, next)),
        }),
        PageBlock::Embed {
            url,
            html,
            poster_photo_id,
            width,
            height,
            full_width,
            allow_scrolling,
            caption,
        } => tl_enums::PageBlock::Embed(tl_types::PageBlockEmbed {
            full_width: *full_width,
            allow_scrolling: *allow_scrolling,
            url: url.clone(),
            html: html.clone(),
            poster_photo_id: *poster_photo_id,
            w: *width,
            h: *height,
            caption: model_caption_to_tl(caption, next),
        }),
        PageBlock::EmbedPost {
            url,
            webpage_id,
            author_photo_id,
            author,
            date,
            blocks,
            caption,
        } => tl_enums::PageBlock::EmbedPost(tl_types::PageBlockEmbedPost {
            url: url.clone(),
            webpage_id: *webpage_id,
            author_photo_id: *author_photo_id,
            author: author.clone(),
            date: *date,
            blocks: blocks.iter().map(|b| model_block_to_tl(b, next)).collect(),
            caption: model_caption_to_tl(caption, next),
        }),
        PageBlock::Collage { items, caption } => {
            tl_enums::PageBlock::Collage(tl_types::PageBlockCollage {
                items: items.iter().map(|b| model_block_to_tl(b, next)).collect(),
                caption: model_caption_to_tl(caption, next),
            })
        }
        PageBlock::Slideshow { items, caption } => {
            tl_enums::PageBlock::Slideshow(tl_types::PageBlockSlideshow {
                items: items.iter().map(|b| model_block_to_tl(b, next)).collect(),
                caption: model_caption_to_tl(caption, next),
            })
        }
        PageBlock::Channel(_) => tl_enums::PageBlock::Unsupported,
        PageBlock::Audio { audio_id, caption } => {
            tl_enums::PageBlock::Audio(tl_types::PageBlockAudio {
                audio_id: *audio_id,
                caption: model_caption_to_tl(caption, next),
            })
        }
        PageBlock::Kicker(t) => tl_enums::PageBlock::Kicker(tl_types::PageBlockKicker {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Table {
            title,
            rows,
            bordered,
            striped,
            compact,
        } => tl_enums::PageBlock::Table(tl_types::PageBlockTable {
            bordered: *bordered,
            striped: *striped,
            compact: *compact,
            title: model_rich_text_to_tl(title, next),
            rows: rows
                .iter()
                .map(|r| {
                    tl_enums::PageTableRow::PageTableRow(tl_types::PageTableRow {
                        cells: r
                            .cells
                            .iter()
                            .map(|c| {
                                tl_enums::PageTableCell::PageTableCell(tl_types::PageTableCell {
                                    header: c.header,
                                    align_center: c.align == TableAlign::Center,
                                    align_right: c.align == TableAlign::Right,
                                    valign_middle: c.valign == TableValign::Middle,
                                    valign_bottom: c.valign == TableValign::Bottom,
                                    text: c.text.as_ref().map(|t| model_rich_text_to_tl(t, next)),
                                    colspan: (c.colspan > 1).then_some(c.colspan),
                                    rowspan: (c.rowspan > 1).then_some(c.rowspan),
                                })
                            })
                            .collect(),
                    })
                })
                .collect(),
        }),
        PageBlock::Details {
            title,
            blocks,
            open,
        } => tl_enums::PageBlock::Details(tl_types::PageBlockDetails {
            open: *open,
            blocks: blocks.iter().map(|b| model_block_to_tl(b, next)).collect(),
            title: model_rich_text_to_tl(title, next),
        }),
        PageBlock::RelatedArticles { title, articles } => {
            tl_enums::PageBlock::RelatedArticles(tl_types::PageBlockRelatedArticles {
                title: model_rich_text_to_tl(title, next),
                articles: articles
                    .iter()
                    .map(|a| {
                        tl_enums::PageRelatedArticle::PageRelatedArticle(
                            tl_types::PageRelatedArticle {
                                url: a.url.clone(),
                                webpage_id: a.webpage_id,
                                title: a.title.clone(),
                                description: a.description.clone(),
                                photo_id: a.photo_id,
                                author: a.author.clone(),
                                published_date: a.published_date,
                            },
                        )
                    })
                    .collect(),
            })
        }
        PageBlock::Map {
            geo,
            zoom,
            width,
            height,
            caption,
        } => tl_enums::PageBlock::Map(tl_types::PageBlockMap {
            geo: tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                long: geo.lon,
                lat: geo.lat,
                access_hash: geo.access_hash,
                accuracy_radius: None,
            }),
            zoom: *zoom,
            w: *width,
            h: *height,
            caption: model_caption_to_tl(caption, next),
        }),
        PageBlock::Math { source } => tl_enums::PageBlock::Math(tl_types::PageBlockMath {
            source: source.clone(),
        }),
        PageBlock::Thinking(t) => tl_enums::PageBlock::Thinking(tl_types::PageBlockThinking {
            text: model_rich_text_to_tl(t, next),
        }),
        PageBlock::Document {
            document_id,
            caption,
        } => tl_enums::PageBlock::Document(tl_types::PageBlockDocument {
            document_id: *document_id,
            caption: model_caption_to_tl(caption, next),
        }),
    }
}
