//! Port of inline bot result model, layout sizing, gallery vs list decisions,
//! query parsing, debounce timing, pagination, and `switch_pm` handling from:
//! - `inline_bots/inline_bot_result.h` / `.cpp`
//! - `inline_bots/inline_bot_layout_item.h` / `.cpp`
//! - `inline_bots/inline_bot_layout_internal.h` / `.cpp`
//! - `inline_bots/inline_results_inner.h` / `.cpp`
//! - `inline_bots/inline_results_widget.cpp`
//! - `chat_helpers/message_field.cpp` (`ParseInlineBotQuery`)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::keyboard_layout::Rect;
use crate::markup::ReplyMarkupData;
use crate::mosaic::{DEFAULT_INLINE_RESULTS_MIN_WIDTH, Margins, MosaicLayout, SimpleMosaicItem};
use crate::stand_in::{DocumentId, PeerId, PhotoId, UserId};

/// Debounce delay before issuing `messages.getInlineBotResults` (`kInlineBotRequestDelay = 400`).
///
/// Port of `inline_bots/inline_results_widget.cpp:27`.
pub const INLINE_BOT_REQUEST_DELAY_MS: u64 = 400;

/// Minimum repaint interval for inline results animations (`kMinRepaintDelay = 33`).
pub const INLINE_MIN_REPAINT_DELAY_MS: u64 = 33;

/// Minimum repaint interval after scrolling inline results (`kMinAfterScrollDelay = 33`).
pub const INLINE_MIN_AFTER_SCROLL_DELAY_MS: u64 = 33;

/// Maximum pixel area for playing an inline GIF (`kMaxInlineArea = 1280 * 720`).
pub const MAX_INLINE_AREA: i32 = 1280 * 720;

/// Maximum frame side length before downscaling (`kMaxInlineFrameSide = 4096`).
pub const MAX_INLINE_FRAME_SIDE: i32 = 4096;

/// Style metrics from `chat_helpers/chat_helpers.style` & `ui/chat/chat.style` (1x logical px).
pub mod style {
    /// Width of the inline results panel (`st::emojiPanWidth = 345px`).
    pub const EMOJI_PAN_WIDTH: i32 = 345;
    /// Scrollbar width (`st::emojiScroll.width = 14px`).
    pub const EMOJI_SCROLL_WIDTH: i32 = 14;
    /// Left content inset (`st::inlineResultsLeft = 11px`).
    pub const INLINE_RESULTS_LEFT: i32 = 11;
    /// Horizontal & vertical gap between gallery cells (`st::inlineResultsSkip = 3px`).
    pub const INLINE_RESULTS_SKIP: i32 = 3;
    /// Gallery cell height for photos/GIFs/thumbnails (`st::inlineMediaHeight = 96px`).
    pub const INLINE_MEDIA_HEIGHT: i32 = 96;
    /// Square thumbnail size for list items (`st::inlineThumbSize = 64px`).
    pub const INLINE_THUMB_SIZE: i32 = 64;
    /// Gap between thumbnail/file circle and text (`st::inlineThumbSkip = 10px`).
    pub const INLINE_THUMB_SKIP: i32 = 10;
    /// File/audio circle diameter (`st::inlineFileSize = 44px`).
    pub const INLINE_FILE_SIZE: i32 = 44;
    /// Top/bottom row margin (`st::inlineRowMargin = 6px`).
    pub const INLINE_ROW_MARGIN: i32 = 6;
    /// Bottom separator border thickness (`st::inlineRowBorder = 1px`).
    pub const INLINE_ROW_BORDER: i32 = 1;
    /// Minimum gallery item width (`st::inlineResultsMinWidth = 48px`).
    pub const INLINE_RESULTS_MIN_WIDTH: i32 = 48;
    /// Sticker grid cell size (`st::stickerPanSize = 64x64`).
    pub const STICKER_PAN_SIZE: i32 = 64;
    /// Panel vertical padding (`st::stickerPanPadding = 11px`).
    pub const STICKER_PAN_PADDING: i32 = 11;
    /// Minimum inline panel content height (`st::inlineResultsMinHeight = 278px`).
    pub const INLINE_RESULTS_MIN_HEIGHT: i32 = 278;
    /// Maximum inline panel content height (`st::inlineResultsMaxHeight = 640px`).
    pub const INLINE_RESULTS_MAX_HEIGHT: i32 = 640;
    /// Default mosaic `bigWidth` (`345 - 14 - 11 = 320px`).
    pub const DEFAULT_MOSAIC_BIG_WIDTH: i32 =
        EMOJI_PAN_WIDTH - EMOJI_SCROLL_WIDTH - INLINE_RESULTS_LEFT;
    /// Maximum sticker dimension (`st::maxStickerSize = 224px`).
    pub const MAX_STICKER_SIZE: i32 = 224;
}

