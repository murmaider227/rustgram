//! Mock [`WebViewBackend`] serving sample Instant View articles and a local demo Mini App.
//!
//! Uses synthetic sample data (no real people's names) and local `rustgram-internal://` assets.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tg_iv::{
    DateFlags, DocumentMeta, GeoPoint, ListItemContent, MemoryMediaStore, Page, PageBlock,
    PageCaption, PageChannel, PageListItem, PageListOrderedItem, PageRelatedArticle, PageTableCell,
    PageTableRow, PhotoMeta, RichText, TableAlign, TableValign, TaskState,
};
use tg_webview::{InlineChatType, SchemeRouter};

use crate::webview::backend::{
    BotInfo, IvPageBundle, PreparedInlineMessageInfo, WebViewBackend, WebViewBackendError,
    WebViewLaunchInfo, WebViewRequest,
};
use tg_app_core::stand_in::types::PeerId;

/// Canonical URL of the primary sample Instant View article.
pub const SAMPLE_IV_URL: &str = "https://telegra.ph/Rustgram-Instant-View-Sample-11-22";

/// Canonical URL of the secondary sample Instant View article (for in-window history navigation).
pub const SAMPLE_IV_SECOND_URL: &str = "https://telegra.ph/Rustgram-Architecture-Notes-11-22";

/// Bot user ID of the local demo Mini App bot.
pub use tg_app_core::core_types::session_ids::SAMPLE_BOT_ID;

/// Base URL prefix for the local demo Mini App custom scheme.
pub const SAMPLE_MINI_APP_PREFIX: &str = "rustgram-internal://miniapp/";

/// Entry URL for the local demo Mini App HTML page.
pub const SAMPLE_MINI_APP_URL: &str = "rustgram-internal://miniapp/index.html";

/// Local fake [`WebViewBackend`] for offline previews, screenshots, and unit tests.
#[derive(Debug, Default)]
pub struct MockWebViewBackend {
    /// Payloads delivered via [`WebViewBackend::send_web_app_data`].
    pub sent_data: Vec<(u64, String, String)>,
    /// Custom method invocations `(bot_id, method, params)`.
    pub custom_method_calls: Vec<(u64, String, Value)>,
    /// Channel IDs joined from Instant View banners.
    pub joined_channels: Vec<i64>,
    /// Bot IDs removed from the attachment/side menu.
    pub removed_bots: Vec<u64>,
    /// Simulated per-bot CloudStorage key-value store.
    pub cloud_storage: HashMap<(u64, String), String>,
    /// Prepared inline message previews keyed by `(bot_id, prepared_id)`.
    pub prepared_messages: HashMap<(u64, String), PreparedInlineMessageInfo>,
    /// Recorded `send_prepared_inline_message` calls `(bot_id, peer_id, query_id, result_id)`.
    pub sent_prepared_messages: Vec<(u64, PeerId, i64, String)>,
    /// Chats offered as prepared-message recipients `(peer, title, chat type)`.
    pub prepared_recipients: Vec<(PeerId, String, InlineChatType)>,
    /// Optional per-account bot local storage directory for persistence tests.
    pub bot_storage_root: Option<PathBuf>,
    /// Next synthetic query ID for `request_web_view`.
    next_query_id: u64,
}

impl MockWebViewBackend {
    /// Creates a new mock backend populated with sample CloudStorage keys.
    #[must_use]
    pub fn new() -> Self {
        let mut backend = Self {
            next_query_id: 1000,
            ..Self::default()
        };
        backend.cloud_storage.insert(
            (SAMPLE_BOT_ID, "plan".to_owned()),
            "Workspace Starter".to_owned(),
        );
        backend
            .cloud_storage
            .insert((SAMPLE_BOT_ID, "cart_items".to_owned()), "2".to_owned());
        backend.prepared_recipients = vec![
            (
                PeerId::from_user(500_001),
                "Sample Contact".to_owned(),
                InlineChatType::Users,
            ),
            (
                PeerId::from_channel(600_001),
                "Sample Group".to_owned(),
                InlineChatType::Groups,
            ),
            (
                PeerId::from_channel(600_002),
                "Sample Channel".to_owned(),
                InlineChatType::Channels,
            ),
        ];
        backend
    }

    /// Returns metadata for the default sample Mini App bot.
    #[must_use]
    pub fn sample_bot_info() -> BotInfo {
        BotInfo {
            id: SAMPLE_BOT_ID,
            username: "catalog_demo_bot".to_owned(),
            title: "Catalog Bot".to_owned(),
            verified: true,
            in_attach_menu: true,
            in_main_menu: false,
            allow_clipboard_read: true,
            is_game: false,
        }
    }

    /// Builds a [`SchemeRouter`] serving the local demo Mini App under `rustgram-internal://miniapp/`.
    #[must_use]
    pub fn build_demo_mini_app_router() -> SchemeRouter {
        let mut router = SchemeRouter::default();
        router.set_data_url_prefix(SAMPLE_MINI_APP_PREFIX);
        router.add_html("index.html", DEMO_MINI_APP_HTML);
        router.add_static(
            "miniapp.css",
            "text/css; charset=utf-8",
            DEMO_MINI_APP_CSS.as_bytes(),
        );
        router.add_static(
            "miniapp.js",
            "application/javascript; charset=utf-8",
            DEMO_MINI_APP_JS.as_bytes(),
        );
        router
    }
}

