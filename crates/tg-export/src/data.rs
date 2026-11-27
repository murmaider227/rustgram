//! Port of `export/data/export_data_types.h` and pure helpers from `export/data/export_data_types.cpp`.
//!
//! Plain Rust export data model independent of `tg-tl`; `tg-api` populates these types
//! from TL responses and passes them to `tg-export` writers and `ExportController`.

use std::collections::BTreeMap;
use std::path::Path;

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader};

use tg_text::utf16::{from_utf16_lossy, to_utf16};
use tg_tl::types::{
    InputDocumentFileLocation, InputPeerChat, InputPeerUser, InputPhotoFileLocation,
    InputUser as TlInputUser,
};

use crate::output::file::OutputFile;
use crate::settings::{ExportTypes, Settings};
use crate::stand_in::{
    BareId, Birthday, ChannelId, ChatId, CreditsAmount, InputFileLocation, InputPeer, InputUser,
    PEER_ID_CHAT_TYPE_MASK, PeerId, Size, TimeId, UserId, file_name_from_user_string,
    fill_amount_and_currency, format_duration_text, format_phone, format_utc_offset, unix_to_civil,
};

/// One Gram in nano-units (`10^9`).
pub const NANOS_IN_GRAM: i64 = 1_000_000_000;

/// Maximum image dimension accepted by `write_image_thumb` (`kMaxImageSize`).
pub const MAX_IMAGE_SIZE: u32 = 10_000;

/// ID shift applied to messages from a migrated basic group (`kMigratedMessagesIdShift`).
pub const MIGRATED_MESSAGES_ID_SHIFT: i32 = -1_000_000_000;

/// Computes the avatar color index `0..8` for a bare peer ID (port of `PeerColorIndex(BareId)`).
pub fn peer_color_index(bare_id: BareId) -> u8 {
    const MAP: [u8; 7] = [0, 7, 4, 1, 6, 3, 5];
    MAP[(bare_id % (MAP.len() as u64)) as usize]
}

/// Extracts the bare ID from a `PeerId` (port of `PeerToBareId`).
pub const fn peer_to_bare_id(peer_id: PeerId) -> BareId {
    peer_id.raw() & PEER_ID_CHAT_TYPE_MASK
}

/// Computes the avatar color index for a `PeerId` (port of `PeerColorIndex(PeerId)`).
pub fn peer_color_index_peer(peer_id: PeerId) -> u8 {
    peer_color_index(peer_to_bare_id(peer_id))
}

/// Computes a deterministic 8-bit pseudo-peer-ID hash from UTF-8 bytes (port of `StringBarePeerId`).
pub fn string_bare_peer_id(data: &[u8]) -> BareId {
    let mut result: u64 = 0xFF;
    for &ch in data {
        result = result.wrapping_mul(239).wrapping_add(u64::from(ch)) & 0xFF;
    }
    result
}

/// Computes the icon color index for an application ID (port of `ApplicationColorIndex`).
pub fn application_color_index(application_id: i32) -> u8 {
    match application_id {
        1 | 7 => 0,     // iOS / iOS X
        6 | 21724 => 1, // Android / Android X
        2834 => 2,      // macOS
        2496 => 3,      // Webogram
        2040 => 4,      // Desktop
        1429 => 5,      // Windows Phone
        _ => peer_color_index(application_id as u64),
    }
}

/// Computes a synthetic application ID from a web-session domain (port of `DomainApplicationId`).
pub fn domain_application_id(domain: &str) -> i32 {
    0x1000 + string_bare_peer_id(domain.as_bytes()) as i32
}

/// Left-pads `data` with `filler` up to `length` bytes (port of `FillLeft`).
pub fn fill_left(data: &str, length: usize, filler: char) -> String {
    if length <= data.len() {
        return data.to_string();
    }
    let mut out = String::with_capacity(length);
    for _ in 0..(length - data.len()) {
        out.push(filler);
    }
    out.push_str(data);
    out
}

/// Formats an integer with optional zero/char left-padding (port of `NumberToString`).
pub fn number_to_string(value: impl std::fmt::Display, length: usize, filler: char) -> String {
    let s = value.to_string().replace(',', ".");
    fill_left(&s, length, filler)
}

/// Formats an `f64` matching C++ `std::to_string(double)` (always 6 fractional digits, `.` decimal).
pub fn float_to_string(value: f64) -> String {
    format!("{value:.6}")
}

/// Text entity classification (port of `Export::Data::TextPart::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextPartType {
    /// Plain unstyled text.
    #[default]
    Text,
    /// Unknown entity.
    Unknown,
    /// `@username` mention.
    Mention,
    /// `#hashtag`.
    Hashtag,
    /// `/command`.
    BotCommand,
    /// Bare URL.
    Url,
    /// Email address.
    Email,
    /// Bold text.
    Bold,
    /// Italic text.
    Italic,
    /// Inline monospace code.
    Code,
    /// Preformatted code block (`additional` = language).
    Pre,
    /// Hyperlink with label (`additional` = href).
    TextUrl,
    /// Mention by user ID (`additional` = decimal user ID).
    MentionName,
    /// Phone number.
    Phone,
    /// `$CASHTAG`.
    Cashtag,
    /// Underlined text.
    Underline,
    /// Strikethrough text.
    Strike,
    /// Blockquote (`additional` = non-empty `"1"` if collapsed).
    Blockquote,
    /// Bank card number.
    BankCard,
    /// TON wallet address.
    TonAddress,
    /// Spoiler text.
    Spoiler,
    /// Custom emoji (`additional` = relative path, document ID, or `"(unavailable)"`).
    CustomEmoji,
}

/// Segment of formatted message or caption text (port of `Export::Data::TextPart`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextPart {
    /// Entity kind.
    pub kind: TextPartType,
    /// Segment UTF-8 text.
    pub text: String,
    /// Extra entity metadata (`href`, `language`, `user_id`, `document_id`, collapsed flag).
    pub additional: String,
}

impl TextPart {
    /// Sentinel value for an unavailable custom emoji (port of `TextPart::UnavailableEmoji`).
    pub const UNAVAILABLE_EMOJI: &'static str = "(unavailable)";

    /// Creates a plain-text `TextPart`.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            kind: TextPartType::Text,
            text: text.into(),
            additional: String::new(),
        }
    }

    /// Creates a typed `TextPart` with optional additional metadata.
    pub fn with_type(
        kind: TextPartType,
        text: impl Into<String>,
        additional: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            text: text.into(),
            additional: additional.into(),
        }
    }
}

/// Pure input representation of a Telegram `MessageEntity` (UTF-16 offsets/lengths, independent of `tg-tl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEntityInput {
    /// Entity type.
    pub kind: TextPartType,
    /// Start offset in UTF-16 code units.
    pub offset: i32,
    /// Length in UTF-16 code units.
    pub length: i32,
    /// Extra metadata (`language`, `url`, decimal `user_id`, decimal `document_id`, or `"1"` for collapsed blockquote).
    pub additional: String,
}

/// Splits `text` according to Telegram UTF-16 `entities` (exact port of `ParseText` in `export_data_types.cpp:274-359`).
pub fn parse_text_entities(text: &str, entities: &[TextEntityInput]) -> Vec<TextPart> {
    let utf16: Vec<u16> = to_utf16(text);
    let utf16_len = utf16.len();
    let slice_utf16 = |start: usize, len: usize| -> String {
        let end = (start + len).min(utf16_len);
        if start >= end {
            return String::new();
        }
        from_utf16_lossy(&utf16[start..end])
    };

    let mut result = Vec::new();
    let mut offset: i32 = 0;

    for entity in entities {
        let start = entity.offset;
        let length = entity.length;
        // Bounds in UTF-16 units, like `QString::size()` (tdesktop 7.4.7 `3fc0789`).
        // i64: hostile server offsets would overflow `start + length` in i32.
        if start < offset || length <= 0 || i64::from(start) + i64::from(length) > utf16_len as i64
        {
            continue;
        }
        if start > offset {
            let chunk = slice_utf16(offset as usize, (start - offset) as usize);
            if !chunk.is_empty() {
                result.push(TextPart::plain(chunk));
            }
        }
        let chunk = slice_utf16(start as usize, length as usize);
        result.push(TextPart {
            kind: entity.kind,
            text: chunk,
            additional: entity.additional.clone(),
        });
        offset = start + length;
    }

    if (offset as usize) < utf16_len {
        let tail = slice_utf16(offset as usize, utf16_len - offset as usize);
        if !tail.is_empty() {
            result.push(TextPart::plain(tail));
        }
    }

    result
}

/// Target peer type filter for switch-inline buttons (port of `InlineButtonPeerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineButtonPeerType {
    /// Same bot PM.
    SameBotPm,
    /// Private chat.
    Pm,
    /// Basic group chat.
    Chat,
    /// Supergroup.
    Megagroup,
    /// Broadcast channel.
    Broadcast,
    /// Bot PM.
    BotPm,
}

impl InlineButtonPeerType {
    /// Wire/export string representation (port of `InlineButtonPeerTypeToString`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameBotPm => "same_bot_pm",
            Self::Pm => "pm",
            Self::Chat => "chat",
            Self::Megagroup => "megagroup",
            Self::Broadcast => "broadcast",
            Self::BotPm => "bot_pm",
        }
    }
}

/// Action kind for rich inline buttons (port of `InlineButtonAction::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum InlineButtonActionType {
    /// Opens a URL.
    #[default]
    Url,
    /// Login/URL auth button.
    Auth,
    /// Opens a WebView URL.
    WebView,
    /// Sends callback data.
    Callback,
    /// Sends callback data requiring 2FA password.
    CallbackWithPassword,
    /// Launches an inline game.
    Game,
    /// Initiates payment.
    Buy,
    /// Switches to inline query in another chat.
    SwitchInline,
    /// Switches to inline query in the same chat.
    SwitchInlineSame,
    /// Opens a user profile.
    UserProfile,
    /// Copies text to clipboard.
    CopyText,
    /// Disabled button.
    Disabled,
}

/// Rich inline button action payload (port of `InlineButtonAction`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineButtonAction {
    /// Target URL for `Url`/`Auth`/`WebView`.
    pub url: String,
    /// Optional forward text for `Auth`.
    pub forward_text: Option<String>,
    /// Raw callback payload bytes.
    pub callback_data: Vec<u8>,
    /// Inline query string for `SwitchInline*`.
    pub query: String,
    /// Optional peer filter types for `SwitchInline*`.
    pub peer_types: Option<Vec<InlineButtonPeerType>>,
    /// Text to copy for `CopyText`.
    pub copy_text: String,
    /// Target user ID for `UserProfile`.
    pub user_id: u64,
    /// Button ID for `Auth`.
    pub button_id: i32,
    /// Action variant.
    pub kind: InlineButtonActionType,
    /// Whether callback requires 2FA password.
    pub requires_password: bool,
    /// Whether switch-inline targets the same peer.
    pub same_peer: bool,
}

