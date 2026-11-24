//! Core text entity types and containers.
//!
//! Port of `Telegram/lib_ui/ui/text/text_entity.h` and core methods in
//! `Telegram/lib_ui/ui/text/text_entity.cpp` and `Telegram/lib_ui/ui/text/text.h`.

use std::cmp::Ordering;

use bitflags::bitflags;

use crate::url::{encode_for_opening, external_url_from_internal_url};
use crate::utf16::{slice_utf16, utf16_len};

/// All Telegram text entity types (`EntityType` in `text_entity.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum EntityType {
    /// Uninitialized / invalid entity.
    #[default]
    Invalid = 0,

    /// Auto-detected URL.
    Url,
    /// Hyperlink with custom target URL in `EntityInText::data`.
    CustomUrl,
    /// Email address.
    Email,
    /// Hashtag (`#tag` or `#tag@username`).
    Hashtag,
    /// Cashtag (`$USD`).
    Cashtag,
    /// `@username` mention.
    Mention,
    /// Text mention referencing a user ID (`"userId.accessHash:selfId"` in `data`).
    MentionName,
    /// Custom emoji document (`"<document_id>"` in `data`).
    CustomEmoji,
    /// Bot command (`/start` or `/start@bot`).
    BotCommand,
    /// Media playback timestamp (`01:23`).
    MediaTimestamp,
    /// UI-only colorized span (color index / background index in `data`).
    Colorized,
    /// Phone number.
    Phone,
    /// Bank card number.
    BankCard,
    /// TON wallet address.
    TonAddress,

    /// Bold formatting.
    Bold,
    /// Semibold formatting (UI-only, not sent to Telegram API).
    Semibold,
    /// Italic formatting.
    Italic,
    /// Underline formatting.
    Underline,
    /// Strikethrough formatting.
    StrikeOut,
    /// Inline monospace code.
    Code,
    /// Preformatted code block (optional language name in `data`).
    Pre,
    /// Blockquote (`data == "1"` / non-empty indicates collapsed blockquote).
    Blockquote,
    /// Spoiler formatting.
    Spoiler,
    /// Subscript (Instant View editor).
    Subscript,
    /// Superscript (Instant View editor).
    Superscript,
    /// Highlighted / marked text (Instant View editor).
    Marked,
    /// Formatted date entity (`"<unix_ts>:<flags>"` in `data`).
    FormattedDate,
}

bitflags! {
    /// Formatting flags for `EntityType::FormattedDate` (`FormattedDateFlags`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
    pub struct FormattedDateFlags: u8 {
        /// Display relative time (e.g. "in 2 hours").
        const RELATIVE = 0x01;
        /// Short time format.
        const SHORT_TIME = 0x02;
        /// Long time format.
        const LONG_TIME = 0x04;
        /// Short date format.
        const SHORT_DATE = 0x08;
        /// Long date format.
        const LONG_DATE = 0x10;
        /// Include day of week.
        const DAY_OF_WEEK = 0x20;
    }
}

/// Serializes `FormattedDate` entity data as `"<date>:<flags>"`
/// (`SerializeFormattedDateData`).
pub fn serialize_formatted_date_data(date: i32, flags: FormattedDateFlags) -> String {
    format!("{date}:{}", flags.bits())
}

/// Deserializes `FormattedDate` entity data from `"<date>:<flags>"`
/// (`DeserializeFormattedDateData`).
pub fn deserialize_formatted_date_data(data: &str) -> (i32, FormattedDateFlags) {
    let mut parts = data.split(':');
    let (Some(date_s), Some(flags_s), None) = (parts.next(), parts.next(), parts.next()) else {
        return (0, FormattedDateFlags::empty());
    };
    let date = date_s.parse::<i32>().unwrap_or(0);
    let raw_flags = flags_s.parse::<u32>().unwrap_or(0) as u8;
    (date, FormattedDateFlags::from_bits_retain(raw_flags))
}

/// Whether a link displays its full URL or custom text (`EntityLinkShown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum EntityLinkShown {
    /// Full link text is shown.
    #[default]
    Full,
    /// Link target differs from shown text.
    Partial,
}