impl WebViewBackend for MockWebViewBackend {
    fn fetch_iv_page(
        &self,
        url: &str,
        webpage_id: Option<i64>,
    ) -> Result<Option<IvPageBundle>, WebViewBackendError> {
        let base = url.split('#').next().unwrap_or(url).trim_end_matches('/');
        if webpage_id == Some(777) || base.eq_ignore_ascii_case(SAMPLE_IV_SECOND_URL) {
            return Ok(Some(IvPageBundle {
                page: build_secondary_iv_page(),
                media: Arc::new(build_sample_media_store()),
            }));
        }
        if webpage_id == Some(9_876_543_210)
            || base.eq_ignore_ascii_case(SAMPLE_IV_URL)
            || base.contains("telegra.ph/Rustgram")
        {
            return Ok(Some(IvPageBundle {
                page: build_primary_iv_page(),
                media: Arc::new(build_sample_media_store()),
            }));
        }
        Ok(None)
    }

    fn request_web_view(
        &mut self,
        request: WebViewRequest,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.next_query_id = self.next_query_id.saturating_add(1);
        let mut bot = Self::sample_bot_info();
        if request.bot_id != 0 {
            bot.id = request.bot_id;
        }
        let url = request
            .url_override
            .unwrap_or_else(|| SAMPLE_MINI_APP_URL.to_owned());
        let local_routes = if url.starts_with(SAMPLE_MINI_APP_PREFIX) {
            Some(Arc::new(Self::build_demo_mini_app_router()))
        } else {
            None
        };
        Ok(WebViewLaunchInfo {
            bot,
            url,
            query_id: self.next_query_id,
            fullscreen: request.fullscreen,
            same_origin: false,
            local_routes,
        })
    }

    fn send_web_app_data(
        &mut self,
        bot_id: u64,
        button_text: &str,
        data: &str,
    ) -> Result<(), WebViewBackendError> {
        self.sent_data
            .push((bot_id, button_text.to_owned(), data.to_owned()));
        Ok(())
    }

    fn invoke_custom_method(
        &mut self,
        bot_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.custom_method_calls
            .push((bot_id, method.to_owned(), params.clone()));
        match method {
            "saveStorageValue" => {
                let key = params
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "KEY_INVALID".to_owned())?;
                match params.get("value") {
                    Some(Value::String(val)) => {
                        self.cloud_storage
                            .insert((bot_id, key.to_owned()), val.clone());
                        Ok(Value::Bool(true))
                    }
                    Some(Value::Null) | None => {
                        self.cloud_storage.remove(&(bot_id, key.to_owned()));
                        Ok(Value::Bool(true))
                    }
                    _ => Err("VALUE_INVALID".to_owned()),
                }
            }
            "getStorageValues" => {
                let mut out = serde_json::Map::new();
                if let Some(keys) = params.get("keys").and_then(Value::as_array) {
                    for k in keys.iter().filter_map(Value::as_str) {
                        let val = self
                            .cloud_storage
                            .get(&(bot_id, k.to_owned()))
                            .cloned()
                            .unwrap_or_default();
                        out.insert(k.to_owned(), Value::String(val));
                    }
                }
                Ok(Value::Object(out))
            }
            "getCatalogInfo" => Ok(json!({
                "catalog": "Rustgram Sample Store",
                "items": 3,
                "currency": "XTR",
                "status": "ready"
            })),
            _ => Ok(json!({
                "ok": true,
                "method": method,
                "echo": params
            })),
        }
    }

    fn join_channel(&mut self, channel_id: i64) -> Result<(), WebViewBackendError> {
        if !self.joined_channels.contains(&channel_id) {
            self.joined_channels.push(channel_id);
        }
        Ok(())
    }

    fn remove_bot_from_menu(&mut self, bot_id: u64) -> Result<(), WebViewBackendError> {
        if !self.removed_bots.contains(&bot_id) {
            self.removed_bots.push(bot_id);
        }
        Ok(())
    }

    fn get_prepared_inline_message(
        &mut self,
        bot_id: u64,
        prepared_id: &str,
    ) -> Result<PreparedInlineMessageInfo, WebViewBackendError> {
        if prepared_id.trim().is_empty() {
            return Err(WebViewBackendError::new(
                "MESSAGE_EXPIRED",
                "Prepared inline message identifier is empty.",
            ));
        }
        if let Some(found) = self
            .prepared_messages
            .get(&(bot_id, prepared_id.to_owned()))
            .cloned()
        {
            return Ok(found);
        }
        self.next_query_id = self.next_query_id.saturating_add(1);
        Ok(PreparedInlineMessageInfo {
            prepared_id: prepared_id.to_owned(),
            query_id: self.next_query_id as i64,
            result_id: format!("res_{prepared_id}"),
            preview_title: "Catalog Bot".to_owned(),
            preview_text: format!("Prepared message #{prepared_id}"),
            allowed_peer_types: vec![InlineChatType::Users, InlineChatType::Groups],
        })
    }

    fn send_prepared_inline_message(
        &mut self,
        bot_id: u64,
        peer_id: PeerId,
        query_id: i64,
        result_id: &str,
    ) -> Result<(), WebViewBackendError> {
        self.sent_prepared_messages
            .push((bot_id, peer_id, query_id, result_id.to_owned()));
        Ok(())
    }

    fn prepared_message_recipients(
        &self,
        _bot_id: u64,
        allowed: &[InlineChatType],
    ) -> Vec<(PeerId, String)> {
        self.prepared_recipients
            .iter()
            .filter(|(_, _, ty)| allowed.is_empty() || allowed.contains(ty))
            .map(|(peer, title, _)| (*peer, title.clone()))
            .collect()
    }

    fn bot_storage_dir(&self) -> Option<PathBuf> {
        self.bot_storage_root.clone()
    }
}