/// Inline bot result type covering all Telegram inline result types.
///
/// Port of `InlineBots::Result::Type` (`inline_bot_result.h:96-110`,
/// `inline_bot_result.cpp:63-85`), preserving the distinct `Voice` variant requested
/// while matching tdesktop's layout behavior (`Voice` lays out as `File` just like `Audio`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InlineResultKind {
    /// `"article"`
    Article,
    /// `"photo"`
    Photo,
    /// `"gif"`
    Gif,
    /// `"video"`
    Video,
    /// `"audio"`
    Audio,
    /// `"voice"` (mapped to `Audio`/file layout in tdesktop)
    Voice,
    /// `"file"` (generic document)
    Document,
    /// `"sticker"`
    Sticker,
    /// `"contact"`
    Contact,
    /// `"geo"` (location)
    Location,
    /// `"venue"`
    Venue,
    /// `"game"`
    Game,
    /// `botInlineMessageWidget`
    Widget,
}

impl InlineResultKind {
    /// Parses a TL `BotInlineResult.type` string (and `is_widget` message check).
    ///
    /// Port of `Result::Create` type mapping (`inline_bot_result.cpp:63-85`).
    pub fn from_tl_type(type_str: &str, is_widget_message: bool) -> Option<Self> {
        if is_widget_message {
            return Some(Self::Widget);
        }
        match type_str {
            "photo" => Some(Self::Photo),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "voice" => Some(Self::Voice),
            "sticker" => Some(Self::Sticker),
            "file" => Some(Self::Document),
            "gif" => Some(Self::Gif),
            "article" => Some(Self::Article),
            "contact" => Some(Self::Contact),
            "venue" => Some(Self::Venue),
            "geo" => Some(Self::Location),
            "game" => Some(Self::Game),
            _ => None,
        }
    }

    /// Returns `true` if `Result::Create` requires a photo attachment (`inline_bot_result.cpp:160-163`).
    pub const fn requires_photo(self) -> bool {
        matches!(self, Self::Photo)
    }

    /// Returns `true` if `Result::Create` requires a document attachment (`inline_bot_result.cpp:164-171`).
    pub const fn requires_document(self) -> bool {
        matches!(
            self,
            Self::Audio | Self::Voice | Self::Document | Self::Sticker | Self::Gif
        )
    }

    /// Default filename synthesized by `Result::adjustAttributes` for GIFs (`inline_bot_result.cpp:483-490`).
    pub fn default_gif_filename(mime_type: &str) -> &'static str {
        if mime_type == "video/mp4" {
            "animation.gif.mp4"
        } else {
            "animation.gif"
        }
    }

    /// Returns `true` when `audio/ogg` should be promoted to a voice message (`inline_bot_result.cpp:498`).
    pub fn is_voice_mime(mime_type: &str) -> bool {
        mime_type == "audio/ogg"
    }
}

/// Coordinates attached to a `Location` or `Venue` inline result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LocationPoint {
    /// Latitude in degrees.
    pub lat: f64,
    /// Longitude in degrees.
    pub lon: f64,
    /// Optional server access hash.
    pub access_hash: i64,
}

/// Lightweight media dimension metadata carried on an [`InlineResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaDimensions {
    /// Width in pixels (`0` if unknown).
    pub width: i32,
    /// Height in pixels (`0` if unknown).
    pub height: i32,
}

/// Plain representation of an inline bot result (`InlineBots::Result`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineResult {
    /// Query id from the enclosing `messages.botResults`.
    pub query_id: u64,
    /// Bot-assigned result id.
    pub id: String,
    /// Result kind.
    pub kind: InlineResultKind,
    /// Result title.
    pub title: String,
    /// Result description.
    pub description: String,
    /// Optional article/external URL.
    pub url: String,
    /// Optional content URL (from `webDocument`).
    pub content_url: String,
    /// Photo id if result carries a photo.
    pub photo_id: Option<PhotoId>,
    /// Document id if result carries a document.
    pub document_id: Option<DocumentId>,
    /// Media dimensions (for photo/GIF/thumbnail mosaic sizing).
    pub dimensions: MediaDimensions,
    /// Whether the result has a separate thumbnail image.
    pub has_thumbnail: bool,
    /// Optional location coordinates (for `Location`/`Venue`).
    pub location: Option<LocationPoint>,
    /// Optional duration in seconds (for video/audio/voice).
    pub duration_secs: Option<i32>,
    /// Optional inline reply markup attached to the sent message.
    pub reply_markup: Option<ReplyMarkupData>,
}