/// Link presentation metadata (`EntityLinkData`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct EntityLinkData {
    /// Visible link text.
    pub text: String,
    /// Link target or payload.
    pub data: String,
    /// Entity type.
    pub entity_type: EntityType,
    /// Whether the link URL is shown in full or partially.
    pub shown: EntityLinkShown,
}

/// A single styled/semantic entity within a text string (`EntityInText`).
///
/// Both `offset` and `length` are measured in **UTF-16 code units**, matching
/// Qt `QString` and the Telegram MTProto API.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityInText {
    entity_type: EntityType,
    offset: i32,
    length: i32,
    data: String,
}

impl EntityInText {
    /// Creates a new `EntityInText` with empty `data`.
    #[inline]
    pub fn new(entity_type: EntityType, offset: i32, length: i32) -> Self {
        Self {
            entity_type,
            offset,
            length,
            data: String::new(),
        }
    }

    /// Creates a new `EntityInText` with explicit `data`.
    #[inline]
    pub fn with_data(
        entity_type: EntityType,
        offset: i32,
        length: i32,
        data: impl Into<String>,
    ) -> Self {
        Self {
            entity_type,
            offset,
            length,
            data: data.into(),
        }
    }

    /// Returns the entity type.
    #[inline]
    pub fn entity_type(&self) -> EntityType {
        self.entity_type
    }

    /// Returns the UTF-16 code unit offset.
    #[inline]
    pub fn offset(&self) -> i32 {
        self.offset
    }

    /// Returns the UTF-16 code unit length.
    #[inline]
    pub fn length(&self) -> i32 {
        self.length
    }

    /// Returns the entity payload (`data`), e.g. URL, code language, custom emoji ID, or `"1"` for collapsed blockquote.
    #[inline]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Returns `true` if `entity_type != EntityType::Invalid`.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.entity_type != EntityType::Invalid
    }

    /// Returns `true` if this is a collapsed `Blockquote` entity (`!data.is_empty()`).
    #[inline]
    pub fn is_collapsed_blockquote(&self) -> bool {
        self.entity_type == EntityType::Blockquote && !self.data.is_empty()
    }

    /// Returns the programming language name if this is a `Pre` entity with a non-empty language.
    #[inline]
    pub fn pre_language(&self) -> Option<&str> {
        if self.entity_type == EntityType::Pre && !self.data.is_empty() {
            Some(&self.data)
        } else {
            None
        }
    }

    /// Checks whether `offset` and `length` lie within `0..=text_utf16_len` (`EntityInText::validForText`).
    #[inline]
    pub fn valid_for_text(&self, text_utf16_len: usize) -> bool {
        let Ok(text_len) = i32::try_from(text_utf16_len) else {
            return false;
        };
        self.offset >= 0
            && self.length > 0
            && self.offset <= text_len
            && self.length <= text_len - self.offset
    }

    /// Extends the entity to the left by `extent` UTF-16 code units (`extendToLeft`).
    #[inline]
    pub fn extend_to_left(&mut self, extent: i32) {
        self.offset -= extent;
        self.length += extent;
    }

    /// Shrinks the entity from the right by `shrink` UTF-16 code units (`shrinkFromRight`).
    #[inline]
    pub fn shrink_from_right(&mut self, shrink: i32) {
        self.length -= shrink;
    }

    /// Shifts the entity left by `shift` UTF-16 code units, clamping at 0 (`shiftLeft`).
    #[inline]
    pub fn shift_left(&mut self, shift: i32) {
        self.offset -= shift;
        if self.offset < 0 {
            self.length += self.offset;
            self.offset = 0;
            if self.length < 0 {
                self.length = 0;
            }
        }
    }

    /// Shifts the entity right by `shift` UTF-16 code units (`shiftRight`).
    #[inline]
    pub fn shift_right(&mut self, shift: i32) {
        self.offset += shift;
    }

    /// Clamps the entity's end to `text_end` UTF-16 code units (`updateTextEnd`).
    #[inline]
    pub fn update_text_end(&mut self, text_end: i32) {
        if self.offset > text_end {
            self.offset = text_end;
            self.length = 0;
        } else if self.offset + self.length > text_end {
            self.length = text_end - self.offset;
        }
    }

    /// Returns the smallest UTF-16 offset among valid `Pre` / `Code` entities,
    /// or `text_utf16_len` if none exist (`EntityInText::FirstMonospaceOffset`).
    pub fn first_monospace_offset(entities: &[EntityInText], text_utf16_len: usize) -> usize {
        entities
            .iter()
            .filter(|e| {
                e.valid_for_text(text_utf16_len)
                    && matches!(e.entity_type, EntityType::Pre | EntityType::Code)
            })
            .map(|e| e.offset as usize)
            .min()
            .unwrap_or(text_utf16_len)
    }
}

