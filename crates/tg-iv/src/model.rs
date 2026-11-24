//! Port of `iv/iv_data.h`, `iv/iv_data.cpp`, `iv/iv_rich_page.h`, and `iv/iv_rich_page.cpp`.
//!
//! Defines the Rust domain model for Telegram Instant View (`Page`, `PageBlock`, `RichText`,
//! captions, tables, lists, related articles, maps, media metadata) matching Telegram API
//! layer 233 (`schema/api.tl`). `tg-api` converts TL `Page` objects into these structs.

use std::collections::HashMap;

/// Maximum nesting depth allowed when rendering blocks or rich text trees.
pub const MAX_NESTING_DEPTH: usize = 64;

/// Maximum number of items placed in a single collage grid (`RichPage::kCollageMaxItems`).
pub const COLLAGE_MAX_ITEMS: usize = 10;

/// Default width used for map blocks when API width is non-positive.
pub const DEFAULT_MAP_WIDTH: i32 = 400;

/// Default height used for map blocks when API height is non-positive.
pub const DEFAULT_MAP_HEIGHT: i32 = 200;

/// A Telegram Instant View page (`page#98657f0d`) with webpage metadata.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    /// Webpage identifier (`webPage.id`).
    pub id: u64,
    /// Webpage hash (`webPage.hash`) used for `messages.getWebPage` revalidation.
    pub hash: i32,
    /// Canonical page URL (`page.url` / `webPage.url`).
    pub url: String,
    /// Optional explicit site name (`webPage.site_name`). Falls back to [`site_name_from_url`].
    pub site_name: Option<String>,
    /// True if only a partial preview of the page was returned (`page.part`).
    pub part: bool,
    /// True if the page is right-to-left (`page.rtl`).
    pub rtl: bool,
    /// True if the page uses Instant View 2.0 semantics (`page.v2`).
    pub v2: bool,
    /// Optional view counter (`page.views`).
    pub views: Option<i32>,
    /// Top-level sequence of page blocks.
    pub blocks: Vec<PageBlock>,
    /// Metadata for photos referenced by `photo_id` inside blocks.
    pub photos: Vec<PhotoMeta>,
    /// Metadata for documents (videos, audio, inline images, files) referenced by `document_id`.
    pub documents: Vec<DocumentMeta>,
}

impl Page {
    /// Returns the display site name, using `site_name` if present or deriving it from `url`.
    pub fn display_site_name(&self) -> String {
        match &self.site_name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => site_name_from_url(&self.url),
        }
    }

    /// Builds a lookup map of photo metadata by `PhotoMeta::id`.
    pub fn photo_map(&self) -> HashMap<i64, &PhotoMeta> {
        self.photos.iter().map(|p| (p.id, p)).collect()
    }

    /// Builds a lookup map of document metadata by `DocumentMeta::id`.
    pub fn document_map(&self) -> HashMap<i64, &DocumentMeta> {
        self.documents.iter().map(|d| (d.id, d)).collect()
    }
}

/// High-level page handle ported from `Iv::Data` (`iv/iv_data.h`, `iv/iv_data.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct PageData {
    /// Underlying Instant View page.
    pub page: Page,
    /// Cached display site name.
    name: String,
}

impl PageData {
    /// Creates a new `PageData` from a [`Page`].
    pub fn new(page: Page) -> Self {
        let name = page.display_site_name();
        Self { page, name }
    }

    /// Page identifier string (matches `Iv::Data::id()`, returning the canonical URL).
    pub fn id(&self) -> &str {
        &self.page.url
    }

    /// Site display name (matches `Iv::Data::name()`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Numeric webpage ID (matches `Iv::Data::pageId()`).
    pub fn page_id(&self) -> u64 {
        self.page.id
    }

    /// Webpage hash (matches `Iv::Data::hash()`).
    pub fn hash(&self) -> i32 {
        self.page.hash
    }