impl InlineResult {
    /// Validates required media fields matching `InlineBots::Result::Create`
    /// (`inline_bot_result.cpp:160-171`).
    pub const fn is_valid(&self) -> bool {
        if self.kind.requires_photo() && self.photo_id.is_none() {
            return false;
        }
        if self.kind.requires_document() && self.document_id.is_none() {
            return false;
        }
        true
    }

    /// Returns `true` if this result displays a thumbnail in list mode.
    ///
    /// Port of `InlineBots::Result::hasThumbDisplay` (`inline_bot_result.cpp:376-387`).
    pub const fn has_thumb_display(&self) -> bool {
        self.has_thumbnail
            || self.photo_id.is_some()
            || matches!(self.kind, InlineResultKind::Contact)
            || self.location.is_some()
    }

    /// Computes the single fallback monogram letter shown when an article has no thumbnail.
    ///
    /// Port of `ItemBase::getResultThumbLetter` (`inline_bot_layout_item.cpp:255-272`).
    pub fn thumb_letter(&self) -> Option<char> {
        if !self.url.is_empty() {
            let parts: Vec<&str> = self.url.split('/').collect();
            if !parts.is_empty() {
                let mut domain = parts[0];
                if parts.len() > 2 && domain.ends_with(':') && parts[1].is_empty() {
                    domain = parts[2];
                }
                let host = domain.rsplit('@').next().unwrap_or(domain);
                let host_parts: Vec<&str> = host.split('.').collect();
                if host_parts.len() > 1
                    && let Some(ch) = host_parts[host_parts.len() - 2].chars().next()
                {
                    return ch.to_uppercase().next();
                }
            }
        }
        self.title
            .chars()
            .next()
            .and_then(|c| c.to_uppercase().next())
    }
}

/// Concrete layout widget variant chosen for an [`InlineResult`].
///
/// Port of `ItemBase::createLayout` (`inline_bot_layout_item.cpp:93-171`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InlineLayoutKind {
    /// Gallery photo cell (`internal::Photo`).
    Photo,
    /// Gallery animated GIF cell (`internal::Gif`).
    Gif,
    /// Gallery sticker cell (`internal::Sticker`).
    Sticker,
    /// Generic gallery thumbnail cell (`internal::Thumbnail`, forced when `gallery = Some(true)`).
    Thumbnail,
    /// Full-line article/geo/venue/widget list row (`internal::Article`).
    Article {
        /// Whether the row reserves left space for a thumbnail.
        with_thumb: bool,
    },
    /// Full-line video list row (`internal::Video`).
    Video,
    /// Full-line file/audio/voice list row (`internal::File`).
    File,
    /// Full-line contact list row (`internal::Contact`).
    Contact,
    /// Full-line game list row (`internal::Game`).
    Game,
}

impl InlineLayoutKind {
    /// Returns `true` if this layout occupies a full row on its own (`isFullLine()`).
    ///
    /// Matches `inline_bot_layout_internal.h`: `Gif`, `Photo`, `Sticker`, and `Thumbnail`
    /// return `false`; all other types inherit `ItemBase::isFullLine() == true`.
    pub const fn is_full_line(self) -> bool {
        !matches!(
            self,
            Self::Photo | Self::Gif | Self::Sticker | Self::Thumbnail
        )
    }

    /// Returns `true` if this layout uses horizontal spacing to the next cell (`hasRightSkip()`).
    ///
    /// Matches `inline_bot_layout_internal.h`: `Gif`, `Photo`, `Thumbnail` -> `true`;
    /// `Sticker` and full-line items -> `false`.
    pub const fn has_right_skip(self) -> bool {
        matches!(self, Self::Photo | Self::Gif | Self::Thumbnail)
    }
}