/// Builds the primary sample Instant View [`Page`] adapted from `tg-iv`'s sample fixture.
#[must_use]
pub fn build_primary_iv_page() -> Page {
    Page {
        id: 9_876_543_210,
        hash: 42,
        url: SAMPLE_IV_URL.to_owned(),
        site_name: Some("Telegraph".to_owned()),
        part: false,
        rtl: false,
        v2: true,
        views: Some(12_345),
        photos: vec![
            PhotoMeta {
                id: 101,
                width: 1200,
                height: 540,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 102,
                width: 800,
                height: 520,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 103,
                width: 600,
                height: 520,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 104,
                width: 900,
                height: 520,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 105,
                width: 96,
                height: 96,
                stripped_thumb: None,
            },
        ],
        documents: vec![
            DocumentMeta {
                id: 201,
                mime_type: "video/mp4".to_owned(),
                file_name: "clip.mp4".to_owned(),
                width: 1280,
                height: 720,
                duration: 18,
                is_video: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 202,
                mime_type: "audio/mpeg".to_owned(),
                file_name: "episode.mp3".to_owned(),
                duration: 245,
                title: "Building Rustgram in Rust".to_owned(),
                performer: "Rustgram Contributors".to_owned(),
                is_audio: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 203,
                mime_type: "image/svg+xml".to_owned(),
                file_name: "badge.svg".to_owned(),
                width: 20,
                height: 20,
                ..Default::default()
            },
        ],
        blocks: vec![
            PageBlock::Cover(Box::new(PageBlock::Photo {
                photo_id: 101,
                caption: PageCaption {
                    text: RichText::plain("Instant View cover artwork rendered over rustgram-internal://"),
                    credit: RichText::plain("Artwork: Rustgram Team"),
                },
                url: None,
                webpage_id: None,
                spoiler: false,
            })),
            PageBlock::Kicker(RichText::plain("ENGINEERING NOTES")),
            PageBlock::Title(RichText::plain("Rustgram Instant View Renderer")),
            PageBlock::Subtitle(RichText::plain(
                "A native Rust WebView2 host for Telegram Instant View pages with live Day/Night theming",
            )),
            PageBlock::AuthorDate {
                author: RichText::plain("Rustgram Engineering"),
                published_date: 1_795_305_600,
            },
            PageBlock::Channel(PageChannel {
                id: 1_000_042,
                title: "Rustgram Updates".to_owned(),
                username: Some("rustgram_news".to_owned()),
                joined: false,
            }),
            PageBlock::Anchor {
                name: "intro".to_owned(),
            },
            PageBlock::Header(RichText::plain("Rich Text & Link Routing")),
            PageBlock::Paragraph(RichText::concat(vec![
                RichText::plain("Instant View supports "),
                RichText::bold(RichText::plain("bold")),
                RichText::plain(", "),
                RichText::italic(RichText::plain("italic")),
                RichText::plain(", "),
                RichText::underline(RichText::plain("underline")),
                RichText::plain(", "),
                RichText::strike(RichText::plain("strikethrough")),
                RichText::plain(", "),
                RichText::fixed(RichText::plain("tg_iv::IvServer")),
                RichText::plain(", and "),
                RichText::marked(RichText::plain("highlighted spans")),
                RichText::plain(". Try an "),
                RichText::Url {
                    text: Box::new(RichText::plain("in-page anchor (#more-details)")),
                    url: "#more-details".to_owned(),
                    webpage_id: 0,
                },
                RichText::plain(", a "),
                RichText::Url {
                    text: Box::new(RichText::plain("linked Instant View article")),
                    url: SAMPLE_IV_SECOND_URL.to_owned(),
                    webpage_id: 777,
                },
                RichText::plain(", an internal "),
                RichText::Url {
                    text: Box::new(RichText::plain("tg:// channel link")),
                    url: "tg://resolve?domain=rustgram_news".to_owned(),
                    webpage_id: 0,
                },
                RichText::plain(", or an "),
                RichText::Url {
                    text: Box::new(RichText::plain("external website")),
                    url: "https://example.org/rustgram".to_owned(),
                    webpage_id: 0,
                },
                RichText::plain(" published "),
                RichText::Date {
                    text: Box::new(RichText::plain("Nov 22, 2026")),
                    date: 1_795_305_600,
                    flags: DateFlags {
                        long_date: true,
                        ..Default::default()
                    },
                },
                RichText::plain("."),
            ])),
            PageBlock::Subheader(RichText::plain("Code & Pullquote")),
            PageBlock::Preformatted {
                text: RichText::plain(
                    "let prepared = tg_iv::prepare_page(&page, &options);\nlet server = tg_iv::IvServer::new(prepared);",
                ),
                language: "rust".to_owned(),
            },
            PageBlock::Blockquote {
                text: RichText::plain(
                    "Every link click is classified in Rust before any navigation occurs: anchors scroll smoothly, IV links push onto history, tg:// links return to the app, and external URLs launch via tg-platform.",
                ),
                caption: RichText::plain("Rustgram WebView Architecture"),
                collapsed: false,
            },
            PageBlock::Header(RichText::plain("Photo Collage & Media Viewer")),
            PageBlock::Collage {
                items: vec![
                    PageBlock::Photo {
                        photo_id: 102,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                    PageBlock::Photo {
                        photo_id: 103,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                    PageBlock::Photo {
                        photo_id: 104,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                ],
                caption: PageCaption {
                    text: RichText::plain(
                        "Click any photograph to open Rustgram's native media viewer via callback.",
                    ),
                    credit: RichText::Empty,
                },
            },
            PageBlock::List {
                items: vec![
                    PageListItem {
                        task_state: TaskState::Checked,
                        content: ListItemContent::Text(RichText::plain(
                            "Serve HTML, CSS, JS, and media over rustgram-internal://",
                        )),
                    },
                    PageListItem {
                        task_state: TaskState::Checked,
                        content: ListItemContent::Text(RichText::plain(
                            "Back/forward history stack with scroll position memory",
                        )),
                    },
                    PageListItem {
                        task_state: TaskState::Checked,
                        content: ListItemContent::Text(RichText::plain(
                            "Day and Night palette synced with tg-theme",
                        )),
                    },
                ],
            },
            PageBlock::OrderedList {
                reversed: false,
                start: Some(1),
                list_type: Some("1".to_owned()),
                items: vec![
                    PageListOrderedItem::text(
                        Some("1".to_owned()),
                        RichText::plain("Fetch Instant View Page via WebViewBackend"),
                    ),
                    PageListOrderedItem::text(
                        Some("2".to_owned()),
                        RichText::plain("Render XSS-safe HTML with tg_iv::prepare_page"),
                    ),
                    PageListOrderedItem::text(
                        Some("3".to_owned()),
                        RichText::plain("Host inside WebView2 with strict navigation policy"),
                    ),
                ],
            },
            PageBlock::Anchor {
                name: "more-details".to_owned(),
            },
            PageBlock::Header(RichText::plain("Subsystem Table & Details")),
            PageBlock::Table {
                title: RichText::plain("Instant View Pipeline"),
                bordered: true,
                striped: true,
                compact: false,
                rows: vec![
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::plain("Component")),
                                header: true,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Middle,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Responsibility")),
                                header: true,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Middle,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Crate")),
                                header: true,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Right,
                                valign: TableValign::Middle,
                            },
                        ],
                    },
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::fixed(RichText::plain("IvServer"))),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Local resource & Range streaming")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::fixed(RichText::plain("tg-iv"))),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Right,
                                valign: TableValign::Top,
                            },
                        ],
                    },
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::fixed(RichText::plain("WebView"))),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("WebView2 child HWND & IPC bridge")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::fixed(RichText::plain("tg-webview"))),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Right,
                                valign: TableValign::Top,
                            },
                        ],
                    },
                ],
            },
            PageBlock::Details {
                title: RichText::plain("Technical Details"),
                open: true,
                blocks: vec![PageBlock::Paragraph(RichText::plain(
                    "Zoom controls adjust both WebView2's native zoom factor and the page zoom style, supporting 25% to 400% with Ctrl++, Ctrl+-, and Ctrl+0.",
                ))],
            },
            PageBlock::Map {
                geo: GeoPoint {
                    lat: 37.7749,
                    lon: -122.4194,
                    access_hash: 0,
                },
                zoom: 13,
                width: 600,
                height: 240,
                caption: PageCaption {
                    text: RichText::plain("Sample Map Location"),
                    credit: RichText::plain("Map Preview"),
                },
            },
            PageBlock::RelatedArticles {
                title: RichText::plain("Related Articles"),
                articles: vec![PageRelatedArticle {
                    url: SAMPLE_IV_SECOND_URL.to_owned(),
                    webpage_id: 777,
                    title: Some("Rustgram Architecture Notes".to_owned()),
                    description: Some(
                        "Explore how Instant View history, theme sync, and Mini App panels fit together."
                            .to_owned(),
                    ),
                    photo_id: Some(102),
                    author: Some("Rustgram Editorial".to_owned()),
                    published_date: Some(1_795_305_600),
                }],
            },
            PageBlock::Footer(RichText::plain("Sample Instant View document for Rustgram.")),
        ],
    }
}