impl InlineButtonAction {
    /// Export string representation of `self.kind` (port of `InlineButtonAction::TypeToString`).
    pub const fn type_str(&self) -> &'static str {
        match self.kind {
            InlineButtonActionType::Url => "url",
            InlineButtonActionType::Auth => "auth",
            InlineButtonActionType::WebView => "web_view",
            InlineButtonActionType::Callback => "callback",
            InlineButtonActionType::CallbackWithPassword => "callback_with_password",
            InlineButtonActionType::Game => "game",
            InlineButtonActionType::Buy => "buy",
            InlineButtonActionType::SwitchInline => "switch_inline",
            InlineButtonActionType::SwitchInlineSame => "switch_inline_same",
            InlineButtonActionType::UserProfile => "user_profile",
            InlineButtonActionType::CopyText => "copy_text",
            InlineButtonActionType::Disabled => "disabled",
        }
    }
}

/// Visual style for rich buttons (port of `RichButtonStyle`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichButtonStyle {
    /// Default neutral style.
    #[default]
    Default,
    /// Primary accent style.
    Primary,
    /// Success style.
    Success,
    /// Danger/destructive style.
    Danger,
    /// Inline link style.
    Link,
}

impl RichButtonStyle {
    /// Export string representation (port of `RichButtonStyleToString`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Primary => "primary",
            Self::Success => "success",
            Self::Danger => "danger",
            Self::Link => "link",
        }
    }
}

/// Alignment for rich button rows (port of `RichButtonAlignment`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichButtonAlignment {
    /// Stretch across container.
    #[default]
    Stretch,
    /// Left-aligned.
    Left,
    /// Center-aligned.
    Center,
    /// Right-aligned.
    Right,
}

impl RichButtonAlignment {
    /// Export string representation (port of `RichButtonAlignmentToString`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }
}

/// Payload for `RichTextType::Button` (port of `RichButtonPayload`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichButtonPayload {
    /// Button action.
    pub action: InlineButtonAction,
    /// Optional visual style.
    pub style: Option<RichButtonStyle>,
}

/// Rich text node variant (port of `Export::Data::RichText::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichTextType {
    /// Empty node.
    #[default]
    Empty,
    /// Plain text leaf.
    Plain,
    /// Concatenation of children.
    Concat,
    /// Bold wrapper.
    Bold,
    /// Italic wrapper.
    Italic,
    /// Underline wrapper.
    Underline,
    /// Strikethrough wrapper.
    Strike,
    /// Inline monospace code wrapper.
    Fixed,
    /// Hyperlink wrapper.
    Url,
    /// Email link wrapper.
    Email,
    /// Phone link wrapper.
    Phone,
    /// Subscript wrapper.
    Subscript,
    /// Superscript wrapper.
    Superscript,
    /// Highlighted/marked wrapper.
    Marked,
    /// Named anchor wrapper.
    Anchor,
    /// Inline math source leaf.
    Math,
    /// Custom emoji leaf.
    CustomEmoji,
    /// Spoiler wrapper.
    Spoiler,
    /// Mention wrapper.
    Mention,
    /// Hashtag wrapper.
    Hashtag,
    /// Bot command wrapper.
    BotCommand,
    /// Cashtag wrapper.
    Cashtag,
    /// Auto-detected URL wrapper.
    AutoUrl,
    /// Auto-detected email wrapper.
    AutoEmail,
    /// Auto-detected phone wrapper.
    AutoPhone,
    /// Bank card wrapper.
    BankCard,
    /// TON address wrapper.
    TonAddress,
    /// User mention-by-ID wrapper.
    MentionName,
    /// Formatted date/time wrapper.
    FormattedDate,
    /// Inline image leaf.
    InlineImage,
    /// Diff wrapper (`children` = new, `old_children` = old).
    Diff,
    /// Inline/block button node.
    Button,
}

/// Recursive rich-text AST node (port of `Export::Data::RichText`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichText {
    /// Text content (for `Plain` and `CustomEmoji` alt).
    pub text: String,
    /// Secondary string data (`href`, `email`, `phone`, anchor `name`, math `source`).
    pub data: String,
    /// Custom emoji document ID or resolved relative path.
    pub custom_emoji_data: String,
    /// Child nodes.
    pub children: Vec<RichText>,
    /// Previous-version children for `Diff`.
    pub old_children: Vec<RichText>,
    /// Button payload for `Button`.
    pub button: Option<Box<RichButtonPayload>>,
    /// Numeric ID (`webpage_id`, `user_id`, `document_id`).
    pub id: u64,
    /// Timestamp for `FormattedDate`.
    pub date: TimeId,
    /// Source width for `InlineImage`.
    pub width: i32,
    /// Source height for `InlineImage`.
    pub height: i32,
    /// Node type.
    pub kind: RichTextType,
    /// Relative date flag (`FormattedDate`).
    pub relative: bool,
    /// Short time flag (`FormattedDate`).
    pub short_time: bool,
    /// Long time flag (`FormattedDate`).
    pub long_time: bool,
    /// Short date flag (`FormattedDate`).
    pub short_date: bool,
    /// Long date flag (`FormattedDate`).
    pub long_date: bool,
    /// Day-of-week flag (`FormattedDate`).
    pub day_of_week: bool,
    /// Unsupported marker (set on `Diff`).
    pub unsupported: bool,
}

/// Caption + credit pair for rich media blocks (port of `RichCaption`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichCaption {
    /// Caption body.
    pub text: RichText,
    /// Attribution/credit text.
    pub credit: RichText,
}

/// Task checkbox state for rich list items (port of `RichTaskState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichTaskState {
    /// Regular list item.
    #[default]
    None,
    /// Unchecked task item.
    Unchecked,
    /// Checked task item.
    Checked,
}

/// Bullet vs ordered rich list kind (port of `RichListKind`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichListKind {
    /// Unordered bullet list.
    #[default]
    Bullet,
    /// Ordered list.
    Ordered,
}

/// Content mode for a rich list item (port of `RichListItemContent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichListItemContent {
    /// Single `RichText`.
    #[default]
    Text,
    /// Nested `RichBlock` list.
    Blocks,
}

/// Content mode for a rich blockquote (port of `RichQuoteContent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichQuoteContent {
    /// Single `RichText`.
    #[default]
    Text,
    /// Nested `RichBlock` list.
    Blocks,
}

/// Ordered list metadata (port of `RichOrderedList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichOrderedList {
    /// Optional HTML/custom list marker type.
    pub list_type: Option<String>,
    /// Optional starting counter.
    pub start: Option<i32>,
    /// Reversed numbering flag.
    pub reversed: bool,
}

/// Item inside a rich list block (port of `RichListItem`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichListItem {
    /// Text content when `content == Text`.
    pub text: Option<RichText>,
    /// Nested blocks when `content == Blocks`.
    pub blocks: Vec<RichBlock>,
    /// Custom marker string.
    pub num: Option<String>,
    /// Optional item type.
    pub item_type: Option<String>,
    /// Optional numeric item value.
    pub value: Option<i32>,
    /// Task checkbox state.
    pub task_state: RichTaskState,
    /// Text vs blocks content selector.
    pub content: RichListItemContent,
}

/// Horizontal alignment in rich table cells (port of `RichTableAlignment`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichTableAlignment {
    /// Left aligned.
    #[default]
    Left,
    /// Center aligned.
    Center,
    /// Right aligned.
    Right,
}

/// Vertical alignment in rich table cells (port of `RichTableVerticalAlignment`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichTableVerticalAlignment {
    /// Top aligned.
    #[default]
    Top,
    /// Middle aligned.
    Middle,
    /// Bottom aligned.
    Bottom,
}

/// Cell inside a rich table row (port of `RichTableCell`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichTableCell {
    /// Cell text content.
    pub text: Option<RichText>,
    /// Optional column span.
    pub colspan: Option<i32>,
    /// Optional row span.
    pub rowspan: Option<i32>,
    /// Horizontal alignment.
    pub alignment: RichTableAlignment,
    /// Vertical alignment.
    pub vertical_alignment: RichTableVerticalAlignment,
    /// Header cell flag (`<th>` vs `<td>`).
    pub header: bool,
}

/// Row inside a rich table block (port of `RichTableRow`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichTableRow {
    /// Row cells.
    pub cells: Vec<RichTableCell>,
}

/// Related article item (port of `RichRelatedArticle`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichRelatedArticle {
    /// Article URL.
    pub url: String,
    /// Optional title.
    pub title: Option<String>,
    /// Optional description.
    pub description: Option<String>,
    /// Optional author.
    pub author: Option<String>,
    /// Optional photo ID referencing `RichMessage::photos`.
    pub photo_id: Option<u64>,
    /// Optional published timestamp.
    pub published_date: Option<TimeId>,
    /// Webpage ID.
    pub webpage_id: u64,
}

/// Source chat/channel variant for `RichChannel` (port of `RichChannel::Source`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichChannelSource {
    /// `chatEmpty`.
    #[default]
    ChatEmpty,
    /// `chat`.
    Chat,
    /// `chatForbidden`.
    ChatForbidden,
    /// `channel`.
    Channel,
    /// `channelForbidden`.
    ChannelForbidden,
    /// `community`.
    Community,
    /// `communityForbidden`.
    CommunityForbidden,
}

/// Embedded channel card inside a rich message (port of `RichChannel`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichChannel {
    /// Optional title.
    pub title: Option<String>,
    /// Optional public username.
    pub username: Option<String>,
    /// Optional access hash.
    pub access_hash: Option<i64>,
    /// Channel/chat ID.
    pub id: u64,
    /// TL source kind.
    pub source: RichChannelSource,
    /// Broadcast flag.
    pub broadcast: bool,
    /// Megagroup flag.
    pub megagroup: bool,
    /// Monoforum flag.
    pub monoforum: bool,
}

/// Source point variant for `RichMapPoint` (port of `RichMapPoint::Source`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichMapPointSource {
    /// `geoPointEmpty`.
    #[default]
    GeoPointEmpty,
    /// `geoPoint`.
    GeoPoint,
    /// `inputGeoPointEmpty`.
    InputGeoPointEmpty,
    /// `inputGeoPoint`.
    InputGeoPoint,
}

/// Geographic coordinate inside a rich map block (port of `RichMapPoint`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichMapPoint {
    /// Optional access hash.
    pub access_hash: Option<i64>,
    /// Optional accuracy radius in meters.
    pub accuracy_radius: Option<i32>,
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// TL source kind.
    pub source: RichMapPointSource,
}

/// Rich block variant (port of `Export::Data::RichBlock::Kind`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RichBlockKind {
    /// Explicitly unsupported block (`pageBlockUnsupported`).
    Unsupported,
    /// Heading (`h1`..`h6`).
    Heading,
    /// Paragraph (`p`).
    Paragraph,
    /// Footer (`footer`).
    Footer,
    /// Thinking block.
    Thinking,
    /// Author + date block.
    AuthorDate,
    /// Preformatted code block.
    Code,
    /// Horizontal divider (`hr`).
    Divider,
    /// Named anchor target.
    Anchor,
    /// Bullet or ordered list.
    List,
    /// Blockquote or pullquote.
    Quote,
    /// Photo block.
    Photo,
    /// Video block.
    Video,
    /// Cover wrapper block.
    Cover,
    /// External embed block.
    Embed,
    /// Embedded post block.
    EmbedPost,
    /// Media collage block.
    Collage,
    /// Media slideshow block.
    Slideshow,
    /// Embedded channel reference block.
    Channel,
    /// Audio block.
    Audio,
    /// Generic file/document block.
    File,
    /// Display math formula block.
    Math,
    /// Table block.
    Table,
    /// Collapsible details block.
    Details,
    /// Related articles list block.
    RelatedArticles,
    /// Map block.
    Map,
    /// Input map block.
    InputMap,
    /// Row of buttons.
    ButtonRow,
    /// Unknown/unrecognized block.
    #[default]
    Unknown,
}