    /// Whether the page is partial (matches `Iv::Data::partial()`).
    pub fn partial(&self) -> bool {
        self.page.part
    }
}

/// Extracts a human-readable site name from a URL, matching `Iv::SiteNameFromUrl` (`iv_data.cpp`).
///
/// Strips any leading `scheme://` and path, takes the last two dot-separated host labels,
/// and capitalizes the first character of the domain label (e.g. `"https://www.example.org/a"` -> `"Example.org"`).
pub fn site_name_from_url(url: &str) -> String {
    let mut pretty = url.trim();
    if let Some(pos) = pretty.find("://") {
        let scheme = &pretty[..pos];
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric()) {
            pretty = &pretty[pos + 3..];
        }
    }
    if let Some(slash) = pretty.find('/')
        && slash > 0
    {
        pretty = &pretty[..slash];
    }
    if let Some(colon) = pretty.find(':') {
        pretty = &pretty[..colon];
    }
    let components: Vec<&str> = pretty.split('.').filter(|s| !s.is_empty()).collect();
    if components.len() >= 2 {
        let domain = components[components.len() - 2];
        let tld = components[components.len() - 1];
        let mut chars = domain.chars();
        if let Some(first) = chars.next() {
            let mut out = String::new();
            for ch in first.to_uppercase() {
                out.push(ch);
            }
            out.push_str(chars.as_str());
            out.push('.');
            out.push_str(tld);
            return out;
        }
    }
    String::new()
}

/// Lightweight metadata for a photo included in `Page::photos`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PhotoMeta {
    /// Telegram photo ID.
    pub id: i64,
    /// Intrinsic width in pixels (0 if unknown).
    pub width: i32,
    /// Intrinsic height in pixels (0 if unknown).
    pub height: i32,
    /// Optional stripped thumbnail JPEG bytes.
    pub stripped_thumb: Option<Vec<u8>>,
}

/// Lightweight metadata for a document included in `Page::documents`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentMeta {
    /// Telegram document ID.
    pub id: i64,
    /// MIME type (e.g. `"video/mp4"`, `"audio/mpeg"`, `"image/png"`).
    pub mime_type: String,
    /// Original file name, if present.
    pub file_name: String,
    /// Intrinsic width in pixels (for videos/images).
    pub width: i32,
    /// Intrinsic height in pixels (for videos/images).
    pub height: i32,
    /// Duration in seconds (for audio/video).
    pub duration: i32,
    /// Audio track title.
    pub title: String,
    /// Audio track performer.
    pub performer: String,
    /// Whether the document is a video file.
    pub is_video: bool,
    /// Whether the document is an animated GIF/video.
    pub is_animation: bool,
    /// Whether the document is an audio file.
    pub is_audio: bool,
}

/// Caption paired with media, embeds, quotes, or maps (`pageCaption#6f747657`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageCaption {
    /// Caption body text.
    pub text: RichText,
    /// Attribution / credit line.
    pub credit: RichText,
}

impl PageCaption {
    /// Returns true if both `text` and `credit` are empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.credit.is_empty()
    }
}

/// Geographic point for `pageBlockMap` (`geoPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GeoPoint {
    /// Latitude in degrees (`-90.0..=90.0`).
    pub lat: f64,
    /// Longitude in degrees (`-180.0..=180.0`).
    pub lon: f64,
    /// Telegram access hash for map tile requests.
    pub access_hash: i64,
}

impl GeoPoint {
    /// Validates that coordinates are finite and within WGS-84 bounds.
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }
}

/// Channel card metadata for `pageBlockChannel`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageChannel {
    /// Channel ID.
    pub id: i64,
    /// Display title of the channel.
    pub title: String,
    /// Public username without leading `@`, if any.
    pub username: Option<String>,
    /// Whether the current user has already joined the channel.
    pub joined: bool,
}

/// Horizontal alignment of a table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableAlign {
    /// Left-aligned (default).
    #[default]
    Left,
    /// Center-aligned (`align_center`).
    Center,
    /// Right-aligned (`align_right`).
    Right,
}