/// Builds the secondary linked Instant View [`Page`] for testing in-window back/forward history.
#[must_use]
pub fn build_secondary_iv_page() -> Page {
    Page {
        id: 777,
        hash: 7,
        url: SAMPLE_IV_SECOND_URL.to_owned(),
        site_name: Some("Telegraph".to_owned()),
        part: false,
        rtl: false,
        v2: true,
        views: Some(4_210),
        photos: vec![PhotoMeta {
            id: 102,
            width: 800,
            height: 520,
            stripped_thumb: None,
        }],
        documents: Vec::new(),
        blocks: vec![
            PageBlock::Kicker(RichText::plain("ARCHITECTURE")),
            PageBlock::Title(RichText::plain("Rustgram Architecture Notes")),
            PageBlock::Subtitle(RichText::plain(
                "Secondary Instant View page demonstrating in-window navigation history",
            )),
            PageBlock::AuthorDate {
                author: RichText::plain("Rustgram Editorial"),
                published_date: 1_795_305_600,
            },
            PageBlock::Paragraph(RichText::concat(vec![
                RichText::plain(
                    "Use the Back button in the Instant View title bar to return to the previous article and restore its scroll position, or click ",
                ),
                RichText::Url {
                    text: Box::new(RichText::plain("Rustgram Instant View Renderer")),
                    url: SAMPLE_IV_URL.to_owned(),
                    webpage_id: 9_876_543_210,
                },
                RichText::plain(" to open it again."),
            ])),
            PageBlock::Photo {
                photo_id: 102,
                caption: PageCaption {
                    text: RichText::plain("System component diagram preview"),
                    credit: RichText::plain("Rustgram Docs"),
                },
                url: None,
                webpage_id: None,
                spoiler: false,
            },
            PageBlock::Footer(RichText::plain("End of secondary sample article.")),
        ],
    }
}