/// Block element inside a `RichMessage` (port of `Export::Data::RichBlock`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichBlock {
    /// Primary text (heading, paragraph, footer, thinking, author, code, quote, table title, details summary, related title).
    pub text: RichText,
    /// Quote attribution caption.
    pub quote_caption: RichText,
    /// Media/embed/map caption.
    pub caption: RichCaption,
    /// Ordered list configuration.
    pub ordered_list: RichOrderedList,
    /// Channel card data.
    pub channel: RichChannel,
    /// Map point data.
    pub map_point: RichMapPoint,
    /// Code language.
    pub language: String,
    /// Math formula source.
    pub formula: String,
    /// Anchor name.
    pub name: String,
    /// EmbedPost URL.
    pub url: String,
    /// EmbedPost author.
    pub author: String,
    /// Optional URL (Photo, Embed).
    pub optional_url: Option<String>,
    /// Optional raw embed HTML.
    pub html: Option<String>,
    /// Nested blocks (Cover, Quote-blocks, EmbedPost, Collage, Slideshow, Details).
    pub blocks: Vec<RichBlock>,
    /// List items (List).
    pub list_items: Vec<RichListItem>,
    /// Table rows (Table).
    pub table_rows: Vec<RichTableRow>,
    /// Related articles (RelatedArticles).
    pub related_articles: Vec<RichRelatedArticle>,
    /// Buttons (ButtonRow).
    pub buttons: Vec<RichText>,
    /// Optional webpage ID (Photo).
    pub optional_webpage_id: Option<u64>,
    /// Optional poster photo ID (Embed).
    pub poster_photo_id: Option<u64>,
    /// Optional embed width.
    pub width: Option<i32>,
    /// Optional embed height.
    pub height: Option<i32>,
    /// Photo ID (Photo).
    pub photo_id: u64,
    /// Document ID (Video, Audio, File).
    pub document_id: u64,
    /// Webpage ID (EmbedPost).
    pub webpage_id: u64,
    /// Author photo ID (EmbedPost).
    pub author_photo_id: u64,
    /// Date timestamp (AuthorDate, EmbedPost).
    pub date: TimeId,
    /// Heading level 1..=6.
    pub heading_level: i32,
    /// Map zoom level.
    pub zoom: i32,
    /// Map source width.
    pub map_width: i32,
    /// Map source height.
    pub map_height: i32,
    /// Block kind.
    pub kind: RichBlockKind,
    /// List kind (bullet vs ordered).
    pub list_kind: RichListKind,
    /// Quote content kind (text vs blocks).
    pub quote_content: RichQuoteContent,
    /// Button row alignment.
    pub button_alignment: RichButtonAlignment,
    /// Unsupported marker.
    pub unsupported: bool,
    /// Full-width embed flag.
    pub full_width: bool,
    /// Allow scrolling embed flag.
    pub allow_scrolling: bool,
    /// Autoplay video flag.
    pub autoplay: bool,
    /// Loop video flag.
    pub is_loop: bool,
    /// Spoiler flag (Photo, Video).
    pub spoiler: bool,
    /// Initially open flag (Details).
    pub open: bool,
    /// Bordered table flag.
    pub bordered: bool,
    /// Striped table flag.
    pub striped: bool,
    /// Compact table flag.
    pub compact: bool,
    /// Pullquote style flag.
    pub pullquote: bool,
}

/// Summary info for userpics step (port of `UserpicsInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserpicsInfo {
    /// Total number of profile pictures.
    pub count: i32,
}

/// Summary info for stories step (port of `StoriesInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoriesInfo {
    /// Total number of archived stories.
    pub count: i32,
}

/// Summary info for profile music step (port of `ProfileMusicInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileMusicInfo {
    /// Total number of saved profile audio tracks.
    pub count: i32,
}

/// Remote DC + TL file location descriptor (port of `Export::Data::FileLocation`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileLocation {
    /// Data center ID (`0` = empty/unavailable).
    pub dc_id: i32,
    /// TL input file location (`None` when unset/unavailable).
    pub data: Option<InputFileLocation>,
}

impl FileLocation {
    /// Returns true if a non-zero DC ID and location are set.
    pub const fn is_valid(&self) -> bool {
        self.dc_id != 0 && self.data.is_some()
    }
}

/// Refreshes a file reference when `to` and `from` refer to the same photo/document thumb
/// (port of `RefreshFileReference`).
pub fn refresh_file_reference(to: &mut FileLocation, from: &FileLocation) -> bool {
    if to.dc_id != from.dc_id {
        return false;
    }
    match (&to.data, &from.data) {
        (
            Some(InputFileLocation::InputPhotoFileLocation(InputPhotoFileLocation {
                id: id1,
                thumb_size: ts1,
                ..
            })),
            Some(InputFileLocation::InputPhotoFileLocation(InputPhotoFileLocation {
                id: id2,
                thumb_size: ts2,
                ..
            })),
        ) if id1 == id2 && ts1 == ts2 => {
            *to = from.clone();
            true
        }
        (
            Some(InputFileLocation::InputDocumentFileLocation(InputDocumentFileLocation {
                id: id1,
                thumb_size: ts1,
                ..
            })),
            Some(InputFileLocation::InputDocumentFileLocation(InputDocumentFileLocation {
                id: id2,
                thumb_size: ts2,
                ..
            })),
        ) if id1 == id2 && ts1 == ts2 => {
            *to = from.clone();
            true
        }
        _ => false,
    }
}

/// Reason why a file was not downloaded/written (port of `Export::Data::File::SkipReason`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// File was exported (or not skipped).
    #[default]
    None,
    /// File unavailable on server or expired.
    Unavailable,
    /// Excluded by `MediaSettings::types`.
    FileType,
    /// Exceeded `MediaSettings::size_limit`.
    FileSize,
    /// Message fell outside `single_peer_from..single_peer_till`.
    DateLimits,
}

/// Exportable file descriptor (port of `Export::Data::File`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct File {
    /// Remote download location.
    pub location: FileLocation,
    /// Total file size in bytes.
    pub size: i64,
    /// Inline/cached content (e.g. vCard or cached photo bytes).
    pub content: Vec<u8>,
    /// Suggested relative path before collision disambiguation.
    pub suggested_path: String,
    /// Final written relative path inside the export directory.
    pub relative_path: String,
    /// Reason if skipped.
    pub skip_reason: SkipReason,
}

/// Image dimensions + backing file (port of `Export::Data::Image`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Image file descriptor.
    pub file: File,
}

/// Contact entry (port of `Export::Data::ContactInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactInfo {
    /// User ID (0 for saved phone contacts without a Telegram user).
    pub user_id: UserId,
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Raw phone number.
    pub phone_number: String,
    /// Contact creation/sync timestamp.
    pub date: TimeId,
    /// Avatar color index.
    pub color_index: u8,
}

impl ContactInfo {
    /// Full display name (`first_name`, `last_name`, or `first_name + " " + last_name`).
    pub fn name(&self) -> String {
        if self.first_name.is_empty() {
            self.last_name.clone()
        } else if self.last_name.is_empty() {
            self.first_name.clone()
        } else {
            format!("{} {}", self.first_name, self.last_name)
        }
    }
}

/// Returns the contact's color index (port of `ContactColorIndex`).
pub const fn contact_color_index(data: &ContactInfo) -> u8 {
    data.color_index
}

/// Photo media item (port of `Export::Data::Photo`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Photo {
    /// Photo ID.
    pub id: u64,
    /// Upload timestamp.
    pub date: TimeId,
    /// Spoiler flag.
    pub spoilered: bool,
    /// Full-size image.
    pub image: Image,
}

/// Document / video / audio / sticker / voice media item (port of `Export::Data::Document`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    /// Document ID.
    pub id: u64,
    /// Upload timestamp.
    pub date: TimeId,
    /// Primary file.
    pub file: File,
    /// Optional thumbnail image.
    pub thumb: Image,
    /// Original filename.
    pub name: String,
    /// MIME type string.
    pub mime: String,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Sticker emoji alt text.
    pub sticker_emoji: String,
    /// Audio track performer.
    pub song_performer: String,
    /// Audio track title.
    pub song_title: String,
    /// Duration in seconds.
    pub duration: i32,
    /// Sticker flag.
    pub is_sticker: bool,
    /// Animated GIF/MP4 flag.
    pub is_animated: bool,
    /// Round video message flag.
    pub is_video_message: bool,
    /// Voice message flag.
    pub is_voice_message: bool,
    /// Video file flag.
    pub is_video_file: bool,
    /// Audio file flag.
    pub is_audio_file: bool,
    /// Spoiler flag.
    pub spoilered: bool,
}

/// Rich structured message (port of `Export::Data::RichMessage`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichMessage {
    /// Top-level blocks.
    pub blocks: Vec<RichBlock>,
    /// Embedded photos keyed by photo ID.
    pub photos: BTreeMap<u64, Photo>,
    /// Embedded documents keyed by document ID.
    pub documents: BTreeMap<u64, Document>,
    /// Right-to-left layout flag.
    pub rtl: bool,
    /// Partial message flag.
    pub part: bool,
}

/// Shared contact attachment (port of `Export::Data::SharedContact`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedContact {
    /// Contact fields.
    pub info: ContactInfo,
    /// Optional vCard file.
    pub vcard: File,
}

/// Geographic point attachment (port of `Export::Data::GeoPoint`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GeoPoint {
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Whether coordinates are valid.
    pub valid: bool,
}

/// Venue attachment (port of `Export::Data::Venue`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Venue {
    /// Venue coordinates.
    pub point: GeoPoint,
    /// Venue title.
    pub title: String,
    /// Venue street address.
    pub address: String,
}

/// Inline game attachment (port of `Export::Data::Game`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Game {
    /// Game ID.
    pub id: u64,
    /// Game short name.
    pub short_name: String,
    /// Game title.
    pub title: String,
    /// Game description.
    pub description: String,
    /// Owning bot user ID.
    pub bot_id: UserId,
}

/// Payment invoice attachment (port of `Export::Data::Invoice`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Invoice {
    /// Invoice title.
    pub title: String,
    /// Invoice description.
    pub description: String,
    /// ISO currency code.
    pub currency: String,
    /// Amount in smallest currency units.
    pub amount: u64,
    /// Receipt message ID if paid.
    pub receipt_msg_id: i32,
}

/// Paid media attachment (port of `Export::Data::PaidMedia`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaidMedia {
    /// Price in Telegram Stars.
    pub stars: u64,
    /// Optional unlocked nested media items.
    pub extended: Vec<Option<Media>>,
}

/// Poll answer choice (port of `Export::Data::Poll::Answer`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollAnswer {
    /// Answer label text parts.
    pub text: Vec<TextPart>,
    /// Opaque option bytes.
    pub option: Vec<u8>,
    /// Vote count for this option.
    pub votes: i32,
    /// Whether the exporting user voted for this option.
    pub my: bool,
}