/// Decides the [`InlineLayoutKind`] for a result given `force_thumb` and optional `gallery` flag.
///
/// Exact port of `InlineBots::Layout::ItemBase::createLayout` (`inline_bot_layout_item.cpp:93-171`).
pub fn choose_layout_kind(
    result: &InlineResult,
    force_thumb: bool,
    gallery: Option<bool>,
) -> InlineLayoutKind {
    if let Some(is_gallery) = gallery {
        if !is_gallery {
            // Force list mode: render gallery types as Article.
            if matches!(
                result.kind,
                InlineResultKind::Photo | InlineResultKind::Sticker | InlineResultKind::Gif
            ) {
                return InlineLayoutKind::Article {
                    with_thumb: force_thumb,
                };
            }
        } else {
            // Force gallery mode: render list types as Thumbnail.
            if matches!(
                result.kind,
                InlineResultKind::Article
                    | InlineResultKind::Location
                    | InlineResultKind::Venue
                    | InlineResultKind::Video
                    | InlineResultKind::Audio
                    | InlineResultKind::Voice
                    | InlineResultKind::Document
                    | InlineResultKind::Contact
                    | InlineResultKind::Game
            ) {
                return InlineLayoutKind::Thumbnail;
            }
        }
    }

    match result.kind {
        InlineResultKind::Photo => InlineLayoutKind::Photo,
        InlineResultKind::Audio | InlineResultKind::Voice | InlineResultKind::Document => {
            InlineLayoutKind::File
        }
        InlineResultKind::Video => InlineLayoutKind::Video,
        InlineResultKind::Sticker => InlineLayoutKind::Sticker,
        InlineResultKind::Gif => InlineLayoutKind::Gif,
        InlineResultKind::Article | InlineResultKind::Location | InlineResultKind::Venue => {
            InlineLayoutKind::Article {
                with_thumb: force_thumb,
            }
        }
        InlineResultKind::Widget => InlineLayoutKind::Article {
            with_thumb: force_thumb || result.has_thumb_display(),
        },
        InlineResultKind::Game => InlineLayoutKind::Game,
        InlineResultKind::Contact => InlineLayoutKind::Contact,
    }
}

/// Downscales `(w, h)` proportionally to fit within `(max_w, max_h)` without upscaling.
///
/// Port of `HistoryView::DownscaledSize` used by `BoundFrameSize`.
pub fn downscaled_size(w: i32, h: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if w <= 0 || h <= 0 || max_w <= 0 || max_h <= 0 {
        return (w.max(0), h.max(0));
    }
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let w64 = w as i64;
    let h64 = h as i64;
    let mw64 = max_w as i64;
    let mh64 = max_h as i64;
    if w64 * mh64 > h64 * mw64 {
        (max_w, ((h64 * mw64) / w64).max(1) as i32)
    } else {
        (((w64 * mh64) / h64).max(1) as i32, max_h)
    }
}

/// Bounds a frame to `kMaxInlineFrameSide / device_pixel_ratio` on each side.
///
/// Port of `BoundFrameSize` in `inline_bot_layout_internal.cpp:67-70`.
pub fn bound_frame_size(w: i32, h: i32, device_pixel_ratio: i32) -> (i32, i32) {
    let dpr = device_pixel_ratio.max(1);
    let side = MAX_INLINE_FRAME_SIDE / dpr;
    downscaled_size(w, h, side, side)
}

/// Computed initial dimensions `(max_width, min_height)` for an inline layout item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InlineItemDimensions {
    /// Chosen layout variant.
    pub layout_kind: InlineLayoutKind,
    /// Natural maximum width (`_maxw`). `0` means the item is skipped (e.g. GIF with zero size).
    pub max_width: i32,
    /// Minimum row height (`_minh`).
    pub min_height: i32,
}

