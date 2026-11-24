//! Port of `lib_ui/ui/text/text_entity.h` (model types).
//!
//! Stand-in / domain text-with-entities types designed to be directly compatible
//! with `tg-text` and Telegram API `MessageEntity` (UTF-16 code unit offset and
//! length + entity kind).

use bitflags::bitflags;

use crate::id::{DocumentId, TimeId, UserId};

bitflags! {
    /// Formatting flags for `EntityKind::FormattedDate` (`FormattedDateFlag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FormattedDateFlags: u8 {
        /// Render as relative time (e.g. "in 5 minutes").
        const RELATIVE    = 0x01;
        /// Render short time (e.g. "14:30").
        const SHORT_TIME  = 0x02;
        /// Render long time (e.g. "14:30:00").
        const LONG_TIME   = 0x04;
        /// Render short date (e.g. "22.11.2026").
        const SHORT_DATE  = 0x08;
        /// Render long date (e.g. "November 22, 2026").
        const LONG_DATE   = 0x10;
        /// Include day of week.
        const DAY_OF_WEEK = 0x20;
    }
}

/// Kind and associated payload of a text entity (all Telegram layer 233
/// `MessageEntity` constructors plus local tdesktop UI entities).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum EntityKind {
    /// Unknown / unsupported entity from a newer API layer.
    Unknown,
    /// `@username` mention.
    Mention,
    /// `#hashtag`.
    Hashtag,
    /// `/command` or `/command@bot`.
    BotCommand,
    /// Plain URL.
    Url,
    /// Email address.
    Email,
    /// Bold text.
    Bold,
    /// Semibold text (local UI formatting).
    Semibold,
    /// Italic text.
    Italic,
    /// Inline monospace code.
    Code,
    /// Preformatted code block with optional syntax highlighting language.
    Pre {
        /// Optional language name (may be empty).
        language: String,
    },
    /// Hyperlink with custom visible text.
    TextUrl {
        /// Target URL.
        url: String,
    },
    /// User mention by ID (`messageEntityMentionName`).
    MentionName {
        /// Mentioned user ID.
        user_id: UserId,
    },
    /// Phone number.
    Phone,
    /// `$CASHTAG`.
    Cashtag,
    /// Underlined text.
    Underline,
    /// Strikethrough text.
    StrikeOut,
    /// Bank card number.
    BankCard,
    /// Hidden spoiler text.
    Spoiler,
    /// Custom animated/static emoji backed by a document.
    CustomEmoji {
        /// Custom emoji document ID.
        document_id: DocumentId,
        /// Whether animation is forced static (e.g., folder titles).
        force_static: bool,
    },
    /// Blockquote, optionally collapsed.
    Blockquote {
        /// Whether the quote is collapsed by default.
        collapsed: bool,
    },
    /// Formatted date/time entity (`messageEntityFormattedDate`).
    FormattedDate {
        /// Unix timestamp.
        date: TimeId,
        /// Formatting flags.
        flags: FormattedDateFlags,
    },
    /// Text diff insertion (`messageEntityDiffInsert`).
    DiffInsert,
    /// Text diff replacement (`messageEntityDiffReplace`).
    DiffReplace {
        /// Previous text before replacement.
        old_text: String,
    },
    /// Text diff deletion (`messageEntityDiffDelete`).
    DiffDelete,
    /// TON wallet address (`messageEntityTonAddress`).
    TonAddress,
    /// Local media timestamp link (seconds into audio/video).
    MediaTimestamp {
        /// Timestamp in seconds.
        seconds: i32,
    },
    /// Local colorized span.
    Colorized,
    /// Subscript span.
    Subscript,
    /// Superscript span.
    Superscript,
    /// Marked/highlighted span.
    Marked,
}

/// A single formatting or semantic entity over a `TextWithEntities` string.
///
/// `offset` and `length` are measured in UTF-16 code units, matching the
/// Telegram Bot/MTProto API and Qt `QString` indexing used by tdesktop.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TextEntity {
    /// Start offset in UTF-16 code units.
    pub offset: i32,
    /// Length in UTF-16 code units.
    pub length: i32,
    /// Entity kind and optional payload.
    pub kind: EntityKind,
}

impl TextEntity {
    /// Creates a new `TextEntity`.
    #[inline]
    pub const fn new(offset: i32, length: i32, kind: EntityKind) -> Self {
        Self {
            offset,
            length,
            kind,
        }
    }

    /// Checks whether this entity lies strictly inside `[0, text_utf16_len]`
    /// with positive length (`EntityInText::validForText`).
    #[inline]
    pub const fn valid_for_text(&self, text_utf16_len: i32) -> bool {
        self.offset >= 0
            && self.length > 0
            && self.offset <= text_utf16_len
            && self.length <= text_utf16_len - self.offset
    }

    /// Extracts the substring covered by this entity from `text`, interpreting
    /// `offset` and `length` as UTF-16 code units.
    pub fn slice_text(&self, text: &str) -> Option<String> {
        if self.offset < 0 || self.length <= 0 {
            return None;
        }
        let start = self.offset as usize;
        let len = self.length as usize;
        let end = start.checked_add(len)?;
        let units: Vec<u16> = text.encode_utf16().collect();
        if end > units.len() {
            return None;
        }
        String::from_utf16(&units[start..end]).ok()
    }
}

/// Plain text paired with its UTF-16-indexed formatting entities (`TextWithEntities`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TextWithEntities {
    /// UTF-8 message/caption/title text.
    pub text: String,
    /// Entities sorted by offset.
    pub entities: Vec<TextEntity>,
}

impl TextWithEntities {
    /// Creates plain text with no entities.
    #[inline]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            entities: Vec::new(),
        }
    }

    /// Creates text with explicit entities.
    #[inline]
    pub fn new(text: impl Into<String>, entities: Vec<TextEntity>) -> Self {
        Self {
            text: text.into(),
            entities,
        }
    }

    /// Returns `true` if the underlying text is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns the length of `self.text` in UTF-16 code units.
    #[inline]
    pub fn utf16_len(&self) -> i32 {
        self.text.encode_utf16().count() as i32
    }

    /// Marks all `CustomEmoji` entities as `force_static = true`
    /// (`Data::ForceCustomEmojiStatic`).
    pub fn force_custom_emoji_static(mut self) -> Self {
        for entity in &mut self.entities {
            if let EntityKind::CustomEmoji { force_static, .. } = &mut entity.kind {
                *force_static = true;
            }
        }
        self
    }
}