/// Vertical alignment of a table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableValign {
    /// Top-aligned (default).
    #[default]
    Top,
    /// Vertically centered (`valign_middle`).
    Middle,
    /// Bottom-aligned (`valign_bottom`).
    Bottom,
}

/// A single cell in a [`PageTableRow`] (`pageTableCell#34566b6a`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageTableCell {
    /// Optional cell text.
    pub text: Option<RichText>,
    /// True if rendered as `<th>` instead of `<td>`.
    pub header: bool,
    /// Horizontal column span (values `< 1` normalize to `1`).
    pub colspan: i32,
    /// Vertical row span (values `< 1` normalize to `1`).
    pub rowspan: i32,
    /// Horizontal alignment.
    pub align: TableAlign,
    /// Vertical alignment.
    pub valign: TableValign,
}

/// A row in a `pageBlockTable` (`pageTableRow#e0c0c5e5`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageTableRow {
    /// Cells in this row.
    pub cells: Vec<PageTableCell>,
}

/// Task checkbox state for list items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskState {
    /// Regular list item without a checkbox.
    #[default]
    None,
    /// Unchecked task checkbox.
    Unchecked,
    /// Checked task checkbox.
    Checked,
}

impl TaskState {
    /// Builds a [`TaskState`] from TL `checkbox` and `checked` flags.
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

/// Content of a list item (inline text or nested blocks).
#[derive(Debug, Clone, PartialEq)]
pub enum ListItemContent {
    /// Inline rich text (`pageListItemText` / `pageListOrderedItemText`).
    Text(RichText),
    /// Nested blocks (`pageListItemBlocks` / `pageListOrderedItemBlocks`).
    Blocks(Vec<PageBlock>),
}

/// Unordered list item (`pageListItemText` / `pageListItemBlocks`).
#[derive(Debug, Clone, PartialEq)]
pub struct PageListItem {
    /// Optional task checkbox state.
    pub task_state: TaskState,
    /// Item body.
    pub content: ListItemContent,
}

impl PageListItem {
    /// Convenience constructor for a plain text list item.
    pub fn text(text: RichText) -> Self {
        Self {
            task_state: TaskState::None,
            content: ListItemContent::Text(text),
        }
    }

    /// Convenience constructor for a nested-blocks list item.
    pub fn blocks(blocks: Vec<PageBlock>) -> Self {
        Self {
            task_state: TaskState::None,
            content: ListItemContent::Blocks(blocks),
        }
    }
}

/// Ordered list item (`pageListOrderedItemText` / `pageListOrderedItemBlocks`).
#[derive(Debug, Clone, PartialEq)]
pub struct PageListOrderedItem {
    /// Optional task checkbox state.
    pub task_state: TaskState,
    /// Explicit marker text provided by the server (e.g. `"1"`, `"a"`).
    pub num: Option<String>,
    /// Explicit numeric `value` attribute on `<li>`.
    pub value: Option<i32>,
    /// Explicit list marker type (`"1"`, `"a"`, `"A"`, `"i"`, `"I"`).
    pub list_type: Option<String>,
    /// Item body.
    pub content: ListItemContent,
}

impl PageListOrderedItem {
    /// Convenience constructor for an ordered text list item.
    pub fn text(num: impl Into<Option<String>>, text: RichText) -> Self {
        Self {
            task_state: TaskState::None,
            num: num.into(),
            value: None,
            list_type: None,
            content: ListItemContent::Text(text),
        }
    }