/// Computes `(max_width, min_height)` for `result` under `layout_kind` at 1x logical pixels.
///
/// Exact port of `initDimensions()` across `Gif`, `Photo`, `Sticker`, `Thumbnail`,
/// `Video`, `File`, `Contact`, `Article`, and `Game` in `inline_bot_layout_internal.cpp`.
///
/// For `Article` and `Game`, `text_height` allows callers to supply measured title+description
/// height; pass `0` to compute the minimum thumbnail-floor height.
pub fn compute_item_dimensions(
    result: &InlineResult,
    layout_kind: InlineLayoutKind,
    device_pixel_ratio: i32,
    text_height: i32,
) -> InlineItemDimensions {
    let full_row_width = style::DEFAULT_MOSAIC_BIG_WIDTH;
    match layout_kind {
        InlineLayoutKind::Gif => {
            let w = result.dimensions.width;
            let h = result.dimensions.height;
            let max_width = if w <= 0 || h <= 0 {
                0
            } else {
                let (bw, bh) = bound_frame_size(w, h, device_pixel_ratio);
                let scaled = (bw * style::INLINE_MEDIA_HEIGHT) / bh.max(1);
                scaled.max(style::INLINE_RESULTS_MIN_WIDTH)
            };
            InlineItemDimensions {
                layout_kind,
                max_width,
                min_height: style::INLINE_MEDIA_HEIGHT + style::INLINE_RESULTS_SKIP,
            }
        }
        InlineLayoutKind::Photo | InlineLayoutKind::Thumbnail => {
            let (mut w, mut h) = (result.dimensions.width, result.dimensions.height);
            if w <= 0 || h <= 0 {
                w = 1;
                h = 1;
            }
            let (bw, bh) = bound_frame_size(w, h, device_pixel_ratio);
            let scaled = (bw * style::INLINE_MEDIA_HEIGHT) / bh.max(1);
            InlineItemDimensions {
                layout_kind,
                max_width: scaled.max(style::INLINE_RESULTS_MIN_WIDTH),
                min_height: style::INLINE_MEDIA_HEIGHT + style::INLINE_RESULTS_SKIP,
            }
        }
        InlineLayoutKind::Sticker => InlineItemDimensions {
            layout_kind,
            max_width: style::STICKER_PAN_SIZE,
            min_height: style::STICKER_PAN_SIZE,
        },
        InlineLayoutKind::Video => InlineItemDimensions {
            layout_kind,
            max_width: full_row_width,
            min_height: style::INLINE_THUMB_SIZE
                + style::INLINE_ROW_MARGIN * 2
                + style::INLINE_ROW_BORDER,
        },
        InlineLayoutKind::File | InlineLayoutKind::Contact => InlineItemDimensions {
            layout_kind,
            max_width: full_row_width,
            min_height: style::INLINE_FILE_SIZE
                + style::INLINE_ROW_MARGIN * 2
                + style::INLINE_ROW_BORDER,
        },
        InlineLayoutKind::Article { with_thumb } => {
            let mut body = text_height.max(0);
            if with_thumb {
                body = body.max(style::INLINE_THUMB_SIZE);
            }
            InlineItemDimensions {
                layout_kind,
                max_width: full_row_width,
                min_height: body + style::INLINE_ROW_MARGIN * 2 + style::INLINE_ROW_BORDER,
            }
        }
        InlineLayoutKind::Game => {
            let body = text_height.max(style::INLINE_THUMB_SIZE);
            InlineItemDimensions {
                layout_kind,
                max_width: full_row_width,
                min_height: body + style::INLINE_ROW_MARGIN * 2 + style::INLINE_ROW_BORDER,
            }
        }
    }
}

/// Laid-out inline result panel output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineResultsLayoutOutput {
    /// Whether any result triggered the list-wide `with_thumb` hint.
    pub with_thumb: bool,
    /// Positioned `(result_index, layout_kind, Rect)` entries.
    pub items: Vec<(usize, InlineLayoutKind, Rect)>,
    /// Total panel height including padding and optional `switch_pm` header.
    pub total_height: i32,
}