/// Poll attachment (port of `Export::Data::Poll`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Poll {
    /// Poll ID.
    pub id: u64,
    /// Poll question text parts.
    pub question: Vec<TextPart>,
    /// Poll answers.
    pub answers: Vec<PollAnswer>,
    /// Total votes across all answers.
    pub total_votes: i32,
    /// Whether the poll is closed.
    pub closed: bool,
}

/// Single task inside a `TodoList` (port of `Export::Data::TodoListItem`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoListItem {
    /// Task text parts.
    pub text: Vec<TextPart>,
    /// Task identifier.
    pub id: i32,
}

/// Checklist/todo-list attachment (port of `Export::Data::TodoList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoList {
    /// Whether other chat members can append tasks.
    pub others_can_append: bool,
    /// Whether other chat members can complete tasks.
    pub others_can_complete: bool,
    /// List title text parts.
    pub title: Vec<TextPart>,
    /// Task items.
    pub items: Vec<TodoListItem>,
}

/// Giveaway announcement attachment (port of `Export::Data::GiveawayStart`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GiveawayStart {
    /// Eligible ISO-3166-1 alpha-2 country codes.
    pub countries: Vec<String>,
    /// Sponsoring channel IDs.
    pub channels: Vec<ChannelId>,
    /// Optional extra prize description.
    pub additional_prize: String,
    /// Selection timestamp.
    pub until_date: TimeId,
    /// Stars prize pool (0 for Premium giveaway).
    pub credits: u64,
    /// Number of winners/subscriptions.
    pub quantity: i32,
    /// Subscription duration in months.
    pub months: i32,
    /// True if all subscribers are eligible (not only new ones).
    pub all: bool,
}

/// Giveaway results attachment (port of `Export::Data::GiveawayResults`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GiveawayResults {
    /// Host channel ID.
    pub channel: ChannelId,
    /// Winning peer IDs.
    pub winners: Vec<PeerId>,
    /// Optional additional prize description.
    pub additional_prize: String,
    /// End timestamp.
    pub until_date: TimeId,
    /// Launch message ID.
    pub launch_id: i32,
    /// Number of additional sponsoring peers.
    pub additional_peers_count: i32,
    /// Total winners count.
    pub winners_count: i32,
    /// Unclaimed prizes count.
    pub unclaimed_count: i32,
    /// Subscription months.
    pub months: i32,
    /// Stars prize pool.
    pub credits: u64,
    /// Refunded flag.
    pub refunded: bool,
    /// All subscribers eligible flag.
    pub all: bool,
}

/// Marker for unsupported message media (port of `Export::Data::UnsupportedMedia`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnsupportedMedia;

/// Tagged media attachment union (port of `Export::Data::Media::content`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum MediaContent {
    /// No attachment.
    #[default]
    None,
    /// Photo attachment.
    Photo(Photo),
    /// Document/video/audio/sticker/voice attachment.
    Document(Box<Document>),
    /// Shared contact attachment.
    SharedContact(SharedContact),
    /// Location pin or live location.
    GeoPoint(GeoPoint),
    /// Named venue.
    Venue(Venue),
    /// Inline game.
    Game(Game),
    /// Payment invoice.
    Invoice(Invoice),
    /// Poll.
    Poll(Poll),
    /// To-do checklist.
    TodoList(TodoList),
    /// Giveaway launch.
    GiveawayStart(GiveawayStart),
    /// Giveaway results.
    GiveawayResults(GiveawayResults),
    /// Paid media.
    PaidMedia(PaidMedia),
    /// Unsupported media placeholder.
    Unsupported(UnsupportedMedia),
}

/// Message or story media wrapper (port of `Export::Data::Media`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    /// Attachment payload.
    pub content: MediaContent,
    /// Self-destruct or live-location TTL in seconds (`0` = none).
    pub ttl: TimeId,
}

impl Media {
    /// Returns a shared reference to the primary file if this media carries one.
    pub fn file(&self) -> Option<&File> {
        match &self.content {
            MediaContent::Photo(p) => Some(&p.image.file),
            MediaContent::Document(d) => Some(&d.file),
            MediaContent::SharedContact(c) => Some(&c.vcard),
            _ => None,
        }
    }

    /// Returns a mutable reference to the primary file if this media carries one.
    pub fn file_mut(&mut self) -> Option<&mut File> {
        match &mut self.content {
            MediaContent::Photo(p) => Some(&mut p.image.file),
            MediaContent::Document(d) => Some(&mut d.file),
            MediaContent::SharedContact(c) => Some(&mut c.vcard),
            _ => None,
        }
    }

    /// Returns a shared reference to the thumbnail image if this media is a document.
    pub fn thumb(&self) -> Option<&Image> {
        match &self.content {
            MediaContent::Document(d) => Some(&d.thumb),
            _ => None,
        }
    }

    /// Returns a mutable reference to the thumbnail image if this media is a document.
    pub fn thumb_mut(&mut self) -> Option<&mut Image> {
        match &mut self.content {
            MediaContent::Document(d) => Some(&mut d.thumb),
            _ => None,
        }
    }
}

/// Batch of exported profile pictures (port of `UserpicsSlice`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserpicsSlice {
    /// Photos in this slice.
    pub list: Vec<Photo>,
}

/// Telegram user record (port of `Export::Data::User`).
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    /// Bare user ID.
    pub bare_id: BareId,
    /// Contact info (name, phone, user_id, color_index).
    pub info: ContactInfo,
    /// Public username without leading `@`.
    pub username: String,
    /// Avatar color index.
    pub color_index: u8,
    /// Bot flag.
    pub is_bot: bool,
    /// Current account flag.
    pub is_self: bool,
    /// Replies service account (`1271266957`).
    pub is_replies: bool,
    /// Verification codes service account (`489000`).
    pub is_verify_codes: bool,
    /// Input user reference.
    pub input: InputUser,
}

impl Default for User {
    fn default() -> Self {
        Self {
            bare_id: 0,
            info: ContactInfo::default(),
            username: String::new(),
            color_index: 0,
            is_bot: false,
            is_self: false,
            is_replies: false,
            is_verify_codes: false,
            input: InputUser::Empty,
        }
    }
}

impl User {
    /// Returns this user's `PeerId`.
    pub const fn id(&self) -> PeerId {
        PeerId::from_user(UserId(self.bare_id))
    }

    /// Returns this user's display name.
    pub fn name(&self) -> String {
        self.info.name()
    }
}

/// Telegram group/supergroup/channel record (port of `Export::Data::Chat`).
#[derive(Debug, Clone, PartialEq)]
pub struct Chat {
    /// Bare chat or channel ID.
    pub bare_id: BareId,
    /// Channel ID if this basic chat migrated to a supergroup.
    pub migrated_to_channel_id: ChannelId,
    /// Chat or channel title.
    pub title: String,
    /// Public username without leading `@`.
    pub username: String,
    /// Avatar color index.
    pub color_index: u8,
    /// Monoforum direct-messages flag.
    pub is_monoforum: bool,
    /// Broadcast channel flag.
    pub is_broadcast: bool,
    /// Supergroup/megagroup flag.
    pub is_supergroup: bool,
    /// Admin of linked monoforum flag.
    pub is_monoforum_admin: bool,
    /// Has manage-direct-messages admin rights.
    pub has_monoforum_admin_rights: bool,
    /// Linked broadcast channel has a public username.
    pub is_monoforum_of_public_broadcast: bool,
    /// Linked monoforum channel bare ID.
    pub monoforum_link_id: BareId,
    /// Input peer reference.
    pub input: InputPeer,
    /// Linked broadcast input peer for monoforum admin.
    pub monoforum_broadcast_input: InputPeer,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            bare_id: 0,
            migrated_to_channel_id: ChannelId(0),
            title: String::new(),
            username: String::new(),
            color_index: 0,
            is_monoforum: false,
            is_broadcast: false,
            is_supergroup: false,
            is_monoforum_admin: false,
            has_monoforum_admin_rights: false,
            is_monoforum_of_public_broadcast: false,
            monoforum_link_id: 0,
            input: InputPeer::Empty,
            monoforum_broadcast_input: InputPeer::Empty,
        }
    }
}

impl Chat {
    /// Returns this chat's `PeerId`.
    pub const fn id(&self) -> PeerId {
        if self.is_broadcast || self.is_supergroup {
            PeerId::from_channel(ChannelId(self.bare_id))
        } else {
            PeerId::from_chat(ChatId(self.bare_id))
        }
    }
}

/// Peer union (port of `Export::Data::Peer`).
#[derive(Debug, Clone, PartialEq)]
pub enum Peer {
    /// User peer.
    User(User),
    /// Chat/channel peer.
    Chat(Chat),
}

impl Default for Peer {
    fn default() -> Self {
        Self::User(User::default())
    }
}

impl Peer {
    /// Returns this peer's `PeerId`.
    pub fn id(&self) -> PeerId {
        match self {
            Self::User(u) => PeerId::from_user(u.info.user_id),
            Self::Chat(c) => c.id(),
        }
    }

    /// Returns this peer's display name.
    pub fn name(&self) -> String {
        match self {
            Self::User(u) => u.name(),
            Self::Chat(c) => c.title.clone(),
        }
    }

    /// Returns the `InputPeer` for API requests.
    pub fn input(&self) -> InputPeer {
        match self {
            Self::User(u) => match &u.input {
                InputUser::InputUser(TlInputUser {
                    user_id,
                    access_hash,
                }) => InputPeer::User(InputPeerUser {
                    user_id: *user_id,
                    access_hash: *access_hash,
                }),
                InputUser::Self_ => InputPeer::Self_,
                InputUser::Empty | InputUser::FromMessage(_) => InputPeer::Empty,
            },
            Self::Chat(c) => c.input.clone(),
        }
    }

    /// Returns the avatar color index.
    pub const fn color_index(&self) -> u8 {
        match self {
            Self::User(u) => u.color_index,
            Self::Chat(c) => c.color_index,
        }
    }

    /// Returns `Some(&User)` if this peer is a user.
    pub const fn as_user(&self) -> Option<&User> {
        match self {
            Self::User(u) => Some(u),
            Self::Chat(_) => None,
        }
    }

    /// Returns `Some(&Chat)` if this peer is a chat or channel.
    pub const fn as_chat(&self) -> Option<&Chat> {
        match self {
            Self::Chat(c) => Some(c),
            Self::User(_) => None,
        }
    }
}

/// Account personal information (port of `Export::Data::PersonalInfo`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonalInfo {
    /// Account user record.
    pub user: User,
    /// Account bio text.
    pub bio: String,
}

/// Frequent-contact top peer entry (port of `Export::Data::TopPeer`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopPeer {
    /// Target peer.
    pub peer: Peer,
    /// Interaction rating score.
    pub rating: f64,
}

/// Saved contacts + frequent contacts lists (port of `Export::Data::ContactsList`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactsList {
    /// Saved contacts.
    pub list: Vec<ContactInfo>,
    /// Frequent correspondents (`"people"`).
    pub correspondents: Vec<TopPeer>,
    /// Frequent inline bots (`"inline_bots"` / `"inline bots"`).
    pub inline_bots: Vec<TopPeer>,
    /// Frequent call peers (`"calls"`).
    pub phone_calls: Vec<TopPeer>,
}