/// Ordered list of entities in a text (`EntitiesInText`).
pub type EntitiesInText = Vec<EntityInText>;

/// Returns `true` if `entity` replaces its underlying text span visually
/// (`EntityReplacesText`).
pub fn entity_replaces_text(entity: &EntityInText) -> bool {
    match entity.entity_type() {
        EntityType::CustomEmoji => true,
        EntityType::FormattedDate => {
            deserialize_formatted_date_data(entity.data()).1 != FormattedDateFlags::empty()
        }
        _ => false,
    }
}

/// Tracks half-open `[from, till)` ranges of text replaced by visual entities
/// (`ReplacedTextRanges`).
#[derive(Debug, Clone, Default)]
pub struct ReplacedTextRanges {
    ranges: Vec<(i32, i32)>,
}

impl ReplacedTextRanges {
    /// Creates an empty range tracker.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a half-open range `[from, till)` if `from < till`.
    #[inline]
    pub fn add(&mut self, from: i32, till: i32) {
        if from < till {
            self.ranges.push((from, till));
        }
    }

    /// Sorts and computes running prefix maximums so `overlaps` runs in O(log n).
    pub fn prepare(&mut self) {
        self.ranges.sort_by_key(|&(from, _)| from);
        let mut max_till = 0;
        for &mut (_, ref mut till) in &mut self.ranges {
            if *till > max_till {
                max_till = *till;
            } else {
                *till = max_till;
            }
        }
    }

    /// Returns `true` if `[from, till)` overlaps any added range (`prepare` must be called first).
    pub fn overlaps(&self, from: i32, till: i32) -> bool {
        if from >= till {
            return false;
        }
        let idx = self.ranges.partition_point(|&(r_from, _)| r_from < till);
        idx > 0 && self.ranges[idx - 1].1 > from
    }
}

/// Plain text paired with UTF-16-indexed formatting/semantic entities (`TextWithEntities`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TextWithEntities {
    /// UTF-8 text content.
    pub text: String,
    /// Entities indexed in UTF-16 code units into `text`.
    pub entities: EntitiesInText,
}

impl TextWithEntities {
    /// Creates an empty `TextWithEntities`.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a `TextWithEntities` with plain text and no entities (`TextWithEntities::Simple`).
    #[inline]
    pub fn simple(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            entities: Vec::new(),
        }
    }

    /// Creates a `TextWithEntities` from text and entities.
    #[inline]
    pub fn with_entities(text: impl Into<String>, entities: EntitiesInText) -> Self {
        Self {
            text: text.into(),
            entities,
        }
    }

    /// Returns `true` if `text` is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns the UTF-16 code unit length of `self.text`.
    #[inline]
    pub fn utf16_len(&self) -> usize {
        utf16_len(&self.text)
    }

    /// Reserves capacity for text bytes and entities.
    #[inline]
    pub fn reserve(&mut self, text_bytes: usize, entities_count: usize) {
        self.text.reserve(text_bytes);
        self.entities.reserve(entities_count);
    }

    /// Appends another `TextWithEntities`, shifting its entities by `self.utf16_len()`.
    pub fn append(&mut self, mut other: TextWithEntities) -> &mut Self {
        let shift = self.utf16_len() as i32;
        for entity in &mut other.entities {
            entity.shift_right(shift);
        }
        self.text.push_str(&other.text);
        self.entities.extend(other.entities);
        self
    }

    /// Appends a borrowed `TextWithEntities`, shifting cloned entities by `self.utf16_len()`.
    pub fn append_ref(&mut self, other: &TextWithEntities) -> &mut Self {
        let shift = self.utf16_len() as i32;
        self.text.push_str(&other.text);
        self.entities.reserve(other.entities.len());
        for entity in &other.entities {
            let mut copy = entity.clone();
            copy.shift_right(shift);
            self.entities.push(copy);
        }
        self
    }

    /// Appends plain text without entities.
    #[inline]
    pub fn append_str(&mut self, other: &str) -> &mut Self {
        self.text.push_str(other);
        self
    }

    /// Appends a single character without entities.
    #[inline]
    pub fn append_char(&mut self, ch: char) -> &mut Self {
        self.text.push(ch);
        self
    }

    /// Extracts the UTF-8 text slice covered by `entity`, or `None` if out of bounds / mid-surrogate.
    pub fn slice_entity(&self, entity: &EntityInText) -> Option<&str> {
        if entity.offset() < 0 || entity.length() < 0 {
            return None;
        }
        slice_utf16(
            &self.text,
            entity.offset() as usize,
            entity.length() as usize,
        )
    }

    /// Sorts entities in canonical Telegram order: ascending `offset`, then descending `length`
    /// (outer/longer enclosing entities before inner/shorter ones), preserving relative order on ties.
    pub fn sort_entities(&mut self) {
        sort_entities(&mut self.entities);
    }
}