/// Builds an in-memory media store with clean vector artwork for the sample Instant View photos.
#[must_use]
pub fn build_sample_media_store() -> MemoryMediaStore {
    let mut store = MemoryMediaStore::new();
    store.insert_photo(
        101,
        "image/svg+xml; charset=utf-8",
        SVG_COVER_ART.as_bytes(),
    );
    store.insert_photo(
        102,
        "image/svg+xml; charset=utf-8",
        SVG_CARD_BLUE.as_bytes(),
    );
    store.insert_photo(
        103,
        "image/svg+xml; charset=utf-8",
        SVG_CARD_TEAL.as_bytes(),
    );
    store.insert_photo(
        104,
        "image/svg+xml; charset=utf-8",
        SVG_CARD_VIOLET.as_bytes(),
    );
    store.insert_photo(
        105,
        "image/svg+xml; charset=utf-8",
        SVG_AVATAR_BADGE.as_bytes(),
    );
    store.insert_document(
        203,
        "image/svg+xml; charset=utf-8",
        SVG_AVATAR_BADGE.as_bytes(),
    );
    store
}

const SVG_COVER_ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="540" viewBox="0 0 1200 540">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#1f6fb2"/>
      <stop offset="55%" stop-color="#2ea6e8"/>
      <stop offset="100%" stop-color="#58c7f3"/>
    </linearGradient>
    <linearGradient id="card" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#ffffff" stop-opacity="0.22"/>
      <stop offset="100%" stop-color="#ffffff" stop-opacity="0.06"/>
    </linearGradient>
  </defs>
  <rect width="1200" height="540" fill="url(#bg)" rx="16"/>
  <circle cx="1010" cy="110" r="190" fill="#ffffff" fill-opacity="0.09"/>
  <circle cx="190" cy="460" r="220" fill="#0d3b66" fill-opacity="0.18"/>
  <rect x="76" y="76" width="1048" height="388" rx="22" fill="url(#card)" stroke="#ffffff" stroke-opacity="0.28" stroke-width="2"/>
  <text x="128" y="195" font-family="Segoe UI, system-ui, sans-serif" font-size="26" font-weight="600" fill="#d9f3ff" letter-spacing="3">RUSTGRAM INSTANT VIEW</text>
  <text x="128" y="276" font-family="Segoe UI, system-ui, sans-serif" font-size="60" font-weight="700" fill="#ffffff">Fast Native Reading</text>
  <text x="128" y="336" font-family="Segoe UI, system-ui, sans-serif" font-size="28" fill="#e8f7ff">Served from rustgram-internal:// via tg_iv::IvServer &amp; WebView2</text>
  <rect x="128" y="376" width="210" height="44" rx="22" fill="#ffffff" fill-opacity="0.22"/>
  <text x="233" y="405" text-anchor="middle" font-family="Segoe UI, system-ui, sans-serif" font-size="18" font-weight="600" fill="#ffffff">Day / Night Sync</text>
  <rect x="356" y="376" width="210" height="44" rx="22" fill="#ffffff" fill-opacity="0.22"/>
  <text x="461" y="405" text-anchor="middle" font-family="Segoe UI, system-ui, sans-serif" font-size="18" font-weight="600" fill="#ffffff">XSS-Safe HTML5</text>
</svg>"##;