    /// Convenience constructor for an ordered nested-blocks list item.
    pub fn blocks(num: impl Into<Option<String>>, blocks: Vec<PageBlock>) -> Self {
        Self {
            task_state: TaskState::None,
            num: num.into(),
            value: None,
            list_type: None,
            content: ListItemContent::Blocks(blocks),
        }
    }
}

/// Related article card (`pageRelatedArticle#b390dc08`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageRelatedArticle {
    /// Article URL.
    pub url: String,
    /// Instant View webpage ID (0 if not an IV page).
    pub webpage_id: i64,
    /// Article title.
    pub title: Option<String>,
    /// Short description snippet.
    pub description: Option<String>,
    /// Preview thumbnail photo ID.
    pub photo_id: Option<i64>,
    /// Author name.
    pub author: Option<String>,
    /// Publication timestamp (Unix seconds).
    pub published_date: Option<i32>,
}

/// Formatting flags for `textDate#a5b45e2b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DateFlags {
    /// Display as relative time (`"r"`).
    pub relative: bool,
    /// Include short time (`"t"`).
    pub short_time: bool,
    /// Include long time (`"T"`).
    pub long_time: bool,
    /// Include short date (`"d"`).
    pub short_date: bool,
    /// Include long date (`"D"`).
    pub long_date: bool,
    /// Include day of week (`"w"`).
    pub day_of_week: bool,
}

impl DateFlags {
    /// Serializes flags to the compact `format` string used by `<tg-time>` in
    /// `iv_rich_message_html_export.cpp` (`FormattedDateFormat`).
    pub fn format_code(self) -> String {
        if self.relative {
            return "r".to_string();
        }
        let mut out = String::new();
        if self.day_of_week {
            out.push('w');
        }
        if self.long_date {
            out.push('D');
        } else if self.short_date {
            out.push('d');
        }
        if self.long_time {
            out.push('T');
        } else if self.short_time {
            out.push('t');
        }
        out
    }
}

/// All Telegram `RichText` variants (`schema/api.tl` layer 233).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum RichText {
    /// Empty rich text (`textEmpty`).
    #[default]
    Empty,
    /// Plain text (`textPlain`).
    Plain(String),
    /// Bold text (`textBold`).
    Bold(Box<RichText>),
    /// Italic text (`textItalic`).
    Italic(Box<RichText>),
    /// Underlined text (`textUnderline`).
    Underline(Box<RichText>),
    /// Strikethrough text (`textStrike`).
    Strike(Box<RichText>),
    /// Inline monospaced code (`textFixed`).
    Fixed(Box<RichText>),
    /// Hyperlink (`textUrl`).
    Url {
        /// Visible link label.
        text: Box<RichText>,
        /// Target URL.
        url: String,
        /// Target Instant View webpage ID (0 if external).
        webpage_id: i64,
    },
    /// Email link (`textEmail`).
    Email {
        /// Visible label.
        text: Box<RichText>,
        /// Email address.
        email: String,
    },
    /// Phone link (`textPhone`).
    Phone {
        /// Visible label.
        text: Box<RichText>,
        /// Phone number.
        phone: String,
    },
    /// Subscript text (`textSubscript`).
    Subscript(Box<RichText>),
    /// Superscript text (`textSuperscript`).
    Superscript(Box<RichText>),
    /// Highlighted / marked text (`textMarked`).
    Marked(Box<RichText>),
    /// Inline image (`textImage`).
    Image {
        /// Document ID of the inline image.
        document_id: i64,
        /// Image width in pixels.
        width: i32,
        /// Image height in pixels.
        height: i32,
    },
    /// Inline named anchor (`textAnchor`).
    Anchor {
        /// Wrapped rich text.
        text: Box<RichText>,
        /// Anchor fragment name.
        name: String,
    },
    /// Concatenation of multiple rich text fragments (`textConcat`).
    Concat(Vec<RichText>),
    /// Spoiler span (`textSpoiler`).
    Spoiler(Box<RichText>),
    /// Inline LaTeX math formula (`textMath`).
    Math {
        /// Raw TeX source.
        source: String,
    },
    /// Inline custom emoji (`textCustomEmoji`).
    CustomEmoji {
        /// Emoji document ID.
        document_id: i64,
        /// Fallback Unicode emoji text.
        alt: String,
    },
    /// `@username` mention (`textMention`).
    Mention(Box<RichText>),
    /// `#hashtag` (`textHashtag`).
    Hashtag(Box<RichText>),
    /// `/command` (`textBotCommand`).
    BotCommand(Box<RichText>),
    /// `$TICKER` cashtag (`textCashtag`).
    Cashtag(Box<RichText>),
    /// Auto-detected URL (`textAutoUrl`).
    AutoUrl(Box<RichText>),
    /// Auto-detected email (`textAutoEmail`).
    AutoEmail(Box<RichText>),
    /// Auto-detected phone (`textAutoPhone`).
    AutoPhone(Box<RichText>),
    /// Bank card number (`textBankCard`).
    BankCard(Box<RichText>),
    /// TON address (`textTonAddress`).
    TonAddress(Box<RichText>),
    /// User mention by ID (`textMentionName`).
    MentionName {
        /// Visible text.
        text: Box<RichText>,
        /// Telegram user ID.
        user_id: i64,
    },
    /// Formatted date/time (`textDate`).
    Date {
        /// Fallback display text.
        text: Box<RichText>,
        /// Unix timestamp in seconds.
        date: i32,
        /// Formatting flags.
        flags: DateFlags,
    },
    /// Inline text diff (`textDiff`).
    Diff {
        /// New text.
        text: Box<RichText>,
        /// Old text.
        old_text: Box<RichText>,
    },
}