/// Entity priority for tie-breaking when two entities share the exact same `(offset, length)`.
/// Block entities wrap links, which wrap inline formatting, which wraps custom emoji.
fn entity_nesting_rank(t: EntityType) -> u8 {
    match t {
        EntityType::Blockquote | EntityType::Pre => 0,
        EntityType::Url
        | EntityType::CustomUrl
        | EntityType::Email
        | EntityType::Mention
        | EntityType::MentionName
        | EntityType::Hashtag
        | EntityType::Cashtag
        | EntityType::BotCommand
        | EntityType::Phone
        | EntityType::BankCard
        | EntityType::TonAddress
        | EntityType::MediaTimestamp => 1,
        EntityType::Bold
        | EntityType::Semibold
        | EntityType::Italic
        | EntityType::Underline
        | EntityType::StrikeOut
        | EntityType::Spoiler
        | EntityType::Code
        | EntityType::Colorized
        | EntityType::Subscript
        | EntityType::Superscript
        | EntityType::Marked => 2,
        EntityType::CustomEmoji | EntityType::FormattedDate => 3,
        EntityType::Invalid => 4,
    }
}

/// Sorts entities in canonical Telegram nesting order:
/// 1. Ascending `offset`
/// 2. Descending `length` (enclosing entities first)
/// 3. Nesting rank (block -> link -> inline -> custom emoji)
pub fn sort_entities(entities: &mut [EntityInText]) {
    entities.sort_by(|a, b| {
        a.offset()
            .cmp(&b.offset())
            .then_with(|| b.length().cmp(&a.length()))
            .then_with(|| {
                entity_nesting_rank(a.entity_type()).cmp(&entity_nesting_rank(b.entity_type()))
            })
    });
}

/// Merges adjacent/overlapping identical-type entities with identical `data` when appropriate,
/// drops zero-length or invalid entities, and sorts them canonically.
///
/// Non-mergeable entities (`CustomEmoji`, `MentionName`, `FormattedDate`, `Blockquote`, `Pre`)
/// are never coalesced.
pub fn merge_and_sort_entities(entities: EntitiesInText) -> EntitiesInText {
    let mut filtered: EntitiesInText = entities
        .into_iter()
        .filter(|e| e.is_valid() && e.offset() >= 0 && e.length() > 0)
        .collect();
    sort_entities(&mut filtered);

    let mergeable = |t: EntityType| {
        matches!(
            t,
            EntityType::Bold
                | EntityType::Semibold
                | EntityType::Italic
                | EntityType::Underline
                | EntityType::StrikeOut
                | EntityType::Spoiler
                | EntityType::Code
                | EntityType::Subscript
                | EntityType::Superscript
                | EntityType::Marked
        )
    };

    let mut result: EntitiesInText = Vec::with_capacity(filtered.len());
    for entity in filtered {
        if mergeable(entity.entity_type())
            && let Some(prev) = result.iter_mut().rev().find(|p| {
                p.entity_type() == entity.entity_type()
                    && p.data() == entity.data()
                    && p.offset() + p.length() >= entity.offset()
            })
        {
            let new_end = (prev.offset() + prev.length()).max(entity.offset() + entity.length());
            prev.length = new_end - prev.offset();
            continue;
        }
        result.push(entity);
    }
    sort_entities(&mut result);
    result
}