const SVG_CARD_BLUE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="800" height="520" viewBox="0 0 800 520">
  <defs><linearGradient id="g1" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#2b5876"/><stop offset="100%" stop-color="#4e89c4"/></linearGradient></defs>
  <rect width="800" height="520" fill="url(#g1)" rx="12"/>
  <rect x="64" y="64" width="672" height="392" rx="16" fill="#ffffff" fill-opacity="0.14"/>
  <text x="400" y="248" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="40" font-weight="700" fill="#ffffff">Instant View Layout</text>
  <text x="400" y="302" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="22" fill="#d9ecff">Photo #102 — Click to open Media Viewer</text>
</svg>"##;

const SVG_CARD_TEAL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="520" viewBox="0 0 600 520">
  <defs><linearGradient id="g2" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#136a8a"/><stop offset="100%" stop-color="#267871"/></linearGradient></defs>
  <rect width="600" height="520" fill="url(#g2)" rx="12"/>
  <circle cx="300" cy="215" r="88" fill="#ffffff" fill-opacity="0.16"/>
  <text x="300" y="226" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="36" font-weight="700" fill="#ffffff">Range 206</text>
  <text x="300" y="356" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="22" fill="#d4fff9">Photo #103</text>
</svg>"##;

const SVG_CARD_VIOLET: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="900" height="520" viewBox="0 0 900 520">
  <defs><linearGradient id="g3" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#4b3f72"/><stop offset="100%" stop-color="#7c68c4"/></linearGradient></defs>
  <rect width="900" height="520" fill="url(#g3)" rx="12"/>
  <rect x="80" y="90" width="740" height="340" rx="18" fill="#ffffff" fill-opacity="0.12"/>
  <text x="450" y="252" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="40" font-weight="700" fill="#ffffff">Grouped Album Grid</text>
  <text x="450" y="308" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="22" fill="#ebe4ff">Photo #104</text>
</svg>"##;

const SVG_AVATAR_BADGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96">
  <circle cx="48" cy="48" r="46" fill="#40a7e3"/>
  <text x="48" y="61" text-anchor="middle" font-family="Segoe UI, sans-serif" font-size="36" font-weight="700" fill="#ffffff">TG</text>
</svg>"##;

/// Local demo Bot Mini App HTML page exercising the full `Telegram.WebView` bridge.
pub const DEMO_MINI_APP_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Catalog Bot Mini App</title>
  <link rel="stylesheet" href="rustgram-internal://miniapp/miniapp.css">
</head>
<body>
  <main class="app">
    <section class="hero">
      <div class="hero-badge">MINI APP DEMO</div>
      <h1>Catalog Bot Storefront</h1>
      <p class="subtitle">Local WebView2 Mini App served from <code>rustgram-internal://miniapp/index.html</code></p>
      <div class="status-row">
        <span class="pill" id="pill-theme">Theme: Syncing…</span>
        <span class="pill" id="pill-viewport">Viewport: —</span>
        <span class="pill" id="pill-fullscreen">Windowed</span>
      </div>
    </section>

    <section class="cards">
      <article class="card">
        <div class="card-icon">📦</div>
        <div class="card-body">
          <h3>Workspace Starter Pack</h3>
          <p>Cloud storage, instant templates, and webhook alerts.</p>
        </div>
        <div class="card-price">120 ★</div>
      </article>
      <article class="card">
        <div class="card-icon">🎨</div>
        <div class="card-body">
          <h3>Adaptive Theme Kit</h3>
          <p>Live Day &amp; Night palette parameters pushed from tg-theme.</p>
        </div>
        <div class="card-price">60 ★</div>
      </article>
    </section>

    <section class="section">
      <h2>Bridge Controls</h2>
      <div class="grid">
        <button id="btn-popup">Open tg-ui Popup</button>
        <button id="btn-close-confirm">Closing Confirm: OFF</button>
        <button id="btn-fullscreen">Toggle Fullscreen</button>
        <button id="btn-clipboard">Read Clipboard</button>
        <button id="btn-custom">Invoke Custom Method</button>
        <button id="btn-send-data">Send WebApp Data</button>
        <button id="btn-tg-link">Open t.me Link</button>
        <button id="btn-ext-link">Open External Link</button>
        <button id="btn-iv-link">Open in Instant View</button>
        <button id="btn-secondary-pos">Cycle Secondary Pos</button>
      </div>
    </section>

    <section class="section">
      <h2>Live Bridge Log</h2>
      <pre id="bridge-log"></pre>
    </section>
  </main>
  <script src="rustgram-internal://miniapp/miniapp.js"></script>
</body>
</html>
"##;