/// Lays out a page of [`InlineResult`] entries into a [`MosaicLayout`], faithfully
/// porting `InlineBots::Layout::Inner::refreshInlineRows` (`inline_results_inner.cpp:560-590`).
///
/// Full-line items flush any pending gallery items first, then get their own single-item row.
pub fn layout_inline_results(
    results: &[InlineResult],
    gallery: bool,
    panel_width: i32,
    big_width: i32,
    switch_pm_height: Option<i32>,
) -> InlineResultsLayoutOutput {
    let with_thumb = results.iter().any(InlineResult::has_thumb_display);
    let mut mosaic = MosaicLayout::<SimpleMosaicItem>::new(big_width);
    mosaic.set_full_width(panel_width);
    mosaic.set_right_skip(style::INLINE_RESULTS_SKIP);
    mosaic.set_min_item_width(DEFAULT_INLINE_RESULTS_MIN_WIDTH);
    let top_offset = switch_pm_height.map_or(0, |h| h + style::INLINE_RESULTS_SKIP);
    mosaic.set_padding(Margins::new(
        style::INLINE_RESULTS_LEFT,
        style::STICKER_PAN_PADDING + top_offset,
        0,
        style::STICKER_PAN_PADDING,
    ));

    let mut kinds = Vec::with_capacity(results.len());
    let mut pending = Vec::new();
    for (idx, res) in results.iter().enumerate() {
        let kind = choose_layout_kind(res, with_thumb, Some(gallery));
        kinds.push(kind);
        let dims = compute_item_dimensions(res, kind, 1, 0);
        if dims.max_width <= 0 {
            continue;
        }
        let item = SimpleMosaicItem::new(idx, dims.max_width, dims.min_height);
        if kind.is_full_line() {
            if !pending.is_empty() {
                mosaic.add_items(std::mem::take(&mut pending));
            }
            mosaic.add_items(vec![item]);
        } else {
            pending.push(item);
        }
    }
    if !pending.is_empty() {
        mosaic.add_items(pending);
    }

    let mut laid_out = Vec::new();
    let mut top = style::STICKER_PAN_PADDING + top_offset;
    for row in mosaic.rows() {
        let mut left = style::INLINE_RESULTS_LEFT;
        for item in &row.items {
            let rect = Rect::new(left, top, item.width, item.height);
            laid_out.push((item.id, kinds[item.id], rect));
            left += item.width + style::INLINE_RESULTS_SKIP;
        }
        top += row.height;
    }

    let total_height = if mosaic.is_empty() && switch_pm_height.is_none() {
        style::STICKER_PAN_PADDING * 2
    } else {
        top + style::STICKER_PAN_PADDING
    };

    InlineResultsLayoutOutput {
        with_thumb,
        items: laid_out,
        total_height,
    }
}

// ---------------------------------------------------------------------------
// Inline query parsing (chat_helpers/message_field.cpp:859-940)
// ---------------------------------------------------------------------------

/// Parsed `@bot query` prefix from a compose input field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParsedInlineQuery {
    /// Bot username without the leading `@`.
    pub username: String,
    /// Query text following `@username ` (empty when only `@username` ending in `"bot"`).
    pub query: String,
}

/// Parses an inline bot query from compose input text.
///
/// Port of `ParseInlineBotQuery` (`chat_helpers/message_field.cpp:859-931`):
/// - Must start with `@` followed by a letter and `[alphanumeric | '_']*`
/// - If the username spans the entire text, it is only accepted when the text ends with `"bot"`
///   (case-sensitive, matching `text.endsWith(u"bot"_q)`).
/// - Otherwise the character immediately after the username must be whitespace.
/// - If `first_formatting_tag_offset` is `< username_end`, returns `None` (matches tdesktop
///   rejecting formatted mentions as inline queries).
/// - Minimum username length is 3 characters (lines 925-928).
pub fn parse_inline_bot_query(
    text: &str,
    first_formatting_tag_offset: Option<usize>,
) -> Option<ParsedInlineQuery> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if first != '@' {
        return None;
    }
    let (uname_start, second) = chars.next()?;
    if !second.is_alphabetic() {
        return None;
    }

    let mut uname_end = uname_start + second.len_utf8();
    let mut char_count = 1usize;
    let mut ended_by_non_space = false;

    for (byte_idx, ch) in chars {
        if ch.is_alphanumeric() || ch == '_' {
            uname_end = byte_idx + ch.len_utf8();
            char_count += 1;
        } else if ch.is_whitespace() {
            break;
        } else {
            ended_by_non_space = true;
            break;
        }
    }

    if ended_by_non_space || char_count < 3 {
        return None;
    }

    let equals_text = uname_end == text.len();
    let valid = if equals_text {
        text.ends_with("bot")
    } else {
        text[uname_end..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    };
    if !valid {
        return None;
    }

    if let Some(tag_offset) = first_formatting_tag_offset
        && tag_offset < uname_end
    {
        return None;
    }

    let username = text[uname_start..uname_end].to_string();
    let query = if equals_text {
        String::new()
    } else {
        let ws_len = text[uname_end..].chars().next().map_or(0, char::len_utf8);
        text[uname_end + ws_len..].to_string()
    };

    Some(ParsedInlineQuery { username, query })
}