/// Compares two entities by span order `(offset asc, length desc)`.
#[inline]
pub fn cmp_entity_span(a: &EntityInText, b: &EntityInText) -> Ordering {
    a.offset()
        .cmp(&b.offset())
        .then_with(|| b.length().cmp(&a.length()))
}

/// Extra MIME tag entry carried alongside `TextForMimeData` (`TextForMimeDataTag`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TextForMimeDataTag {
    /// Start offset in UTF-16 code units.
    pub offset: i32,
    /// Length in UTF-16 code units.
    pub length: i32,
    /// Tag identifier.
    pub id: String,
}

/// Clipboard payload holding both link-expanded plain text and rich entities (`TextForMimeData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextForMimeData {
    /// Plain text with custom links expanded as `"text (url)"`.
    pub expanded: String,
    /// Rich text with entities.
    pub rich: TextWithEntities,
    /// Additional MIME-only tags.
    pub tags: Vec<TextForMimeDataTag>,
}

impl TextForMimeData {
    /// Creates an empty `TextForMimeData`.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if `expanded` is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.expanded.is_empty()
    }

    /// Wraps plain text with no entities (`TextForMimeData::Simple`).
    pub fn simple(simple: impl Into<String>) -> Self {
        let s = simple.into();
        Self {
            expanded: s.clone(),
            rich: TextWithEntities::simple(s),
            tags: Vec::new(),
        }
    }

    /// Wraps rich text without expanding links (`TextForMimeData::Rich`).
    pub fn rich(rich: TextWithEntities) -> Self {
        let expanded = rich.text.clone();
        Self {
            expanded,
            rich,
            tags: Vec::new(),
        }
    }

    /// Expands `EntityType::CustomUrl` entities into `"text (url)"` in `expanded`
    /// (`TextForMimeData::WithExpandedLinks`).
    pub fn with_expanded_links(text: &TextWithEntities) -> Self {
        let mut result = Self {
            expanded: String::new(),
            rich: text.clone(),
            tags: Vec::new(),
        };
        if !text
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::CustomUrl)
        {
            result.expanded = text.text.clone();
            return result;
        }
        let text_u16_len = text.utf16_len();
        let mut from_u16 = 0usize;
        for entity in &text.entities {
            if entity.entity_type() != EntityType::CustomUrl || !entity.valid_for_text(text_u16_len)
            {
                continue;
            }
            let external = external_url_from_internal_url(entity.data());
            let data = if external.is_empty() {
                entity.data()
            } else {
                external.as_str()
            };
            let entity_slice = slice_utf16(
                &text.text,
                entity.offset() as usize,
                entity.length() as usize,
            )
            .unwrap_or("");
            if !data.starts_with("internal:") && data != encode_for_opening(entity_slice) {
                let till_u16 = (entity.offset() + entity.length()) as usize;
                if till_u16 > from_u16 {
                    if let Some(chunk) = slice_utf16(&text.text, from_u16, till_u16 - from_u16) {
                        result.expanded.push_str(chunk);
                    }
                    from_u16 = till_u16;
                }
                result.expanded.push_str(" (");
                result.expanded.push_str(data);
                result.expanded.push(')');
            }
        }
        if text_u16_len > from_u16
            && let Some(tail) = slice_utf16(&text.text, from_u16, text_u16_len - from_u16)
        {
            result.expanded.push_str(tail);
        }
        result
    }

    /// Appends another `TextForMimeData`, shifting entities and tags.
    pub fn append(&mut self, mut other: TextForMimeData) -> &mut Self {
        let shift = self.rich.utf16_len() as i32;
        self.expanded.push_str(&other.expanded);
        self.rich.append(other.rich);
        self.tags.reserve(other.tags.len());
        for tag in &mut other.tags {
            tag.offset += shift;
        }
        self.tags.extend(other.tags);
        self
    }

    /// Appends a `TextWithEntities` to both `expanded` and `rich`.
    pub fn append_rich(&mut self, other: TextWithEntities) -> &mut Self {
        self.expanded.push_str(&other.text);
        self.rich.append(other);
        self
    }

    /// Appends plain text to both `expanded` and `rich`.
    pub fn append_str(&mut self, other: &str) -> &mut Self {
        self.expanded.push_str(other);
        self.rich.append_str(other);
        self
    }
}