/// Stylesheet for the local demo Bot Mini App bound to Telegram Mini App theme CSS variables.
pub const DEMO_MINI_APP_CSS: &str = r##"
:root {
  --tg-theme-bg-color: #ffffff;
  --tg-theme-secondary-bg-color: #f1f1f1;
  --tg-theme-section-bg-color: #ffffff;
  --tg-theme-text-color: #000000;
  --tg-theme-hint-color: #707579;
  --tg-theme-link-color: #168acd;
  --tg-theme-button-color: #40a7e3;
  --tg-theme-button-text-color: #ffffff;
  --tg-theme-section-header-text-color: #168acd;
  --tg-theme-section-separator-color: #e6e6e6;
}
* { box-sizing: border-box; }
html, body {
  margin: 0;
  padding: 0;
  background: var(--tg-theme-secondary-bg-color);
  color: var(--tg-theme-text-color);
  font-family: "Segoe UI", system-ui, -apple-system, sans-serif;
  font-size: 13.5px;
  line-height: 1.4;
}
.app {
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.hero {
  background: var(--tg-theme-bg-color);
  border-radius: 12px;
  padding: 14px 16px;
  border: 1px solid var(--tg-theme-section-separator-color);
}
.hero-badge {
  display: inline-block;
  font-size: 10.5px;
  font-weight: 700;
  letter-spacing: 0.08em;
  padding: 3px 8px;
  border-radius: 999px;
  background: var(--tg-theme-button-color);
  color: var(--tg-theme-button-text-color);
  margin-bottom: 6px;
}
h1 {
  margin: 0 0 4px 0;
  font-size: 18px;
  font-weight: 700;
}
.subtitle {
  margin: 0 0 10px 0;
  font-size: 12px;
  color: var(--tg-theme-hint-color);
}
code {
  font-family: Consolas, monospace;
  font-size: 11.5px;
}
.status-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.pill {
  background: var(--tg-theme-secondary-bg-color);
  color: var(--tg-theme-hint-color);
  border-radius: 999px;
  padding: 3px 9px;
  font-size: 11.5px;
  font-weight: 600;
}
.cards {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.card {
  display: flex;
  align-items: center;
  gap: 12px;
  background: var(--tg-theme-bg-color);
  border: 1px solid var(--tg-theme-section-separator-color);
  border-radius: 12px;
  padding: 11px 14px;
}
.card-icon {
  width: 38px;
  height: 38px;
  border-radius: 10px;
  background: var(--tg-theme-secondary-bg-color);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 18px;
  flex-shrink: 0;
}
.card-body {
  flex: 1;
  min-width: 0;
}
.card-body h3 {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
}
.card-body p {
  margin: 2px 0 0 0;
  font-size: 12px;
  color: var(--tg-theme-hint-color);
}
.card-price {
  font-weight: 700;
  font-size: 13.5px;
  color: var(--tg-theme-link-color);
  white-space: nowrap;
}
.section {
  background: var(--tg-theme-bg-color);
  border: 1px solid var(--tg-theme-section-separator-color);
  border-radius: 12px;
  padding: 12px 14px;
}
.section h2 {
  margin: 0 0 9px 0;
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--tg-theme-section-header-text-color);
}
.grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 7px;
}
button {
  background: var(--tg-theme-button-color);
  color: var(--tg-theme-button-text-color);
  border: none;
  border-radius: 8px;
  padding: 8px 10px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: filter 0.12s ease;
}
button:hover { filter: brightness(0.92); }
pre {
  margin: 0;
  max-height: 150px;
  overflow-y: auto;
  font-family: Consolas, monospace;
  font-size: 11px;
  color: var(--tg-theme-hint-color);
  white-space: pre-wrap;
  word-break: break-word;
}
"##;