/// Returns contact indices sorted case-insensitively by `"first_name last_name"`
/// (port of `SortedContactsIndices`).
pub fn sorted_contacts_indices(data: &ContactsList) -> Vec<usize> {
    let names: Vec<String> = data
        .list
        .iter()
        .map(|info| format!("{} {}", info.first_name, info.last_name).to_lowercase())
        .collect();
    let mut indices: Vec<usize> = (0..data.list.len()).collect();
    indices.sort_by(|&i, &j| names[i].cmp(&names[j]));
    indices
}

/// Active Telegram client authorization (port of `Export::Data::Session`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    /// Telegram API app ID.
    pub application_id: i32,
    /// OS / platform name.
    pub platform: String,
    /// Device model string.
    pub device_model: String,
    /// OS version string.
    pub system_version: String,
    /// Application name.
    pub application_name: String,
    /// Application version.
    pub application_version: String,
    /// Creation timestamp.
    pub created: TimeId,
    /// Last active timestamp.
    pub last_active: TimeId,
    /// Last IP address.
    pub ip: String,
    /// Country name.
    pub country: String,
    /// Region/city name.
    pub region: String,
}

/// Active website Telegram Login authorization (port of `Export::Data::WebSession`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSession {
    /// Bot username without `@`.
    pub bot_username: String,
    /// Website domain name.
    pub domain: String,
    /// Browser name.
    pub browser: String,
    /// Platform name.
    pub platform: String,
    /// Creation timestamp.
    pub created: TimeId,
    /// Last active timestamp.
    pub last_active: TimeId,
    /// Last IP address.
    pub ip: String,
    /// Region name.
    pub region: String,
}

/// Active sessions and web sessions (port of `Export::Data::SessionsList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionsList {
    /// Client sessions.
    pub list: Vec<Session>,
    /// Web login sessions.
    pub web_list: Vec<WebSession>,
}

/// Phone call discard / conference state (port of `ActionPhoneCall::State`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PhoneCallState {
    /// Unknown/unspecified.
    #[default]
    Unknown,
    /// Missed call.
    Missed,
    /// Disconnected.
    Disconnect,
    /// Normal hangup.
    Hangup,
    /// Line busy / declined.
    Busy,
    /// Migrated to conference call.
    MigrateConferenceCall,
    /// Conference call invitation.
    Invitation,
    /// Active conference call.
    Active,
}

/// Telegram Passport value type (port of `ActionSecureValuesSent::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecureValueType {
    /// Personal details.
    PersonalDetails,
    /// Passport.
    Passport,
    /// Driver license.
    DriverLicense,
    /// Identity card.
    IdentityCard,
    /// Internal passport.
    InternalPassport,
    /// Address information.
    Address,
    /// Utility bill.
    UtilityBill,
    /// Bank statement.
    BankStatement,
    /// Rental agreement.
    RentalAgreement,
    /// Passport registration.
    PassportRegistration,
    /// Temporary registration.
    TemporaryRegistration,
    /// Phone number.
    Phone,
    /// Email address.
    Email,
}

/// Service message action payload (port of `Export::Data::ServiceAction` and all 62 `Action*` structs).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ServiceAction {
    /// Regular (non-service) message.
    #[default]
    None,
    /// Group created.
    ChatCreate {
        /// Group title.
        title: String,
        /// Initial member user IDs.
        user_ids: Vec<UserId>,
    },
    /// Group or channel title edited.
    ChatEditTitle {
        /// New title.
        title: String,
    },
    /// Group or channel photo edited.
    ChatEditPhoto {
        /// New photo.
        photo: Photo,
    },
    /// Group or channel photo removed.
    ChatDeletePhoto,
    /// Members invited to group.
    ChatAddUser {
        /// Invited user IDs.
        user_ids: Vec<UserId>,
    },
    /// Member removed or left group.
    ChatDeleteUser {
        /// Removed user ID.
        user_id: UserId,
    },
    /// User joined group via invite link.
    ChatJoinedByLink {
        /// Inviter user ID.
        inviter_id: UserId,
    },
    /// Channel created.
    ChannelCreate {
        /// Channel title.
        title: String,
    },
    /// Basic group migrated to supergroup.
    ChatMigrateTo {
        /// Target supergroup channel ID.
        channel_id: ChannelId,
    },
    /// Supergroup migrated from basic group.
    ChannelMigrateFrom {
        /// Original basic group title.
        title: String,
        /// Original basic group chat ID.
        chat_id: ChatId,
    },
    /// Message pinned.
    PinMessage,
    /// Chat history cleared.
    HistoryClear,
    /// Game high score posted.
    GameScore {
        /// Game ID.
        game_id: u64,
        /// Score value.
        score: i32,
    },
    /// Payment sent.
    PaymentSent {
        /// ISO currency code.
        currency: String,
        /// Amount in smallest currency units.
        amount: u64,
        /// Initialized recurring payments.
        recurring_init: bool,
        /// Charged via recurring payment.
        recurring_used: bool,
    },
    /// Voice/video or conference call.
    PhoneCall {
        /// Conference call ID (`0` for 1-on-1 call).
        conference_id: u64,
        /// Call discard or conference state.
        state: PhoneCallState,
        /// Duration in seconds.
        duration: i32,
    },
    /// Screenshot taken in secret/protected chat.
    ScreenshotTaken,
    /// Custom server service message text.
    CustomAction {
        /// Message text.
        message: String,
    },
    /// User allowed bot to send messages.
    BotAllowed {
        /// Bot app ID.
        app_id: u64,
        /// Bot app title.
        app: String,
        /// Login domain.
        domain: String,
        /// Allowed via attachment menu.
        attach_menu: bool,
        /// Allowed via web-app request.
        from_request: bool,
    },
    /// Telegram Passport values sent.
    SecureValuesSent {
        /// Sent document types.
        types: Vec<SecureValueType>,
    },
    /// Contact joined Telegram.
    ContactSignUp,
    /// Phone number requested.
    PhoneNumberRequest,
    /// Live location proximity alert triggered.
    GeoProximityReached {
        /// Moving peer.
        from_id: PeerId,
        /// Target peer.
        to_id: PeerId,
        /// Distance in meters.
        distance: i32,
        /// Moving peer is self.
        from_self: bool,
        /// Target peer is self.
        to_self: bool,
    },
    /// Group voice chat started/ended.
    GroupCall {
        /// Duration in seconds (0 if just started).
        duration: i32,
    },
    /// Users invited to group voice chat.
    InviteToGroupCall {
        /// Invited user IDs.
        user_ids: Vec<UserId>,
    },
    /// Auto-delete TTL changed.
    SetMessagesTtl {
        /// Auto-delete period in seconds (0 = disabled).
        period: TimeId,
    },
    /// Group voice chat scheduled.
    GroupCallScheduled {
        /// Scheduled timestamp.
        date: TimeId,
    },
    /// Chat theme changed or disabled.
    SetChatTheme {
        /// Theme emoji (empty = disabled).
        emoji: String,
    },
    /// User joined group by approval request.
    ChatJoinedByRequest,
    /// User joined group via community.
    ChatJoinedViaCommunity {
        /// Community channel ID.
        community_id: ChannelId,
    },
    /// WebView data sent to bot.
    WebViewDataSent {
        /// Button text.
        text: String,
    },
    /// Telegram Premium gifted.
    GiftPremium {
        /// Formatted cost string.
        cost: String,
        /// Number of days.
        days: i32,
    },
    /// Forum topic created.
    TopicCreate {
        /// Topic title.
        title: String,
    },
    /// Forum topic edited.
    TopicEdit {
        /// New title (empty if unchanged).
        title: String,
        /// New icon custom emoji ID if changed.
        icon_emoji_id: Option<u64>,
    },
    /// Suggested profile photo.
    SuggestProfilePhoto {
        /// Suggested photo.
        photo: Photo,
    },
    /// Peer selected for bot request.
    RequestedPeer {
        /// Selected peer IDs.
        peers: Vec<PeerId>,
        /// Request button ID.
        button_id: i32,
    },
    /// Chat wallpaper changed.
    SetChatWallPaper {
        /// Set the same wallpaper as the other side.
        same: bool,
        /// Applied for both sides.
        both: bool,
    },
    /// Premium gift code received or won.
    GiftCode {
        /// Gift code slug.
        code: String,
        /// Sponsoring channel peer ID.
        boost_peer_id: PeerId,
        /// Duration in days.
        days: i32,
        /// Won via giveaway.
        via_giveaway: bool,
        /// Unclaimed flag.
        unclaimed: bool,
    },
    /// Giveaway launched.
    GiveawayLaunch,
    /// Giveaway results summary action.
    GiveawayResults {
        /// Number of winners.
        winners: i32,
        /// Number of unclaimed prizes.
        unclaimed: i32,
        /// Stars giveaway flag.
        credits: bool,
    },
    /// Channel/group boosted.
    BoostApply {
        /// Number of boosts applied.
        boosts: i32,
    },
    /// Payment refunded.
    PaymentRefunded {
        /// Peer who issued refund.
        peer_id: PeerId,
        /// ISO currency code.
        currency: String,
        /// Refunded amount.
        amount: u64,
        /// Charge/transaction ID.
        transaction_id: String,
    },
    /// Stars or TON gifted.
    GiftCredits {
        /// Formatted cost string.
        cost: String,
        /// Gifted amount.
        amount: CreditsAmount,
    },
    /// Stars giveaway prize won.
    PrizeStars {
        /// Sponsoring channel peer ID.
        peer_id: PeerId,
        /// Stars amount.
        amount: u64,
        /// Transaction ID.
        transaction_id: String,
        /// Giveaway message ID.
        giveaway_msg_id: i32,
        /// Unclaimed flag.
        is_unclaimed: bool,
    },
    /// Star gift or purchase offer.
    StarGift {
        /// Gift ID.
        gift_id: u64,
        /// Star value.
        stars: i64,
        /// Gift caption text parts.
        text: Vec<TextPart>,
        /// Anonymous sender flag.
        anonymous: bool,
        /// Limited edition flag.
        limited: bool,
        /// Purchase offer price.
        offer_price: CreditsAmount,
        /// Purchase offer expiration timestamp.
        offer_expire_at: TimeId,
        /// Is purchase offer.
        offer: bool,
        /// Offer accepted flag.
        offer_accepted: bool,
        /// Offer declined flag.
        offer_declined: bool,
        /// Offer expired flag.
        offer_expired: bool,
    },
    /// Paid messages refunded.
    PaidMessagesRefunded {
        /// Number of messages refunded.
        messages: i32,
        /// Stars refunded.
        stars: i64,
    },
    /// Paid messages price changed.
    PaidMessagesPrice {
        /// Price per message in Stars.
        stars: i32,
        /// Direct messages allowed flag (channels).
        broadcast_allowed: bool,
    },
    /// To-do list items marked done/undone.
    TodoCompletions {
        /// Item IDs marked completed.
        completed: Vec<i32>,
        /// Item IDs marked incompleted.
        incompleted: Vec<i32>,
    },
    /// Tasks appended to a to-do list.
    TodoAppendTasks {
        /// Added tasks.
        items: Vec<TodoListItem>,
    },
    /// Option appended to a poll.
    PollAppendAnswer {
        /// Option text.
        option: String,
    },
    /// Option deleted from a poll.
    PollDeleteAnswer {
        /// Option text.
        option: String,
    },
    /// Suggested post approved or rejected.
    SuggestedPostApproval {
        /// Optional rejection comment.
        reject_comment: String,
        /// Scheduled publish timestamp.
        schedule_date: TimeId,
        /// Price in Stars or TON.
        price: CreditsAmount,
        /// Rejected flag.
        rejected: bool,
        /// Balance too low flag.
        balance_too_low: bool,
    },
    /// Paid suggested post payout completed.
    SuggestedPostSuccess {
        /// Transferred price.
        price: CreditsAmount,
    },
    /// Paid suggested post refunded.
    SuggestedPostRefund {
        /// Whether initiated by the payer.
        payer_initiated: bool,
    },
    /// Birthday suggestion.
    SuggestBirthday {
        /// Suggested birthday.
        birthday: Birthday,
    },
    /// Content sharing toggled.
    NoForwardsToggle {
        /// New no-forwards value (`true` = disabled sharing).
        new_value: bool,
    },
    /// Content sharing toggle requested.
    NoForwardsRequest {
        /// Request expired flag.
        expired: bool,
        /// Requested no-forwards value.
        new_value: bool,
    },
    /// Creator ownership transfer pending.
    NewCreatorPending {
        /// Future creator user ID.
        new_creator_id: UserId,
    },
    /// Creator ownership transferred.
    ChangeCreator {
        /// New creator user ID.
        new_creator_id: UserId,
    },
    /// Managed bot created.
    ManagedBotCreated {
        /// Created bot user ID.
        bot_id: UserId,
    },
    /// Gram/TON transfer.
    GramTransfer {
        /// Amount in nano-Grams.
        amount: i64,
        /// Peer wallet address.
        peer_address: String,
        /// Transaction ID.
        transaction_id: String,
        /// Plain or base64 encrypted comment.
        comment: String,
        /// Whether comment is encrypted.
        comment_encrypted: bool,
    },
    /// TON Connect wallet request.
    WalletTonConnectRequest {
        /// Session ID.
        session_id: u64,
        /// Expiry timestamp.
        expires: TimeId,
        /// Request topic.
        topic: String,
        /// Trace ID.
        trace_id: String,
        /// Accepted flag.
        accepted: bool,
        /// Declined flag.
        declined: bool,
    },
    /// Free messages link token used.
    NoPaidMessagesTokenUsed,
}