impl RichText {
    /// Creates a [`RichText::Plain`] fragment.
    pub fn plain(s: impl Into<String>) -> Self {
        Self::Plain(s.into())
    }

    /// Creates a [`RichText::Bold`] wrapper.
    pub fn bold(inner: RichText) -> Self {
        Self::Bold(Box::new(inner))
    }

    /// Creates a [`RichText::Italic`] wrapper.
    pub fn italic(inner: RichText) -> Self {
        Self::Italic(Box::new(inner))
    }

    /// Creates a [`RichText::Underline`] wrapper.
    pub fn underline(inner: RichText) -> Self {
        Self::Underline(Box::new(inner))
    }

    /// Creates a [`RichText::Strike`] wrapper.
    pub fn strike(inner: RichText) -> Self {
        Self::Strike(Box::new(inner))
    }

    /// Creates a [`RichText::Fixed`] wrapper.
    pub fn fixed(inner: RichText) -> Self {
        Self::Fixed(Box::new(inner))
    }

    /// Creates a [`RichText::Subscript`] wrapper.
    pub fn subscript(inner: RichText) -> Self {
        Self::Subscript(Box::new(inner))
    }

    /// Creates a [`RichText::Superscript`] wrapper.
    pub fn superscript(inner: RichText) -> Self {
        Self::Superscript(Box::new(inner))
    }

    /// Creates a [`RichText::Marked`] wrapper.
    pub fn marked(inner: RichText) -> Self {
        Self::Marked(Box::new(inner))
    }

    /// Creates a [`RichText::Concat`] fragment.
    pub fn concat(parts: Vec<RichText>) -> Self {
        Self::Concat(parts)
    }

    /// Returns true if this rich text renders no visible content.
    pub fn is_empty(&self) -> bool {
        self.is_empty_depth(0)
    }