/// Client-side script for the local demo Bot Mini App interacting with `window.Telegram.WebView`.
pub const DEMO_MINI_APP_JS: &str = r##"(function () {
  'use strict';

  var logEl = document.getElementById('bridge-log');
  var pillTheme = document.getElementById('pill-theme');
  var pillViewport = document.getElementById('pill-viewport');
  var pillFullscreen = document.getElementById('pill-fullscreen');
  var btnCloseConfirm = document.getElementById('btn-close-confirm');

  var needConfirmation = false;
  var isFullscreen = false;
  var positions = ['left', 'right', 'top', 'bottom'];
  var posIndex = 0;
  var reqSeq = 1;

  function log(dir, type, data) {
    var ts = new Date().toLocaleTimeString();
    var extra = data === undefined ? '' : ' ' + JSON.stringify(data);
    logEl.textContent = '[' + ts + '] ' + dir + ' ' + type + extra + '\n' + logEl.textContent;
  }

  function post(type, data) {
    log('APP -> HOST', type, data);
    if (window.Telegram && window.Telegram.WebView) {
      window.Telegram.WebView.postEvent(type, data);
    }
  }

  function applyTheme(params) {
    if (!params) return;
    var root = document.documentElement.style;
    var mapping = {
      bg_color: '--tg-theme-bg-color',
      secondary_bg_color: '--tg-theme-secondary-bg-color',
      section_bg_color: '--tg-theme-section-bg-color',
      text_color: '--tg-theme-text-color',
      hint_color: '--tg-theme-hint-color',
      link_color: '--tg-theme-link-color',
      button_color: '--tg-theme-button-color',
      button_text_color: '--tg-theme-button-text-color',
      section_header_text_color: '--tg-theme-section-header-text-color',
      section_separator_color: '--tg-theme-section-separator-color'
    };
    Object.keys(mapping).forEach(function (k) {
      if (params[k]) {
        root.setProperty(mapping[k], params[k]);
      }
    });
    if (params.bg_color) {
      pillTheme.textContent = 'Theme: ' + params.bg_color;
    }
  }

  var hostEvents = [
    'theme_changed',
    'viewport_changed',
    'safe_area_changed',
    'content_safe_area_changed',
    'fullscreen_changed',
    'main_button_pressed',
    'secondary_button_pressed',
    'back_button_pressed',
    'settings_button_pressed',
    'popup_closed',
    'clipboard_text_received',
    'custom_method_invoked'
  ];

  if (window.Telegram && window.Telegram.WebView) {
    hostEvents.forEach(function (ev) {
      window.Telegram.WebView.onEvent(ev, function (type, data) {
        log('HOST -> APP', type, data);
        if (type === 'theme_changed' && data && data.theme_params) {
          applyTheme(data.theme_params);
        } else if (type === 'viewport_changed' && data) {
          pillViewport.textContent = 'Viewport: ' + Math.round(data.height || 0) + 'px';
        } else if (type === 'fullscreen_changed' && data) {
          isFullscreen = !!data.is_fullscreen;
          pillFullscreen.textContent = isFullscreen ? 'Fullscreen' : 'Windowed';
        } else if (type === 'main_button_pressed') {
          post('web_app_open_popup', {
            title: 'Checkout Confirmed',
            message: 'Proceed with 180 Stars order?',
            buttons: [
              { id: 'confirm_pay', type: 'ok' },
              { id: 'cancel_pay', type: 'cancel' }
            ]
          });
        } else if (type === 'secondary_button_pressed') {
          post('web_app_invoke_custom_method', {
            req_id: 'req-' + (reqSeq++),
            method: 'getCatalogInfo',
            params: {}
          });
        } else if (type === 'settings_button_pressed') {
          post('web_app_open_popup', {
            title: 'Mini App Settings',
            message: 'Settings button was triggered from the Bot Panel menu.',
            buttons: [{ id: 'close_settings', type: 'close' }]
          });
        }
      });
    });
  }

  document.getElementById('btn-popup').addEventListener('click', function () {
    post('web_app_open_popup', {
      title: 'Confirm Action',
      message: 'This dialog is rendered natively as a tg-ui BoxModal.',
      buttons: [
        { id: 'proceed', type: 'default', text: 'Proceed' },
        { id: 'remove', type: 'destructive', text: 'Reset' },
        { id: 'cancel', type: 'cancel' }
      ]
    });
  });

  btnCloseConfirm.addEventListener('click', function () {
    needConfirmation = !needConfirmation;
    btnCloseConfirm.textContent = 'Closing Confirm: ' + (needConfirmation ? 'ON' : 'OFF');
    post('web_app_setup_closing_behavior', { need_confirmation: needConfirmation });
  });

  document.getElementById('btn-fullscreen').addEventListener('click', function () {
    post(isFullscreen ? 'web_app_exit_fullscreen' : 'web_app_request_fullscreen');
  });

  document.getElementById('btn-clipboard').addEventListener('click', function () {
    post('web_app_read_text_from_clipboard', { req_id: 'clip-' + (reqSeq++) });
  });

  document.getElementById('btn-custom').addEventListener('click', function () {
    post('web_app_invoke_custom_method', {
      req_id: 'custom-' + (reqSeq++),
      method: 'getCatalogInfo',
      params: { section: 'featured' }
    });
  });

  document.getElementById('btn-send-data').addEventListener('click', function () {
    post('web_app_data_send', { data: JSON.stringify({ order: 'starter-pack', qty: 1 }) });
  });

  document.getElementById('btn-tg-link').addEventListener('click', function () {
    post('web_app_open_tg_link', { path_full: '/rustgram_news' });
  });

  document.getElementById('btn-ext-link').addEventListener('click', function () {
    post('web_app_open_link', { url: 'https://example.org/catalog' });
  });

  document.getElementById('btn-iv-link').addEventListener('click', function () {
    post('web_app_open_link', {
      url: 'https://telegra.ph/Rustgram-Instant-View-Sample-11-22',
      try_instant_view: true
    });
  });

  document.getElementById('btn-secondary-pos').addEventListener('click', function () {
    posIndex = (posIndex + 1) % positions.length;
    post('web_app_setup_secondary_button', {
      is_visible: true,
      is_active: true,
      text: 'Details (' + positions[posIndex] + ')',
      position: positions[posIndex]
    });
  });

  function bootstrap() {
    post('web_app_ready');
    post('web_app_request_theme');
    post('web_app_request_viewport');
    post('web_app_request_safe_area');
    post('web_app_request_content_safe_area');
    post('web_app_setup_back_button', { is_visible: true });
    post('web_app_setup_settings_button', { is_visible: true });
    post('web_app_setup_main_button', {
      is_visible: true,
      is_active: true,
      text: 'Checkout · 180 Stars',
      color: '#40a7e3',
      text_color: '#ffffff'
    });
    post('web_app_setup_secondary_button', {
      is_visible: true,
      is_active: true,
      text: 'Preview Cart',
      position: 'left'
    });
  }

  if (document.readyState === 'loading') {
    window.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
"##;