/// Prepends a leading space when `text` starts with `@`, preventing it from being parsed
/// as an inline bot query (`PreventInlineBotQuery` in `message_field.cpp:933-940`).
pub fn prevent_inline_bot_query(text: &str) -> String {
    let first = text.chars().find(|&c| c != '\u{FFFC}');
    if first == Some('@') {
        format!(" {text}")
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// Inline query debounce, cache, pagination & switch_pm (inline_results_widget.cpp)
// ---------------------------------------------------------------------------

/// Header action button returned alongside inline results (`switch_pm` or `switch_webview`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitchPmButton {
    /// Switch to the bot's private chat with `/start <start_param>` (`inlineBotSwitchPM`).
    StartPm {
        /// Button label.
        text: String,
        /// Start token passed when opening the bot's PM.
        start_param: String,
    },
    /// Open a Mini App WebView (`inlineBotWebView`).
    WebView {
        /// Button label.
        text: String,
        /// Target WebView URL.
        url: String,
    },
}

impl SwitchPmButton {
    /// Returns the visible button label.
    pub fn text(&self) -> &str {
        match self {
            Self::StartPm { text, .. } | Self::WebView { text, .. } => text,
        }
    }
}

/// Cached results page(s) for a single inline query string (`InlineBots::Layout::CacheEntry`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InlineCacheEntry {
    /// Pagination cursor for the next page (empty when exhausted).
    pub next_offset: String,
    /// Optional `switch_pm` / `switch_webview` button.
    pub switch_pm: Option<SwitchPmButton>,
    /// Accumulated results across fetched pages.
    pub results: Vec<InlineResult>,
    /// Server-specified gallery hint (`messages.botResults.gallery`).
    pub gallery: bool,
    /// Server-specified cache lifetime in seconds (`cache_time`).
    pub cache_time: i32,
}

impl InlineCacheEntry {
    /// Returns `true` if the panel should be visible (at least one result or a `switch_pm` button).
    pub fn has_visible_content(&self) -> bool {
        !self.results.is_empty()
            || self
                .switch_pm
                .as_ref()
                .is_some_and(|b| !b.text().is_empty())
    }
}

/// Incoming page from `messages.getInlineBotResults`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineResultsPage {
    /// Query string this response belongs to.
    pub query: String,
    /// Pagination cursor for the next page.
    pub next_offset: Option<String>,
    /// Optional `switch_pm` or `switch_webview` button.
    pub switch_pm: Option<SwitchPmButton>,
    /// Server gallery mode flag.
    pub gallery: bool,
    /// Cache TTL in seconds.
    pub cache_time: i32,
    /// Results in this page (invalid entries are filtered out just like `Result::Create`).
    pub results: Vec<InlineResult>,
}

/// Outgoing `messages.getInlineBotResults` request emitted by [`InlineQueryController`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetInlineBotResultsRequest {
    /// Inline bot user id.
    pub bot_id: UserId,
    /// Chat peer where results will be sent.
    pub peer_id: PeerId,
    /// Query text.
    pub query: String,
    /// Pagination offset (`""` for first page).
    pub offset: String,
}

/// Instruction emitted by [`InlineQueryController::on_query_changed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineQueryAction {
    /// Nothing changed.
    NoOp,
    /// Cached entry exists; immediately display it (cancelling any pending debounce/request).
    ShowCached {
        /// Cancel the previously in-flight request, if any.
        cancel_in_flight: bool,
    },
    /// Schedule a one-shot debounce timer for `delay_ms` (400 ms).
    ScheduleDebounce {
        /// Debounce duration in milliseconds (`INLINE_BOT_REQUEST_DELAY_MS`).
        delay_ms: u64,
        /// Cancel the previously in-flight request, if any.
        cancel_in_flight: bool,
    },
}

/// Pure state machine porting `InlineBots::Layout::Widget` debounce, cache, and pagination
/// (`inline_bots/inline_results_widget.cpp:350-481`).
#[derive(Debug, Default, Clone)]
pub struct InlineQueryController {
    bot_id: Option<UserId>,
    peer_id: Option<PeerId>,
    active_query: String,
    next_query: String,
    in_flight: bool,
    debounce_deadline_ms: Option<u64>,
    cache: HashMap<String, InlineCacheEntry>,
}

impl InlineQueryController {
    /// Creates a new idle controller.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the currently targeted inline bot, if any.
    pub const fn bot_id(&self) -> Option<UserId> {
        self.bot_id
    }

    /// Returns the active query string.
    pub fn active_query(&self) -> &str {
        &self.active_query
    }

    /// Returns `true` when an RPC request is currently in flight.
    pub const fn is_requesting(&self) -> bool {
        self.in_flight
    }