    fn is_empty_depth(&self, depth: usize) -> bool {
        if depth >= MAX_NESTING_DEPTH {
            return true;
        }
        match self {
            Self::Empty => true,
            Self::Plain(s) => s.is_empty(),
            Self::Bold(t)
            | Self::Italic(t)
            | Self::Underline(t)
            | Self::Strike(t)
            | Self::Fixed(t)
            | Self::Subscript(t)
            | Self::Superscript(t)
            | Self::Marked(t)
            | Self::Spoiler(t)
            | Self::Mention(t)
            | Self::Hashtag(t)
            | Self::BotCommand(t)
            | Self::Cashtag(t)
            | Self::AutoUrl(t)
            | Self::AutoEmail(t)
            | Self::AutoPhone(t)
            | Self::BankCard(t)
            | Self::TonAddress(t)
            | Self::Anchor { text: t, .. }
            | Self::MentionName { text: t, .. }
            | Self::Date { text: t, .. } => t.is_empty_depth(depth + 1),
            Self::Url { text, url, .. } => text.is_empty_depth(depth + 1) && url.is_empty(),
            Self::Email { text, email } => text.is_empty_depth(depth + 1) && email.is_empty(),
            Self::Phone { text, phone } => text.is_empty_depth(depth + 1) && phone.is_empty(),
            Self::Image { .. } => false,
            Self::Math { source } => source.trim().is_empty(),
            Self::CustomEmoji { alt, document_id } => alt.is_empty() && *document_id == 0,
            Self::Concat(parts) => parts.iter().all(|p| p.is_empty_depth(depth + 1)),
            Self::Diff { text, old_text } => {
                text.is_empty_depth(depth + 1) && old_text.is_empty_depth(depth + 1)
            }
        }
    }

    /// Extracts plain unformatted text (used for page `<title>` and fallback text).
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        self.collect_plain(&mut out, 0);
        out
    }

    fn collect_plain(&self, out: &mut String, depth: usize) {
        if depth >= MAX_NESTING_DEPTH {
            return;
        }
        match self {
            Self::Empty => {}
            Self::Plain(s) => out.push_str(s),
            Self::Bold(t)
            | Self::Italic(t)
            | Self::Underline(t)
            | Self::Strike(t)
            | Self::Fixed(t)
            | Self::Subscript(t)
            | Self::Superscript(t)
            | Self::Marked(t)
            | Self::Spoiler(t)
            | Self::Mention(t)
            | Self::Hashtag(t)
            | Self::BotCommand(t)
            | Self::Cashtag(t)
            | Self::AutoUrl(t)
            | Self::AutoEmail(t)
            | Self::AutoPhone(t)
            | Self::BankCard(t)
            | Self::TonAddress(t)
            | Self::Anchor { text: t, .. }
            | Self::MentionName { text: t, .. }
            | Self::Date { text: t, .. } => t.collect_plain(out, depth + 1),
            Self::Url { text, url, .. } => {
                let start = out.len();
                text.collect_plain(out, depth + 1);
                if out.len() == start {
                    out.push_str(url);
                }
            }
            Self::Email { text, email } => {
                let start = out.len();
                text.collect_plain(out, depth + 1);
                if out.len() == start {
                    out.push_str(email);
                }
            }
            Self::Phone { text, phone } => {
                let start = out.len();
                text.collect_plain(out, depth + 1);
                if out.len() == start {
                    out.push_str(phone);
                }
            }
            Self::Image { .. } => out.push_str("[image]"),
            Self::Math { source } => out.push_str(trim_math_source(source)),
            Self::CustomEmoji { alt, .. } => out.push_str(alt),
            Self::Concat(parts) => {
                for p in parts {
                    p.collect_plain(out, depth + 1);
                }
            }
            Self::Diff { text, .. } => text.collect_plain(out, depth + 1),
        }
    }
}