bitflags! {
    /// Flags controlling entity & markdown parsing (`TextParse*` enum in `text_entity.h`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct TextParseFlags: u32 {
        /// Allow multiline text.
        const MULTILINE = 0x001;
        /// Enable link parsing / keeping link entities.
        const LINKS = 0x002;
        /// Parse `@username` mentions.
        const MENTIONS = 0x004;
        /// Parse `#hashtag` entities.
        const HASHTAGS = 0x008;
        /// Parse `/bot_command` entities.
        const BOT_COMMANDS = 0x010;
        /// Parse Markdown formatting (`**bold**`, `__italic__`, `~~strike~~`, `` `code` ``, ```` ```pre``` ````, `||spoiler||`, `[text](url)`).
        const MARKDOWN = 0x020;
        /// Keep UI colorized entities.
        const COLORIZED = 0x040;
        /// Keep leading spaces.
        const KEEP_LEADING_SPACES = 0x080;
        /// Optional extension: parse `$CASHTAG` entities locally (e.g. `$USD`, `$AAPL@bot`).
        const CASHTAGS = 0x100;
    }
}

/// Decoded payload of `EntityType::MentionName` (`TextUtilities::MentionNameFields`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MentionNameFields {
    /// Logged-in user's own bare user ID.
    pub self_id: u64,
    /// Mentioned user's bare user ID.
    pub user_id: u64,
    /// Mentioned user's access hash.
    pub access_hash: u64,
}

/// Parses `"<userId>.<accessHash>:<selfId>"` into `MentionNameFields`
/// (`TextUtilities::MentionNameDataToFields`).
pub fn mention_name_data_to_fields(data: &str) -> MentionNameFields {
    let mut comps = data.split('.');
    let (Some(user_part), Some(rest), None) = (comps.next(), comps.next(), comps.next()) else {
        return MentionNameFields::default();
    };
    let mut parts = rest.split(':');
    let (Some(hash_part), Some(self_part), None) = (parts.next(), parts.next(), parts.next())
    else {
        return MentionNameFields::default();
    };
    MentionNameFields {
        self_id: self_part.parse::<u64>().unwrap_or(0),
        user_id: user_part.parse::<u64>().unwrap_or(0),
        access_hash: hash_part.parse::<u64>().unwrap_or(0),
    }
}

/// Formats `MentionNameFields` as `"<userId>.<accessHash>:<selfId>"`
/// (`TextUtilities::MentionNameDataFromFields`).
pub fn mention_name_data_from_fields(fields: MentionNameFields) -> String {
    format!(
        "{}.{}:{}",
        fields.user_id, fields.access_hash, fields.self_id
    )
}

/// Selection range in UTF-16 code units (`TextSelection` in `text.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextSelection {
    /// Start UTF-16 offset.
    pub from: u16,
    /// End UTF-16 offset.
    pub to: u16,
}

/// Selection covering the entire text (`AllTextSelection`).
pub const ALL_TEXT_SELECTION: TextSelection = TextSelection {
    from: 0,
    to: 0xFFFF,
};

impl TextSelection {
    /// Creates a new `TextSelection`.
    #[inline]
    pub const fn new(from: u16, to: u16) -> Self {
        Self { from, to }
    }

    /// Returns `true` if `from == to`.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.from == self.to
    }

    /// Clamps `from` and `to` to `0..=0xFFFF` (`snapSelection`).
    #[inline]
    pub fn snap(from: i32, to: i32) -> Self {
        Self {
            from: from.clamp(0, 0xFFFF) as u16,
            to: to.clamp(0, 0xFFFF) as u16,
        }
    }

    /// Shifts selection right by `by_length` (`shiftSelection`).
    #[inline]
    pub fn shift(self, by_length: u16) -> Self {
        Self::snap(
            i32::from(self.from) + i32::from(by_length),
            i32::from(self.to) + i32::from(by_length),
        )
    }

    /// Shifts selection left by `by_length` (`unshiftSelection`).
    #[inline]
    pub fn unshift(self, by_length: u16) -> Self {
        Self::snap(
            i32::from(self.from) - i32::from(by_length),
            i32::from(self.to) - i32::from(by_length),
        )
    }
}