    /// Returns the cached entry for the active query, if any.
    pub fn current_entry(&self) -> Option<&InlineCacheEntry> {
        self.cache.get(&self.active_query)
    }

    /// Returns the pending debounce deadline timestamp in milliseconds, if scheduled.
    pub const fn debounce_deadline_ms(&self) -> Option<u64> {
        self.debounce_deadline_ms
    }

    /// Resets state when switching away from or changing the inline bot (`inlineBotChanged`).
    pub fn clear_bot(&mut self) {
        self.bot_id = None;
        self.active_query.clear();
        self.next_query.clear();
        self.in_flight = false;
        self.debounce_deadline_ms = None;
        self.cache.clear();
    }

    /// Handles a user query update (`Widget::queryInlineBot`, `inline_results_widget.cpp:428-452`).
    pub fn on_query_changed(
        &mut self,
        bot_id: UserId,
        peer_id: PeerId,
        query: impl Into<String>,
        now_ms: u64,
    ) -> InlineQueryAction {
        let query = query.into();
        let mut force = false;
        self.peer_id = Some(peer_id);
        if self.bot_id != Some(bot_id) {
            self.clear_bot();
            self.bot_id = Some(bot_id);
            force = true;
        }

        if self.active_query != query || self.next_query != query || force {
            let cancel_in_flight = std::mem::replace(&mut self.in_flight, false);
            if self.cache.contains_key(&query) {
                self.debounce_deadline_ms = None;
                self.active_query = query.clone();
                self.next_query = query;
                return InlineQueryAction::ShowCached { cancel_in_flight };
            }
            self.next_query = query;
            self.debounce_deadline_ms = Some(now_ms.saturating_add(INLINE_BOT_REQUEST_DELAY_MS));
            return InlineQueryAction::ScheduleDebounce {
                delay_ms: INLINE_BOT_REQUEST_DELAY_MS,
                cancel_in_flight,
            };
        }

        InlineQueryAction::NoOp
    }

    /// Fires when the debounce timer reaches `now_ms`, returning a network request if due.
    pub fn on_timer_tick(&mut self, now_ms: u64) -> Option<GetInlineBotResultsRequest> {
        let deadline = self.debounce_deadline_ms?;
        if now_ms < deadline {
            return None;
        }
        self.debounce_deadline_ms = None;
        self.build_next_request()
    }

    /// Called when the user scrolls near the bottom of the results panel (`Widget::onScroll`),
    /// returning a paginated request if `next_offset` is non-empty and nothing is in flight.
    pub fn on_scroll_bottom(&mut self) -> Option<GetInlineBotResultsRequest> {
        if self.debounce_deadline_ms.is_some() {
            return None;
        }
        self.build_next_request()
    }

    /// Core request builder matching `Widget::onInlineRequest` (`inline_results_widget.cpp:454-481`).
    fn build_next_request(&mut self) -> Option<GetInlineBotResultsRequest> {
        if self.in_flight {
            return None;
        }
        let bot_id = self.bot_id?;
        let peer_id = self.peer_id?;
        self.active_query = self.next_query.clone();

        let next_offset = if let Some(entry) = self.cache.get(&self.active_query) {
            if entry.next_offset.is_empty() {
                return None;
            }
            entry.next_offset.clone()
        } else {
            String::new()
        };

        self.in_flight = true;
        Some(GetInlineBotResultsRequest {
            bot_id,
            peer_id,
            query: self.active_query.clone(),
            offset: next_offset,
        })
    }

    /// Applies an incoming [`InlineResultsPage`] (`Widget::inlineResultsDone`, lines 369-426).
    ///
    /// Returns the number of valid results newly added to the cache entry.
    pub fn on_results_received(&mut self, page: InlineResultsPage) -> usize {
        self.in_flight = false;
        let entry = self.cache.entry(page.query).or_default();
        entry.next_offset = page.next_offset.unwrap_or_default();
        if page.switch_pm.is_some() {
            entry.switch_pm = page.switch_pm;
        }
        entry.gallery = page.gallery;
        entry.cache_time = page.cache_time;

        let mut added = 0usize;
        for res in page.results {
            if res.is_valid() {
                entry.results.push(res);
                added += 1;
            }
        }
        if added == 0 || !entry.has_visible_content() {
            entry.next_offset.clear();
        }
        added
    }

    /// Marks the in-flight request as failed (`inline_results_widget.cpp:476-480`).
    pub fn on_request_failed(&mut self) {
        self.in_flight = false;
    }
}