impl ServiceAction {
    /// Returns true if this is `ServiceAction::None`.
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// Message reaction kind (port of `Export::Data::Reaction::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ReactionType {
    /// Empty reaction.
    #[default]
    Empty,
    /// Standard Unicode emoji reaction.
    Emoji,
    /// Custom emoji document reaction.
    CustomEmoji,
    /// Paid star reaction.
    Paid,
}

impl ReactionType {
    /// Wire/export string representation (port of `Reaction::TypeToString`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Emoji => "emoji",
            Self::CustomEmoji => "custom_emoji",
            Self::Paid => "paid",
        }
    }
}

/// Individual peer entry in a reaction's recent-reactors list (port of `Reaction::Recent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReactionRecent {
    /// Reactor peer ID.
    pub peer_id: PeerId,
    /// Reaction timestamp.
    pub date: TimeId,
}

/// Reaction counter on a message (port of `Export::Data::Reaction`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reaction {
    /// Reaction kind.
    pub kind: ReactionType,
    /// Emoji string (for `Emoji`).
    pub emoji: String,
    /// Custom emoji document ID or resolved relative path (for `CustomEmoji`).
    pub document_id: String,
    /// Total reaction count.
    pub count: u32,
    /// Recent reactors.
    pub recent: Vec<ReactionRecent>,
}

impl Reaction {
    /// Unique key string (port of `Reaction::Id`).
    pub fn id_key(&self) -> String {
        let suffix = match self.kind {
            ReactionType::Emoji => self.emoji.as_str(),
            ReactionType::CustomEmoji => self.document_id.as_str(),
            ReactionType::Empty | ReactionType::Paid => "",
        };
        format!("{}{}", self.kind.as_str(), suffix)
    }
}

/// Composite channel + message identifier (port of `Export::Data::MessageId`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId {
    /// Channel bare ID (0 for basic chats / personal chats).
    pub channel_id: ChannelId,
    /// Message ID.
    pub msg_id: i32,
}

/// Inline keyboard button kind (port of `HistoryMessageMarkupButton::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MarkupButtonType {
    /// Default reply button.
    #[default]
    Default,
    /// URL button.
    Url,
    /// Callback button.
    Callback,
    /// Callback button requiring password.
    CallbackWithPassword,
    /// Request phone number.
    RequestPhone,
    /// Request location.
    RequestLocation,
    /// Request poll.
    RequestPoll,
    /// Request peer.
    RequestPeer,
    /// Switch inline query.
    SwitchInline,
    /// Switch inline query in current chat.
    SwitchInlineSame,
    /// Game button.
    Game,
    /// Payment button.
    Buy,
    /// Login URL auth button.
    Auth,
    /// User profile link button.
    UserProfile,
    /// WebView button.
    WebView,
    /// Simple WebView button.
    SimpleWebView,
    /// Copy text button.
    CopyText,
    /// Disabled button.
    Disabled,
}

/// Inline keyboard button attached to a message (port of `HistoryMessageMarkupButton`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryMessageMarkupButton {
    /// Button kind.
    pub kind: MarkupButtonType,
    /// Visible label.
    pub text: String,
    /// Raw data bytes (URL, callback payload, query, user ID, copy text).
    pub data: Vec<u8>,
    /// Forward text for `Auth` buttons.
    pub forward_text: String,
    /// Button ID for `Auth` buttons.
    pub button_id: i64,
}

impl HistoryMessageMarkupButton {
    /// Export string representation of `self.kind` (port of `HistoryMessageMarkupButton::TypeToString`).
    pub const fn type_str(&self) -> &'static str {
        match self.kind {
            MarkupButtonType::Default => "default",
            MarkupButtonType::Url => "url",
            MarkupButtonType::Callback => "callback",
            MarkupButtonType::CallbackWithPassword => "callback_with_password",
            MarkupButtonType::RequestPhone => "request_phone",
            MarkupButtonType::RequestLocation => "request_location",
            MarkupButtonType::RequestPoll => "request_poll",
            MarkupButtonType::RequestPeer => "request_peer",
            MarkupButtonType::SwitchInline => "switch_inline",
            MarkupButtonType::SwitchInlineSame => "switch_inline_same",
            MarkupButtonType::Game => "game",
            MarkupButtonType::Buy => "buy",
            MarkupButtonType::Auth => "auth",
            MarkupButtonType::UserProfile => "user_profile",
            MarkupButtonType::WebView => "web_view",
            MarkupButtonType::SimpleWebView => "simple_web_view",
            MarkupButtonType::CopyText => "copy_text",
            MarkupButtonType::Disabled => "disabled",
        }
    }
}

/// Exported chat message (port of `Export::Data::Message`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    /// Message ID (shifted by `MIGRATED_MESSAGES_ID_SHIFT` for migrated basic-group slices).
    pub id: i32,
    /// Creation timestamp.
    pub date: TimeId,
    /// Last edit timestamp (`0` = never edited).
    pub edited: TimeId,
    /// Sender peer ID.
    pub from_id: PeerId,
    /// Chat peer ID.
    pub peer_id: PeerId,
    /// Exporting user's own peer ID.
    pub self_id: PeerId,
    /// Original sender peer ID when forwarded.
    pub forwarded_from_id: PeerId,
    /// Hidden forward sender display name.
    pub forwarded_from_name: String,
    /// Original message timestamp when forwarded.
    pub forwarded_date: TimeId,
    /// Whether this message was forwarded.
    pub forwarded: bool,
    /// Display forwarded author as primary sender (e.g. Saved Messages).
    pub show_forwarded_as_original: bool,
    /// Saved-from peer ID.
    pub saved_from_chat_id: PeerId,
    /// Channel post author signature.
    pub signature: String,
    /// Inline bot user ID if sent via inline bot.
    pub via_bot_id: UserId,
    /// Replied-to message ID (`0` = not a reply).
    pub reply_to_msg_id: i32,
    /// Replied-to peer ID when replying across chats (`PeerId(0)` = same chat).
    pub reply_to_peer_id: PeerId,
    /// Formatted message text parts.
    pub text: Vec<TextPart>,
    /// Message reactions.
    pub reactions: Vec<Reaction>,
    /// Media attachment.
    pub media: Media,
    /// Service action (`ServiceAction::None` for regular messages).
    pub action: ServiceAction,
    /// Outgoing message flag.
    pub out: bool,
    /// Inline bot keyboard rows.
    pub inline_button_rows: Vec<Vec<HistoryMessageMarkupButton>>,
    /// Optional rich structured message.
    pub rich_message: Option<RichMessage>,
}

impl Message {
    /// Returns the primary file attached to this message (action photo takes precedence over media).
    pub fn file(&self) -> Option<&File> {
        match &self.action {
            ServiceAction::ChatEditPhoto { photo } => Some(&photo.image.file),
            ServiceAction::SuggestProfilePhoto { photo } => Some(&photo.image.file),
            _ => self.media.file(),
        }
    }

    /// Returns a mutable reference to the primary file attached to this message.
    pub fn file_mut(&mut self) -> Option<&mut File> {
        match &mut self.action {
            ServiceAction::ChatEditPhoto { photo } => Some(&mut photo.image.file),
            ServiceAction::SuggestProfilePhoto { photo } => Some(&mut photo.image.file),
            _ => self.media.file_mut(),
        }
    }

    /// Returns the thumbnail image if this message carries a document attachment.
    pub fn thumb(&self) -> Option<&Image> {
        self.media.thumb()
    }

    /// Returns a mutable reference to the thumbnail image.
    pub fn thumb_mut(&mut self) -> Option<&mut Image> {
        self.media.thumb_mut()
    }
}

/// Origin context for refreshing expired file references (port of `Export::Data::FileOrigin`).
#[derive(Debug, Clone, PartialEq)]
pub struct FileOrigin {
    /// Takeout split index.
    pub split: i32,
    /// Peer containing the message.
    pub peer: InputPeer,
    /// Message ID.
    pub message_id: i32,
    /// Story ID.
    pub story_id: i32,
    /// Custom emoji document ID.
    pub custom_emoji_id: u64,
    /// Whether the file originates from a `RichMessage`.
    pub rich_message: bool,
}

impl Default for FileOrigin {
    fn default() -> Self {
        Self {
            split: 0,
            peer: InputPeer::Empty,
            message_id: 0,
            story_id: 0,
            custom_emoji_id: 0,
            rich_message: false,
        }
    }
}

/// Archived story record (port of `Export::Data::Story`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Story {
    /// Story ID.
    pub id: i32,
    /// Creation timestamp.
    pub date: TimeId,
    /// Expiration timestamp.
    pub expires: TimeId,
    /// Story photo or video media.
    pub media: Media,
    /// Pinned to profile flag.
    pub pinned: bool,
    /// Formatted caption text parts.
    pub caption: Vec<TextPart>,
}