/// All Telegram `PageBlock` variants (`schema/api.tl` layer 233).
#[derive(Debug, Clone, PartialEq)]
pub enum PageBlock {
    /// Unsupported block (`pageBlockUnsupported`).
    Unsupported,
    /// Article title (`pageBlockTitle`).
    Title(RichText),
    /// Article subtitle (`pageBlockSubtitle`).
    Subtitle(RichText),
    /// Author & publication date byline (`pageBlockAuthorDate`).
    AuthorDate {
        /// Author rich text.
        author: RichText,
        /// Unix timestamp in seconds (0 if omitted).
        published_date: i32,
    },
    /// Section header (`pageBlockHeader`).
    Header(RichText),
    /// Section subheader (`pageBlockSubheader`).
    Subheader(RichText),
    /// Explicit heading level 1..=6 (`pageBlockHeading1`..`pageBlockHeading6`).
    Heading {
        /// Heading level (clamped to `1..=6`).
        level: u8,
        /// Heading text.
        text: RichText,
    },
    /// Body paragraph (`pageBlockParagraph`).
    Paragraph(RichText),
    /// Preformatted code block (`pageBlockPreformatted`).
    Preformatted {
        /// Code text.
        text: RichText,
        /// Optional language tag (e.g. `"rust"`).
        language: String,
    },
    /// Footer block (`pageBlockFooter`).
    Footer(RichText),
    /// Horizontal divider (`pageBlockDivider`).
    Divider,
    /// Named anchor (`pageBlockAnchor`).
    Anchor {
        /// Anchor fragment name.
        name: String,
    },
    /// Unordered bullet or task list (`pageBlockList`).
    List {
        /// List items.
        items: Vec<PageListItem>,
    },
    /// Ordered list (`pageBlockOrderedList`).
    OrderedList {
        /// Ordered items.
        items: Vec<PageListOrderedItem>,
        /// Reverse numbering flag.
        reversed: bool,
        /// Optional starting counter.
        start: Option<i32>,
        /// Optional HTML list type (`"1"`, `"a"`, `"A"`, `"i"`, `"I"`).
        list_type: Option<String>,
    },
    /// Blockquote with inline text (`pageBlockBlockquote`).
    Blockquote {
        /// Quote body.
        text: RichText,
        /// Quote attribution.
        caption: RichText,
        /// Whether the quote is collapsed by default.
        collapsed: bool,
    },
    /// Blockquote containing nested blocks (`pageBlockBlockquoteBlocks`).
    BlockquoteBlocks {
        /// Nested blocks.
        blocks: Vec<PageBlock>,
        /// Quote attribution.
        caption: RichText,
    },
    /// Pullquote (`pageBlockPullquote`).
    Pullquote {
        /// Pullquote body.
        text: RichText,
        /// Pullquote attribution.
        caption: RichText,
    },
    /// Single photo (`pageBlockPhoto`).
    Photo {
        /// Telegram photo ID.
        photo_id: i64,
        /// Caption and credit.
        caption: PageCaption,
        /// Optional click-through URL.
        url: Option<String>,
        /// Optional Instant View webpage ID for `url`.
        webpage_id: Option<i64>,
        /// Spoiler blur flag.
        spoiler: bool,
    },
    /// Single video (`pageBlockVideo`).
    Video {
        /// Telegram document ID.
        video_id: i64,
        /// Caption and credit.
        caption: PageCaption,
        /// Autoplay flag.
        autoplay: bool,
        /// Loop playback flag.
        loop_video: bool,
        /// Spoiler blur flag.
        spoiler: bool,
    },
    /// Cover media wrapper (`pageBlockCover`).
    Cover(Box<PageBlock>),
    /// Embedded external widget / iframe (`pageBlockEmbed`).
    Embed {
        /// External embed URL.
        url: Option<String>,
        /// Raw embed HTML snippet (strictly sandboxed by the renderer!).
        html: Option<String>,
        /// Optional poster photo ID.
        poster_photo_id: Option<i64>,
        /// Requested embed width.
        width: Option<i32>,
        /// Requested embed height.
        height: Option<i32>,
        /// Span full article width.
        full_width: bool,
        /// Allow scrolling inside the embed frame.
        allow_scrolling: bool,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Embedded social post (`pageBlockEmbedPost`).
    EmbedPost {
        /// Original post URL.
        url: String,
        /// Webpage ID if available.
        webpage_id: i64,
        /// Author avatar photo ID.
        author_photo_id: i64,
        /// Author display name.
        author: String,
        /// Post timestamp (Unix seconds).
        date: i32,
        /// Nested content blocks.
        blocks: Vec<PageBlock>,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Collage media group (`pageBlockCollage`).
    Collage {
        /// Photo/video items.
        items: Vec<PageBlock>,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Interactive slideshow (`pageBlockSlideshow`).
    Slideshow {
        /// Photo/video items.
        items: Vec<PageBlock>,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Embedded Telegram channel banner (`pageBlockChannel`).
    Channel(PageChannel),
    /// Audio player block (`pageBlockAudio`).
    Audio {
        /// Telegram document ID.
        audio_id: i64,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Kicker / category eyebrow (`pageBlockKicker`).
    Kicker(RichText),
    /// Structured table (`pageBlockTable`).
    Table {
        /// Optional table title.
        title: RichText,
        /// Table rows.
        rows: Vec<PageTableRow>,
        /// Render cell borders.
        bordered: bool,
        /// Alternate row background shading.
        striped: bool,
        /// Compact cell padding.
        compact: bool,
    },
    /// Collapsible `<details>` section (`pageBlockDetails`).
    Details {
        /// Summary title.
        title: RichText,
        /// Collapsible child blocks.
        blocks: Vec<PageBlock>,
        /// Expanded by default.
        open: bool,
    },
    /// Related articles section (`pageBlockRelatedArticles`).
    RelatedArticles {
        /// Section heading.
        title: RichText,
        /// Related article cards.
        articles: Vec<PageRelatedArticle>,
    },
    /// Geographic map snapshot (`pageBlockMap`).
    Map {
        /// Center coordinates.
        geo: GeoPoint,
        /// Zoom level (`1..=20`).
        zoom: i32,
        /// Width in pixels.
        width: i32,
        /// Height in pixels.
        height: i32,
        /// Caption and credit.
        caption: PageCaption,
    },
    /// Display math block (`pageBlockMath`).
    Math {
        /// TeX formula source.
        source: String,
    },
    /// Thinking / reasoning callout block (`pageBlockThinking`).
    Thinking(RichText),
    /// Generic downloadable document attachment (`pageBlockDocument`).
    Document {
        /// Telegram document ID.
        document_id: i64,
        /// Caption and credit.
        caption: PageCaption,
    },
}

/// Strips optional surrounding `$...$` delimiters from TeX source, matching
/// `FormulaTexFromSource` in `iv_rich_page.cpp`.
pub fn trim_math_source(source: &str) -> &str {
    let trimmed = source.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('$') && trimmed.ends_with('$') {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    }
}

/// Normalizes an anchor / URL fragment identifier, matching `Iv::Markdown::NormalizeFragmentId`
/// (`iv_markdown_prepare_links.cpp`).
///
/// Percent-decodes UTF-8 sequences, trims whitespace, lowercases, and strips leading `#` characters.
pub fn normalize_fragment_id(fragment: &str) -> String {
    let decoded = percent_decode_utf8(fragment);
    let trimmed = decoded.trim().to_lowercase();
    trimmed.trim_start_matches('#').to_string()
}

/// Decodes percent-encoded UTF-8 sequences (`%XX`), leaving invalid escapes verbatim.
pub(crate) fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Formats an ordered list marker in alphabetic form (`a`, `b`, ..., `z`, `aa`),
/// matching `OrderedAlphaText` in `iv_rich_page.cpp`.
pub fn ordered_alpha_text(mut value: i32, upper: bool) -> String {
    if value <= 0 {
        return value.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    let mut chars = Vec::new();
    while value > 0 {
        value -= 1;
        chars.push((base + (value % 26) as u8) as char);
        value /= 26;
    }
    chars.into_iter().rev().collect()
}

/// Formats an ordered list marker in Roman numerals (`1..=3999`),
/// matching `OrderedRomanText` in `iv_rich_page.cpp`.
pub fn ordered_roman_text(mut value: i32, upper: bool) -> String {
    if !(1..=3999).contains(&value) {
        return value.to_string();
    }
    const PARTS: &[(i32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(v, text) in PARTS {
        while value >= v {
            out.push_str(text);
            value -= v;
        }
    }
    if upper { out } else { out.to_ascii_lowercase() }
}
