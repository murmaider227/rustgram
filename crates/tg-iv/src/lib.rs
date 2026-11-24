//! Telegram Instant View (`tg-iv`) for Rustgram.
//!
//! Port of Telegram Desktop's Instant View subsystem (`Telegram/SourceFiles/iv/`):
//! - [`model`]: Rust domain model of Telegram API layer 233 `Page`, `PageBlock`, and `RichText`
//!   (`iv_data.cpp`, `iv_rich_page.h`, `iv_rich_page.cpp`)
//! - [`grouped_layout`]: Telegram album / collage layout algorithm (`ui/grouped_layout.cpp`)
//! - [`prepare`]: XSS-safe `Page -> HTML` generator (`iv_rich_message_html_export.cpp`)
//! - [`controller`]: Link classification, zoom state, JS bridge protocol, and navigation history
//!   (`iv_controller.cpp`, `iv_zoom_controls.cpp`, `iv_instance.cpp`)
//! - [`serve`]: Local custom-scheme resource server with HTTP `Range` support
//!   (`lib_webview/webview/webview_interface.h`)
//! - [`assets`]: Embedded GPL-3.0 CSS and JavaScript resources
//! - [`rich_page`]: Editable [`RichPage`] model and Layer-233 TL `PageBlock` / `RichMessage` /
//!   `InputRichMessage` serialization (`iv_rich_page.cpp`, `iv_rich_message_serializer.cpp`)
//! - [`markdown`]: Native Markdown parser/serializer and pure-Rust TeX math layout engine
//!   (`iv/markdown/*`)
//! - [`editor`]: Headless rich document editor model with undo/redo, cursor/selection, tables,
//!   lists, inline styling, clipboard import, and math box validation (`iv/editor/*`)
//! - [`cached_media`]: LRU photo, document, and geo-map tile cache with content-type sniffing
//!   and disk backing (`iv_cached_media.cpp`)
//! - [`html_export`]: Self-contained sanitized HTML export of [`rich_page::RichPage`] articles
//!   (`iv_rich_message_html_export.cpp`)
//! - [`search`]: In-page Unicode search controller with overlapping/multi-block indexing,
//!   cycling, and snippet highlighting (`iv_search_controller.cpp`)

pub mod assets;
pub mod cached_media;
pub mod controller;
pub mod editor;
pub mod error;
pub mod grouped_layout;
pub mod html_export;
pub mod markdown;
pub mod model;
pub mod prepare;
pub mod rich_page;
pub mod search;
pub mod serve;

pub use cached_media::{
    CachedMediaKey, CachedMediaStats, DEFAULT_CACHE_MAX_BYTES, DEFAULT_CACHE_MAX_ENTRIES,
    GEO_POINT_SCALE, GEO_POINT_ZOOM_MIN, IvCachedMedia, MapTileKey, cached_page_document_copy_text,
    cached_page_document_subtitle_text, cached_page_document_title_text, normalize_geo_zoom,
    serialize_native_iv_channel_context, sniff_content_type,
};
pub use controller::{
    BridgeMessage, ControllerAction, HistoryEntry, HostCommand, IvController, LinkTarget,
    MediaKind, NavigationDecision, ScrollPosition, ShortcutAction, ZOOM_DEFAULT, ZOOM_MAX,
    ZOOM_MIN, ZOOM_SMALL_STEP, ZOOM_STEP, ZOOM_TINY_STEP, ZoomState, ZoomStep, classify_link,
    default_iv_zoom, https_to_tonsite, is_good_tonsite_url, prefer_iv_for_uri, resolve_iv_zoom,
    tonsite_to_https,
};
pub use error::IvError;
pub use html_export::{
    CLIPBOARD_FORMULA_LIMIT, CLIPBOARD_MEDIA_LIMIT, DiskExportResult, HtmlExportOptions,
    HtmlExportResult, MAX_FOLDER_NAME_LENGTH, encode_base64, export_rich_page_html,
    export_rich_page_to_dir, rich_blocks_clipboard_html, safe_export_url, sanitize_file_name_part,
    sanitize_raw_html_fragment, title_from_page,
};
pub use model::{
    DateFlags, DocumentMeta, GeoPoint, ListItemContent, Page, PageBlock, PageCaption, PageChannel,
    PageData, PageListItem, PageListOrderedItem, PageRelatedArticle, PageTableCell, PageTableRow,
    PhotoMeta, RichText, TableAlign, TableValign, TaskState, normalize_fragment_id,
    ordered_alpha_text, ordered_roman_text, site_name_from_url,
};
pub use prepare::{
    DEFAULT_EMBED_ALLOWLIST, EmbedPolicy, PreparedEmbed, PreparedPage, RenderOptions, escape_html,
    format_utc_date, is_allowed_embed_url, prepare_page, safe_url,
};
pub use search::{
    IvSearchController, SearchMatch, SearchOptions, SearchSnippet, SearchSource,
    extract_search_sources_from_page, extract_search_sources_from_rich_page, find_matches_in_str,
    scan_search_entries,
};
pub use serve::{
    DEFAULT_IV_CSP, IvServer, MediaBlob, MediaProvider, MemoryMediaStore, RangeParse,
    ResourceResponse, parse_range_header,
};