impl Story {
    /// Returns the primary media file for this story.
    pub fn file(&self) -> Option<&File> {
        self.media.file()
    }

    /// Returns the thumbnail image for this story if it is a video.
    pub fn thumb(&self) -> Option<&Image> {
        self.media.thumb()
    }
}

/// Batch of exported stories (port of `StoriesSlice`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoriesSlice {
    /// Stories in this slice.
    pub list: Vec<Story>,
    /// Last story ID processed.
    pub last_id: i32,
    /// Number of unsupported/skipped stories in this slice.
    pub skipped: i32,
}

/// Batch of exported profile music items (port of `ProfileMusicSlice`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileMusicSlice {
    /// Synthetic audio messages representing profile music tracks.
    pub list: Vec<Message>,
    /// Number of skipped entries.
    pub skipped: i32,
}

/// Classification of an exported dialog (port of `Export::Data::DialogInfo::Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DialogType {
    /// Unknown dialog kind.
    #[default]
    Unknown,
    /// Saved Messages (`is_self`).
    SelfChat,
    /// Replies bot chat (`1271266957`).
    Replies,
    /// Verification codes chat (`489000`).
    VerifyCodes,
    /// Personal 1-on-1 chat.
    Personal,
    /// Bot chat.
    Bot,
    /// Legacy basic group.
    PrivateGroup,
    /// Private supergroup.
    PrivateSupergroup,
    /// Public supergroup.
    PublicSupergroup,
    /// Private channel.
    PrivateChannel,
    /// Public channel.
    PublicChannel,
}

/// Metadata for one dialog to be exported (port of `Export::Data::DialogInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct DialogInfo {
    /// Dialog classification.
    pub kind: DialogType,
    /// Primary title or user first name.
    pub name: String,
    /// User last name (for personal/bot chats).
    pub last_name: String,
    /// API input peer.
    pub input: InputPeer,
    /// Top message ID.
    pub top_message_id: i32,
    /// Top message timestamp.
    pub top_message_date: TimeId,
    /// Peer ID.
    pub peer_id: PeerId,
    /// Avatar color index.
    pub color_index: u8,
    /// Input peer of basic chat migrated into this supergroup.
    pub migrated_from_input: InputPeer,
    /// Target supergroup channel ID if this basic chat migrated.
    pub migrated_to_channel_id: ChannelId,
    /// Linked broadcast input peer for monoforum admin.
    pub monoforum_broadcast_input: InputPeer,
    /// Takeout split indices containing messages for this dialog.
    pub splits: Vec<i32>,
    /// True if exporting only outgoing messages (e.g. public groups/channels).
    pub only_my_messages: bool,
    /// True if this is a left/banned channel or supergroup.
    pub is_left_channel: bool,
    /// True if this is a monoforum.
    pub is_monoforum: bool,
    /// Relative folder inside the export directory (e.g. `"chats/chat_01/"`, empty for single-peer).
    pub relative_path: String,
    /// Expected message counts per split.
    pub messages_count_per_split: Vec<i32>,
}

impl Default for DialogInfo {
    fn default() -> Self {
        Self {
            kind: DialogType::default(),
            name: String::new(),
            last_name: String::new(),
            input: InputPeer::Empty,
            top_message_id: 0,
            top_message_date: 0,
            peer_id: PeerId::ZERO,
            color_index: 0,
            migrated_from_input: InputPeer::Empty,
            migrated_to_channel_id: ChannelId(0),
            monoforum_broadcast_input: InputPeer::Empty,
            splits: Vec::new(),
            only_my_messages: false,
            is_left_channel: false,
            is_monoforum: false,
            relative_path: String::new(),
            messages_count_per_split: Vec::new(),
        }
    }
}

/// Complete list of active chats and left channels (port of `Export::Data::DialogsInfo`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DialogsInfo {
    /// Active dialogs.
    pub chats: Vec<DialogInfo>,
    /// Left/banned channels and supergroups.
    pub left: Vec<DialogInfo>,
}

impl DialogsInfo {
    /// Returns a reference to the `index`-th dialog across `chats` followed by `left`.
    pub fn item(&self, index: usize) -> Option<&DialogInfo> {
        let chats_count = self.chats.len();
        if index < chats_count {
            Some(&self.chats[index])
        } else {
            self.left.get(index - chats_count)
        }
    }

    /// Returns a mutable reference to the `index`-th dialog across `chats` followed by `left`.
    pub fn item_mut(&mut self, index: usize) -> Option<&mut DialogInfo> {
        let chats_count = self.chats.len();
        if index < chats_count {
            Some(&mut self.chats[index])
        } else {
            self.left.get_mut(index - chats_count)
        }
    }

    /// Total number of dialogs (`chats.len() + left.len()`).
    pub fn total_count(&self) -> usize {
        self.chats.len() + self.left.len()
    }
}

/// Determines `DialogType` from a `Chat` (port of `DialogTypeFromChat`).
pub fn dialog_type_from_chat(chat: &Chat) -> DialogType {
    if chat.is_monoforum && !chat.is_monoforum_admin {
        DialogType::Personal
    } else if chat.is_monoforum_admin && chat.is_monoforum_of_public_broadcast {
        DialogType::PublicSupergroup
    } else if chat.is_monoforum_admin {
        DialogType::PrivateSupergroup
    } else if chat.username.is_empty() {
        if chat.is_broadcast {
            DialogType::PrivateChannel
        } else if chat.is_supergroup {
            DialogType::PrivateSupergroup
        } else {
            DialogType::PrivateGroup
        }
    } else if chat.is_broadcast {
        DialogType::PublicChannel
    } else {
        DialogType::PublicSupergroup
    }
}

/// Determines `DialogType` from a `User` (port of `DialogTypeFromUser`).
pub const fn dialog_type_from_user(user: &User) -> DialogType {
    if user.is_self {
        DialogType::SelfChat
    } else if user.is_replies {
        DialogType::Replies
    } else if user.is_verify_codes {
        DialogType::VerifyCodes
    } else if user.is_bot {
        DialogType::Bot
    } else {
        DialogType::Personal
    }
}

/// Merges a migrated basic-group split into its target supergroup `DialogInfo`
/// (port of `AddMigrateFromSlice`).
pub fn add_migrate_from_slice(
    to: &mut DialogInfo,
    from: &DialogInfo,
    split_index: i32,
    splits_count: i32,
) -> bool {
    let good = match &to.migrated_from_input {
        InputPeer::Empty => true,
        InputPeer::Chat(InputPeerChat { chat_id }) => {
            PeerId::from_chat(ChatId(*chat_id as u64)) == from.peer_id
        }
        _ => false,
    };
    if !good || from.splits.len() != from.messages_count_per_split.len() {
        return false;
    }
    for (&split, &count) in from.splits.iter().zip(&from.messages_count_per_split) {
        to.splits.push(split - splits_count);
        to.messages_count_per_split.push(count);
    }
    to.migrated_from_input = from.input.clone();
    to.splits.push(split_index - splits_count);
    to.messages_count_per_split.push(0);
    true
}

/// Computes `relative_path`, `only_my_messages`, and sorts `splits` for all dialogs
/// (port of `FinalizeDialogsInfo`).
pub fn finalize_dialogs_info(info: &mut DialogsInfo, settings: &Settings) {
    let full_count = info.chats.len() + info.left.len();
    if full_count == 0 {
        return;
    }
    let digits = (full_count - 1).to_string().len();
    let mut index = 0usize;
    for dialog in &mut info.chats {
        index += 1;
        let number = number_to_string(index, digits, '0');
        dialog.relative_path = if settings.only_single_peer() {
            String::new()
        } else {
            format!("chats/chat_{number}/")
        };
        let setting = match dialog.kind {
            DialogType::SelfChat
            | DialogType::Replies
            | DialogType::VerifyCodes
            | DialogType::Personal
            | DialogType::Unknown => ExportTypes::PERSONAL_CHATS,
            DialogType::Bot => ExportTypes::BOT_CHATS,
            DialogType::PrivateGroup | DialogType::PrivateSupergroup => ExportTypes::PRIVATE_GROUPS,
            DialogType::PrivateChannel => ExportTypes::PRIVATE_CHANNELS,
            DialogType::PublicSupergroup => ExportTypes::PUBLIC_GROUPS,
            DialogType::PublicChannel => ExportTypes::PUBLIC_CHANNELS,
        };
        dialog.only_my_messages =
            dialog.kind != DialogType::Personal && !settings.full_chats.contains(setting);
        dialog.splits.sort_unstable();
    }
    for dialog in &mut info.left {
        index += 1;
        let number = number_to_string(index, digits, '0');
        dialog.relative_path = format!("chats/chat_{number}/");
        dialog.only_my_messages = true;
    }
}

/// Batch of messages + referenced peer map (port of `Export::Data::MessagesSlice`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagesSlice {
    /// Messages in chronological order.
    pub list: Vec<Message>,
    /// Referenced peers indexed by `PeerId`.
    pub peers: BTreeMap<PeerId, Peer>,
}

/// Shifts message IDs and local reply IDs for a migrated basic-group history slice
/// (port of `AdjustMigrateMessageIds`).
pub fn adjust_migrate_message_ids(mut slice: MessagesSlice) -> MessagesSlice {
    for message in &mut slice.list {
        message.id += MIGRATED_MESSAGES_ID_SHIFT;
        if message.reply_to_msg_id != 0 && message.reply_to_peer_id.is_zero() {
            message.reply_to_msg_id += MIGRATED_MESSAGES_ID_SHIFT;
        }
    }
    slice
}

/// Returns true if `message` should be skipped due to `single_peer_from..single_peer_till`
/// (port of `SkipMessageByDate`).
pub const fn skip_message_by_date(message: &Message, settings: &Settings) -> bool {
    let good_from = settings.single_peer_from <= 0 || settings.single_peer_from <= message.date;
    let good_till = settings.single_peer_till <= 0 || message.date < settings.single_peer_till;
    !good_from || !good_till
}

/// Context tracking sequential numbering when generating suggested media paths
/// (port of `Export::Data::ParseMediaContext`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseMediaContext {
    /// Exporting user's own peer ID.
    pub self_peer_id: PeerId,
    /// Photo counter.
    pub photos: i32,
    /// Voice/audio counter.
    pub audios: i32,
    /// Video counter.
    pub videos: i32,
    /// Generic file counter.
    pub files: i32,
    /// Contact vCard counter.
    pub contacts: i32,
    /// Current inline bot ID for game links.
    pub bot_id: UserId,
}

/// Formats the `@DD-MM-YYYY_HH-MM-SS` filename date suffix (port of `PrepareFileNameDatePart`).
pub fn prepare_file_name_date_part(date: TimeId, utc_offset_secs: i32) -> String {
    if date == 0 {
        return String::new();
    }
    format!(
        "@{}",
        format_date_time_with_offset(date, utc_offset_secs, false, '-', '-', '_')
    )
}

/// Builds a suggested photo filename (`photo_<index>@DD-MM-YYYY_HH-MM-SS.jpg`).
pub fn prepare_photo_file_name(index: i32, date: TimeId, utc_offset_secs: i32) -> String {
    format!(
        "photo_{index}{}.jpg",
        prepare_file_name_date_part(date, utc_offset_secs)
    )
}

/// Builds a suggested story filename (`story_<index>@DD-MM-YYYY_HH-MM-SS<ext>`).
pub fn prepare_story_file_name(
    index: i32,
    date: TimeId,
    extension: &str,
    utc_offset_secs: i32,
) -> String {
    format!(
        "story_{index}{}{extension}",
        prepare_file_name_date_part(date, utc_offset_secs)
    )
}

/// Returns the media subfolder name for a document (port of `DocumentFolder`).
pub const fn document_folder(doc: &Document) -> &'static str {
    if doc.is_video_file {
        "video_files"
    } else if doc.is_animated {
        "animations"
    } else if doc.is_sticker {
        "stickers"
    } else if doc.is_voice_message {
        "voice_messages"
    } else if doc.is_video_message {
        "round_video_messages"
    } else {
        "files"
    }
}

/// Maps common MIME types to a file extension (stand-in for `Core::MimeTypeForName`).
pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "audio/ogg" => ".ogg",
        "audio/mpeg" | "audio/mp3" => ".mp3",
        "audio/flac" => ".flac",
        "audio/wav" => ".wav",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "application/x-tgsticker" => ".tgs",
        "text/plain" => ".txt",
        _ => "",
    }
}

/// Computes a sanitized suggested document filename (port of `ComputeDocumentName` +
/// `FileNameFromUserString`).
pub fn compute_document_name(
    context: &mut ParseMediaContext,
    doc: &Document,
    date: TimeId,
    utc_offset_secs: i32,
) -> String {
    let raw = if !doc.name.is_empty() {
        doc.name.clone()
    } else if doc.is_voice_message {
        context.audios += 1;
        let is_mp3 = doc.mime.eq_ignore_ascii_case("audio/mp3");
        format!(
            "audio_{}{}{}",
            context.audios,
            prepare_file_name_date_part(date, utc_offset_secs),
            if is_mp3 { ".mp3" } else { ".ogg" }
        )
    } else if doc.is_video_file {
        context.videos += 1;
        let ext = extension_for_mime(&doc.mime);
        let ext = if ext.is_empty() { ".mov" } else { ext };
        format!(
            "video_{}{}{ext}",
            context.videos,
            prepare_file_name_date_part(date, utc_offset_secs)
        )
    } else {
        context.files += 1;
        let ext = extension_for_mime(&doc.mime);
        let ext = if ext.is_empty() { ".unknown" } else { ext };
        format!(
            "file_{}{}{ext}",
            context.files,
            prepare_file_name_date_part(date, utc_offset_secs)
        )
    };
    file_name_from_user_string(&raw)
}

/// Computes the suggested relative path for a document under `suggested_folder`.
pub fn compute_document_suggested_path(
    context: &mut ParseMediaContext,
    doc: &Document,
    suggested_folder: &str,
    date: TimeId,
    utc_offset_secs: i32,
) -> String {
    format!(
        "{suggested_folder}{}/{}",
        document_folder(doc),
        compute_document_name(context, doc, date, utc_offset_secs)
    )
}

/// Returns true if `format` is `"svg"` or `"svgz"` (case-insensitive, port of `IsSvgFormat`).
pub fn is_svg_format(format: &str) -> bool {
    format.eq_ignore_ascii_case("svg") || format.eq_ignore_ascii_case("svgz")
}

/// Generates a scaled thumbnail image on disk (port of `WriteImageThumb`).
///
/// Returns `(relative_thumb_path, final_size)` or `(String::new(), Size::default())` if the
/// source image cannot be read, is SVG, exceeds `MAX_IMAGE_SIZE`, or fails to encode.
pub fn write_image_thumb<F>(
    base_path: &str,
    large_path: &str,
    convert_size: F,
    target_format: Option<ImageFormat>,
    postfix: &str,
) -> (String, Size)
where
    F: FnOnce(Size) -> Size,
{
    if large_path.is_empty() {
        return (String::new(), Size::default());
    }
    if large_path.to_ascii_lowercase().ends_with(".svg")
        || large_path.to_ascii_lowercase().ends_with(".svgz")
    {
        return (String::new(), Size::default());
    }
    let full_path = format!("{base_path}{large_path}");
    let reader = match ImageReader::open(&full_path) {
        Ok(r) => match r.with_guessed_format() {
            Ok(r) => r,
            Err(_) => return (String::new(), Size::default()),
        },
        Err(_) => return (String::new(), Size::default()),
    };
    let source_format = reader.format();
    let dims = match reader.into_dimensions() {
        Ok(d) => d,
        Err(_) => return (String::new(), Size::default()),
    };
    if dims.0 == 0 || dims.1 == 0 || dims.0 >= MAX_IMAGE_SIZE || dims.1 >= MAX_IMAGE_SIZE {
        return (String::new(), Size::default());
    }
    let img = match ImageReader::open(&full_path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok())
    {
        Some(i) => i,
        None => return (String::new(), Size::default()),
    };
    let final_size = convert_size(Size::new(img.width() as i32, img.height() as i32));
    if final_size.is_empty() {
        return (String::new(), Size::default());
    }
    let resized = img.resize_exact(
        final_size.width as u32,
        final_size.height as u32,
        FilterType::Triangle,
    );
    let last_slash = large_path.rfind('/').map_or(0, |i| i + 1);
    let first_dot = large_path[last_slash..].find('.').map(|i| last_slash + i);
    let thumb_suggested = match first_dot {
        Some(dot) => format!("{}{postfix}{}", &large_path[..dot], &large_path[dot..]),
        None => format!("{large_path}{postfix}"),
    };
    let result_rel = OutputFile::prepare_relative_path(base_path, &thumb_suggested);
    let save_path = format!("{base_path}{result_rel}");
    if let Some(parent) = Path::new(&save_path).parent()
        && !parent.exists()
        && std::fs::create_dir_all(parent).is_err()
    {
        return (String::new(), Size::default());
    }
    let format = target_format.or(source_format).unwrap_or(ImageFormat::Png);
    let save_res = if format == ImageFormat::Jpeg {
        resized
            .to_rgb8()
            .save_with_format(&save_path, ImageFormat::Jpeg)
    } else {
        resized.save_with_format(&save_path, format)
    };
    if save_res.is_err() {
        return (String::new(), Size::default());
    }
    (result_rel, final_size)
}

/// Generates an exact-dimension thumbnail image on disk (port of `WriteImageThumb` width/height overload).
pub fn write_image_thumb_exact(
    base_path: &str,
    large_path: &str,
    width: i32,
    height: i32,
    postfix: &str,
) -> String {
    write_image_thumb(
        base_path,
        large_path,
        |_| Size::new(width, height),
        None,
        postfix,
    )
    .0
}

/// Formats a phone number (port of `FormatPhoneNumber`).
pub fn format_phone_number(phone: &str) -> String {
    if phone.is_empty() {
        String::new()
    } else {
        format_phone(phone)
    }
}

/// Formats a Unix timestamp in `DD.MM.YYYY HH:MM:SS` (with optional ` UTC±HH:MM`) using UTC+0
/// (port of `FormatDateTime`).
pub fn format_date_time(
    date: TimeId,
    has_time_zone: bool,
    date_separator: char,
    time_separator: char,
    separator: char,
) -> String {
    format_date_time_with_offset(
        date,
        0,
        has_time_zone,
        date_separator,
        time_separator,
        separator,
    )
}

/// Formats a Unix timestamp in `DD<sep>MM<sep>YYYY<mid>HH<tsep>MM<tsep>SS` with a specific UTC offset.
pub fn format_date_time_with_offset(
    date: TimeId,
    utc_offset_secs: i32,
    has_time_zone: bool,
    date_separator: char,
    time_separator: char,
    separator: char,
) -> String {
    if date == 0 {
        return String::new();
    }
    let (year, month, day, hour, minute, second) = unix_to_civil(i64::from(date), utc_offset_secs);
    let mut out = format!(
        "{day:02}{date_separator}{month:02}{date_separator}{year}{separator}{hour:02}{time_separator}{minute:02}{time_separator}{second:02}"
    );
    if has_time_zone {
        out.push(separator);
        out.push_str(&format_utc_offset(utc_offset_secs));
    }
    out
}

/// Formats a Unix timestamp as ISO-8601 local datetime `YYYY-MM-DDTHH:MM:SS`
/// (matching `QDateTime::fromSecsSinceEpoch(date).toString(Qt::ISODate)`).
pub fn format_iso_date_with_offset(date: TimeId, utc_offset_secs: i32) -> String {
    let (year, month, day, hour, minute, second) = unix_to_civil(i64::from(date), utc_offset_secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}

/// Formats a Unix timestamp as `D Month YYYY` (matching `FormatDateText` in `export_output_html.cpp`).
pub fn format_date_text_with_offset(date: TimeId, utc_offset_secs: i32) -> String {
    let (year, month, day, _, _, _) = unix_to_civil(i64::from(date), utc_offset_secs);
    let month_name = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    };
    format!("{day} {month_name} {year}")
}

/// Formats a Unix timestamp as `HH:MM` (matching `FormatTimeText` in `export_output_html.cpp`).
pub fn format_time_text_with_offset(date: TimeId, utc_offset_secs: i32) -> String {
    let (_, _, _, hour, minute, _) = unix_to_civil(i64::from(date), utc_offset_secs);
    format!("{hour:02}:{minute:02}")
}

/// Formats a monetary amount (port of `FormatMoneyAmount`).
pub fn format_money_amount(amount: i64, currency: &str) -> String {
    fill_amount_and_currency(amount, currency)
}

/// Formats a nano-Gram amount (exact port of `FormatGramsAmount`).
pub fn format_grams_amount(nanos: i64) -> String {
    const NANOS: u64 = NANOS_IN_GRAM as u64;
    let negative = nanos < 0;
    let absolute = nanos.unsigned_abs();
    let whole = absolute / NANOS;
    let mut fraction = number_to_string(absolute % NANOS, 9, '0');
    while fraction.ends_with('0') {
        fraction.pop();
    }
    let sign = if negative { "-" } else { "" };
    let suffix = if absolute == NANOS { " Gram" } else { " Grams" };
    if fraction.is_empty() {
        format!("{sign}{whole}{suffix}")
    } else {
        format!("{sign}{whole}.{fraction}{suffix}")
    }
}

/// Formats a byte count in English (`B`, `KB`, `MB`) with truncated integer tenths
/// (exact port of `FormatFileSize`).
pub fn format_file_size(size: i64) -> String {
    let tenths = |value: i64| format!("{}.{}", value / 10, value % 10);
    if size >= 1024 * 1024 {
        format!("{} MB", tenths(size.saturating_mul(10) / (1024 * 1024)))
    } else if size >= 1024 {
        format!("{} KB", tenths(size.saturating_mul(10) / 1024))
    } else {
        format!("{size} B")
    }
}

/// Formats a duration in seconds (port of `FormatDuration`).
pub fn format_duration(seconds: i64) -> String {
    format_duration_text(seconds)
}

/// Computes plain readable text by concatenating `TextPart` texts.
pub fn compute_message_text(parts: &[TextPart]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str(&part.text);
    }
    out
}
