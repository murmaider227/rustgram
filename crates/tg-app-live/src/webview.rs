//! Live implementation of [`WebViewBackend`], Instant View TL `Page` -> `tg_iv::Page`
//! conversion, Mini App launch RPCs, and `messages.prolongWebView` keep-alive loop on
//! [`LiveSession`].
//!
//! Port origin: `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`,
//! `Telegram/SourceFiles/iv/iv_data.cpp`, `Telegram/SourceFiles/iv/iv_rich_page.cpp`,
//! and `Telegram/SourceFiles/iv/iv_instance.cpp`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde_json::Value;
use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_bots::mini_app::{
    MINI_APP_PLATFORM, MiniAppLaunchParams, POPULAR_APP_BOTS_LIMIT, WEBVIEW_PROLONG_TIMEOUT_MS,
    WebViewSource,
};
use tg_bots::protocol::{
    BotAppCache, BotAppLookupResult, PopularAppBotsPage, RequestedWebViewButton,
};
use tg_bots::stand_in::UserId as BotUserId;
use tg_bots::tl_convert::{
    bot_app_from_tl, popular_app_bots_from_tl, requested_webview_button_from_tl,
    web_view_result_from_tl,
};
use tg_data::{DocumentData, Peer, PeerId as DataPeerId, PhotoData, Session, UserId as DataUserId};
use tg_iv::model::{DEFAULT_MAP_HEIGHT, DEFAULT_MAP_WIDTH, MAX_NESTING_DEPTH};
use tg_iv::{
    DateFlags, DocumentMeta, GeoPoint, ListItemContent, MediaBlob, MediaProvider, Page, PageBlock,
    PageCaption, PageChannel, PageListItem, PageListOrderedItem, PageRelatedArticle, PageTableCell,
    PageTableRow, PhotoMeta, RichText, TableAlign, TableValign, TaskState,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::runtime::Handle;
use tokio::sync::{RwLock, watch};
use tracing::debug;

use crate::bots::{attach_menu_flags_for_bot, mark_attach_menu_bot_removed};
use crate::convert::{now_unix_i32, peer_display_title, to_data_peer};
use crate::media::{
    block_on_runtime, document_to_location, download_file_bytes, photo_to_location,
    with_rpc_timeout,
};
use crate::session::LiveSession;
use tg_app_core::core_types::misc::account_bot_storage_dir;
use tg_app_core::core_types::misc::{build_webview_theme_params, tg_palette_for_mode};
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::webview::backend::{
    BotInfo, IvPageBundle, PreparedInlineMessageInfo, WebViewBackend, WebViewBackendError,
    WebViewLaunchInfo, WebViewRequest,
};

const WEBVIEW_RPC_TIMEOUT: Duration = Duration::from_secs(10);

// ============================================================================
// ProlongWebView keep-alive loop handle & per-session registry
// ============================================================================

/// Cancellation handle for a running `messages.prolongWebView` background loop.
///
/// Dropping the handle (or calling [`ProlongWebViewHandle::stop`]) cancels the keep-alive loop.
#[derive(Debug)]
pub struct ProlongWebViewHandle {
    query_id: u64,
    stop_tx: watch::Sender<bool>,
}

impl ProlongWebViewHandle {
    /// Returns the Mini App `query_id` being prolonged.
    #[must_use]
    pub const fn query_id(&self) -> u64 {
        self.query_id
    }

    /// Stops the background `messages.prolongWebView` loop immediately.
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for ProlongWebViewHandle {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
    }
}

struct LiveWebViewState {
    prolong_tasks: HashMap<u64, watch::Sender<bool>>,
    next_random_id: u64,
    iv_page_cache: HashMap<String, (std::time::Instant, Option<tl_types::WebPage>)>,
    bot_app_cache: BotAppCache,
    emoji_status_permissions: HashMap<u64, bool>,
    tdata_dir: Option<PathBuf>,
    account_key: Option<String>,
}

impl LiveWebViewState {
    fn new() -> Self {
        Self {
            prolong_tasks: HashMap::new(),
            next_random_id: 1,
            iv_page_cache: HashMap::new(),
            bot_app_cache: BotAppCache::new(),
            emoji_status_permissions: HashMap::new(),
            tdata_dir: None,
            account_key: None,
        }
    }

    fn alloc_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0xAEBF_1020_3040) as i64
    }
}

type WebViewStateRegistry = Mutex<Vec<(Weak<RwLock<Session>>, Arc<Mutex<LiveWebViewState>>)>>;

fn webview_state_registry() -> &'static WebViewStateRegistry {
    static REGISTRY: OnceLock<WebViewStateRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn webview_state_for_session(session: &Arc<RwLock<Session>>) -> Arc<Mutex<LiveWebViewState>> {
    let key = Arc::downgrade(session);
    let mut guard = webview_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, state)) = guard.iter().find(|(weak, _)| Weak::ptr_eq(weak, &key)) {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(LiveWebViewState::new()));
    guard.push((key, Arc::clone(&state)));
    state
}

// ============================================================================
// Instant View TL -> `tg_iv::Page` conversion & live media provider
// ============================================================================

/// Converts a TL `RichText` (layer 233) into `tg_iv::RichText`, respecting [`MAX_NESTING_DEPTH`].
#[must_use]
pub fn tl_rich_text_to_iv(text: &tl_enums::RichText) -> RichText {
    tl_rich_text_to_iv_depth(text, 0)
}

fn tl_rich_text_to_iv_depth(text: &tl_enums::RichText, depth: usize) -> RichText {
    if depth >= MAX_NESTING_DEPTH {
        return RichText::Empty;
    }
    let next = depth + 1;
    match text {
        tl_enums::RichText::TextEmpty => RichText::Empty,
        tl_enums::RichText::TextPlain(t) => RichText::Plain(t.text.clone()),
        tl_enums::RichText::TextBold(t) => {
            RichText::Bold(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextItalic(t) => {
            RichText::Italic(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextUnderline(t) => {
            RichText::Underline(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextStrike(t) => {
            RichText::Strike(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextFixed(t) => {
            RichText::Fixed(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextUrl(t) => RichText::Url {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            url: t.url.clone(),
            webpage_id: t.webpage_id,
        },
        tl_enums::RichText::TextEmail(t) => RichText::Email {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            email: t.email.clone(),
        },
        tl_enums::RichText::TextPhone(t) => RichText::Phone {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            phone: t.phone.clone(),
        },
        tl_enums::RichText::TextSubscript(t) => {
            RichText::Subscript(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextSuperscript(t) => {
            RichText::Superscript(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextMarked(t) => {
            RichText::Marked(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextImage(t) => RichText::Image {
            document_id: t.document_id,
            width: t.w,
            height: t.h,
        },
        tl_enums::RichText::TextAnchor(t) => RichText::Anchor {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            name: t.name.clone(),
        },
        tl_enums::RichText::TextConcat(t) => RichText::Concat(
            t.texts
                .iter()
                .map(|part| tl_rich_text_to_iv_depth(part, next))
                .collect(),
        ),
        tl_enums::RichText::TextSpoiler(t) => {
            RichText::Spoiler(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextMath(t) => RichText::Math {
            source: t.source.clone(),
        },
        tl_enums::RichText::TextCustomEmoji(t) => RichText::CustomEmoji {
            document_id: t.document_id,
            alt: t.alt.clone(),
        },
        tl_enums::RichText::TextMention(t) => {
            RichText::Mention(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextHashtag(t) => {
            RichText::Hashtag(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextBotCommand(t) => {
            RichText::BotCommand(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextCashtag(t) => {
            RichText::Cashtag(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextAutoUrl(t) => {
            RichText::AutoUrl(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextAutoEmail(t) => {
            RichText::AutoEmail(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextAutoPhone(t) => {
            RichText::AutoPhone(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextBankCard(t) => {
            RichText::BankCard(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextTonAddress(t) => {
            RichText::TonAddress(Box::new(tl_rich_text_to_iv_depth(&t.text, next)))
        }
        tl_enums::RichText::TextMentionName(t) => RichText::MentionName {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            user_id: t.user_id,
        },
        tl_enums::RichText::TextDate(t) => RichText::Date {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
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
        tl_enums::RichText::TextDiff(t) => RichText::Diff {
            text: Box::new(tl_rich_text_to_iv_depth(&t.text, next)),
            old_text: Box::new(tl_rich_text_to_iv_depth(&t.old_text, next)),
        },
        tl_enums::RichText::TextButton(t) => tl_rich_text_to_iv_depth(&t.text, next),
    }
}

fn tl_page_caption_to_iv(caption: &tl_enums::PageCaption, depth: usize) -> PageCaption {
    let tl_enums::PageCaption::PageCaption(c) = caption;
    PageCaption {
        text: tl_rich_text_to_iv_depth(&c.text, depth),
        credit: tl_rich_text_to_iv_depth(&c.credit, depth),
    }
}

fn tl_page_channel_to_iv(chat: &tl_enums::Chat) -> PageChannel {
    match chat {
        tl_enums::Chat::Channel(ch) => PageChannel {
            id: ch.id,
            title: ch.title.clone(),
            username: ch.username.clone(),
            joined: !ch.left,
        },
        tl_enums::Chat::ChannelForbidden(ch) => PageChannel {
            id: ch.id,
            title: ch.title.clone(),
            username: None,
            joined: false,
        },
        tl_enums::Chat::Chat(c) => PageChannel {
            id: c.id,
            title: c.title.clone(),
            username: None,
            joined: !c.left,
        },
        tl_enums::Chat::Forbidden(c) => PageChannel {
            id: c.id,
            title: c.title.clone(),
            username: None,
            joined: false,
        },
        tl_enums::Chat::Empty(e) => PageChannel {
            id: e.id,
            title: String::new(),
            username: None,
            joined: false,
        },
        tl_enums::Chat::Community(c) => PageChannel {
            id: c.id,
            title: c.title.clone(),
            username: None,
            joined: !c.left,
        },
        tl_enums::Chat::CommunityForbidden(c) => PageChannel {
            id: c.id,
            title: c.title.clone(),
            username: None,
            joined: false,
        },
    }
}

fn tl_page_block_to_iv(block: &tl_enums::PageBlock, depth: usize) -> PageBlock {
    if depth >= MAX_NESTING_DEPTH {
        return PageBlock::Unsupported;
    }
    let next = depth + 1;
    match block {
        tl_enums::PageBlock::Unsupported | tl_enums::PageBlock::ButtonRow(_) => {
            PageBlock::Unsupported
        }
        tl_enums::PageBlock::Title(b) => PageBlock::Title(tl_rich_text_to_iv_depth(&b.text, next)),
        tl_enums::PageBlock::Subtitle(b) => {
            PageBlock::Subtitle(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::AuthorDate(b) => PageBlock::AuthorDate {
            author: tl_rich_text_to_iv_depth(&b.author, next),
            published_date: b.published_date,
        },
        tl_enums::PageBlock::Header(b) => {
            PageBlock::Header(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Subheader(b) => {
            PageBlock::Subheader(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Heading1(b) => PageBlock::Heading {
            level: 1,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Heading2(b) => PageBlock::Heading {
            level: 2,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Heading3(b) => PageBlock::Heading {
            level: 3,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Heading4(b) => PageBlock::Heading {
            level: 4,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Heading5(b) => PageBlock::Heading {
            level: 5,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Heading6(b) => PageBlock::Heading {
            level: 6,
            text: tl_rich_text_to_iv_depth(&b.text, next),
        },
        tl_enums::PageBlock::Paragraph(b) => {
            PageBlock::Paragraph(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Preformatted(b) => PageBlock::Preformatted {
            text: tl_rich_text_to_iv_depth(&b.text, next),
            language: b.language.clone(),
        },
        tl_enums::PageBlock::Footer(b) => {
            PageBlock::Footer(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Divider => PageBlock::Divider,
        tl_enums::PageBlock::Anchor(b) => PageBlock::Anchor {
            name: b.name.clone(),
        },
        tl_enums::PageBlock::List(b) => PageBlock::List {
            items: b
                .items
                .iter()
                .map(|item| match item {
                    tl_enums::PageListItem::Text(t) => PageListItem {
                        task_state: TaskState::from_flags(t.checkbox, t.checked),
                        content: ListItemContent::Text(tl_rich_text_to_iv_depth(&t.text, next)),
                    },
                    tl_enums::PageListItem::Blocks(bl) => PageListItem {
                        task_state: TaskState::from_flags(bl.checkbox, bl.checked),
                        content: ListItemContent::Blocks(
                            bl.blocks
                                .iter()
                                .map(|sub| tl_page_block_to_iv(sub, next))
                                .collect(),
                        ),
                    },
                })
                .collect(),
        },
        tl_enums::PageBlock::OrderedList(b) => PageBlock::OrderedList {
            items: b
                .items
                .iter()
                .map(|item| match item {
                    tl_enums::PageListOrderedItem::Text(t) => PageListOrderedItem {
                        task_state: TaskState::from_flags(t.checkbox, t.checked),
                        num: t.num.clone(),
                        value: t.value,
                        list_type: t.r#type.clone(),
                        content: ListItemContent::Text(tl_rich_text_to_iv_depth(&t.text, next)),
                    },
                    tl_enums::PageListOrderedItem::Blocks(bl) => PageListOrderedItem {
                        task_state: TaskState::from_flags(bl.checkbox, bl.checked),
                        num: bl.num.clone(),
                        value: bl.value,
                        list_type: bl.r#type.clone(),
                        content: ListItemContent::Blocks(
                            bl.blocks
                                .iter()
                                .map(|sub| tl_page_block_to_iv(sub, next))
                                .collect(),
                        ),
                    },
                })
                .collect(),
            reversed: b.reversed,
            start: b.start,
            list_type: b.r#type.clone(),
        },
        tl_enums::PageBlock::Blockquote(b) => PageBlock::Blockquote {
            text: tl_rich_text_to_iv_depth(&b.text, next),
            caption: tl_rich_text_to_iv_depth(&b.caption, next),
            collapsed: b.collapsed,
        },
        tl_enums::PageBlock::BlockquoteBlocks(b) => PageBlock::BlockquoteBlocks {
            blocks: b
                .blocks
                .iter()
                .map(|sub| tl_page_block_to_iv(sub, next))
                .collect(),
            caption: tl_rich_text_to_iv_depth(&b.caption, next),
        },
        tl_enums::PageBlock::Pullquote(b) => PageBlock::Pullquote {
            text: tl_rich_text_to_iv_depth(&b.text, next),
            caption: tl_rich_text_to_iv_depth(&b.caption, next),
        },
        tl_enums::PageBlock::Photo(b) => PageBlock::Photo {
            photo_id: b.photo_id,
            caption: tl_page_caption_to_iv(&b.caption, next),
            url: b.url.clone(),
            webpage_id: b.webpage_id,
            spoiler: b.spoiler,
        },
        tl_enums::PageBlock::Video(b) => PageBlock::Video {
            video_id: b.video_id,
            caption: tl_page_caption_to_iv(&b.caption, next),
            autoplay: b.autoplay,
            loop_video: b.r#loop,
            spoiler: b.spoiler,
        },
        tl_enums::PageBlock::Cover(b) => {
            PageBlock::Cover(Box::new(tl_page_block_to_iv(&b.cover, next)))
        }
        tl_enums::PageBlock::Embed(b) => PageBlock::Embed {
            url: b.url.clone(),
            html: b.html.clone(),
            poster_photo_id: b.poster_photo_id,
            width: b.w,
            height: b.h,
            full_width: b.full_width,
            allow_scrolling: b.allow_scrolling,
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
        tl_enums::PageBlock::EmbedPost(b) => PageBlock::EmbedPost {
            url: b.url.clone(),
            webpage_id: b.webpage_id,
            author_photo_id: b.author_photo_id,
            author: b.author.clone(),
            date: b.date,
            blocks: b
                .blocks
                .iter()
                .map(|sub| tl_page_block_to_iv(sub, next))
                .collect(),
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
        tl_enums::PageBlock::Collage(b) => PageBlock::Collage {
            items: b
                .items
                .iter()
                .map(|sub| tl_page_block_to_iv(sub, next))
                .collect(),
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
        tl_enums::PageBlock::Slideshow(b) => PageBlock::Slideshow {
            items: b
                .items
                .iter()
                .map(|sub| tl_page_block_to_iv(sub, next))
                .collect(),
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
        tl_enums::PageBlock::Channel(b) => PageBlock::Channel(tl_page_channel_to_iv(&b.channel)),
        tl_enums::PageBlock::Audio(b) => PageBlock::Audio {
            audio_id: b.audio_id,
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
        tl_enums::PageBlock::Kicker(b) => {
            PageBlock::Kicker(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Table(b) => PageBlock::Table {
            title: tl_rich_text_to_iv_depth(&b.title, next),
            rows: b
                .rows
                .iter()
                .map(|row| {
                    let tl_enums::PageTableRow::PageTableRow(r) = row;
                    PageTableRow {
                        cells: r
                            .cells
                            .iter()
                            .map(|cell| {
                                let tl_enums::PageTableCell::PageTableCell(c) = cell;
                                let align = if c.align_right {
                                    TableAlign::Right
                                } else if c.align_center {
                                    TableAlign::Center
                                } else {
                                    TableAlign::Left
                                };
                                let valign = if c.valign_bottom {
                                    TableValign::Bottom
                                } else if c.valign_middle {
                                    TableValign::Middle
                                } else {
                                    TableValign::Top
                                };
                                PageTableCell {
                                    text: c
                                        .text
                                        .as_ref()
                                        .map(|t| tl_rich_text_to_iv_depth(t, next)),
                                    header: c.header,
                                    colspan: c.colspan.unwrap_or(1),
                                    rowspan: c.rowspan.unwrap_or(1),
                                    align,
                                    valign,
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
            bordered: b.bordered,
            striped: b.striped,
            compact: b.compact,
        },
        tl_enums::PageBlock::Details(b) => PageBlock::Details {
            title: tl_rich_text_to_iv_depth(&b.title, next),
            blocks: b
                .blocks
                .iter()
                .map(|sub| tl_page_block_to_iv(sub, next))
                .collect(),
            open: b.open,
        },
        tl_enums::PageBlock::RelatedArticles(b) => PageBlock::RelatedArticles {
            title: tl_rich_text_to_iv_depth(&b.title, next),
            articles: b
                .articles
                .iter()
                .map(|art| {
                    let tl_enums::PageRelatedArticle::PageRelatedArticle(a) = art;
                    PageRelatedArticle {
                        url: a.url.clone(),
                        webpage_id: a.webpage_id,
                        title: a.title.clone(),
                        description: a.description.clone(),
                        photo_id: a.photo_id,
                        author: a.author.clone(),
                        published_date: a.published_date,
                    }
                })
                .collect(),
        },
        tl_enums::PageBlock::Map(b) => {
            let geo = match &b.geo {
                tl_enums::GeoPoint::GeoPoint(g) => GeoPoint {
                    lat: g.lat,
                    lon: g.long,
                    access_hash: g.access_hash,
                },
                tl_enums::GeoPoint::Empty => GeoPoint::default(),
            };
            PageBlock::Map {
                geo,
                zoom: b.zoom,
                width: if b.w > 0 { b.w } else { DEFAULT_MAP_WIDTH },
                height: if b.h > 0 { b.h } else { DEFAULT_MAP_HEIGHT },
                caption: tl_page_caption_to_iv(&b.caption, next),
            }
        }
        tl_enums::PageBlock::InputPageBlockMap(b) => {
            let geo = match &b.geo {
                tl_enums::InputGeoPoint::InputGeoPoint(g) => GeoPoint {
                    lat: g.lat,
                    lon: g.long,
                    access_hash: 0,
                },
                tl_enums::InputGeoPoint::Empty => GeoPoint::default(),
            };
            PageBlock::Map {
                geo,
                zoom: b.zoom,
                width: if b.w > 0 { b.w } else { DEFAULT_MAP_WIDTH },
                height: if b.h > 0 { b.h } else { DEFAULT_MAP_HEIGHT },
                caption: tl_page_caption_to_iv(&b.caption, next),
            }
        }
        tl_enums::PageBlock::Math(b) => PageBlock::Math {
            source: b.source.clone(),
        },
        tl_enums::PageBlock::Thinking(b) => {
            PageBlock::Thinking(tl_rich_text_to_iv_depth(&b.text, next))
        }
        tl_enums::PageBlock::Document(b) => PageBlock::Document {
            document_id: b.document_id,
            caption: tl_page_caption_to_iv(&b.caption, next),
        },
    }
}

fn photo_meta_from_tl(photo: &tl_enums::Photo) -> Option<(PhotoMeta, PhotoData)> {
    let data = tg_api::convert::photo_from_tl(photo)?;
    let best = data
        .sizes
        .iter()
        .max_by_key(|s| i64::from(s.width) * i64::from(s.height));
    let meta = PhotoMeta {
        id: data.id.0 as i64,
        width: best.map_or(0, |s| s.width),
        height: best.map_or(0, |s| s.height),
        stripped_thumb: data.stripped_thumb.clone(),
    };
    Some((meta, data))
}

fn document_meta_from_tl(doc: &tl_enums::Document) -> Option<(DocumentMeta, DocumentData)> {
    let data = tg_api::convert::document_from_tl(doc)?;
    let is_animation = matches!(data.kind, tg_data::DocumentKind::Gif);
    let is_video = matches!(
        data.kind,
        tg_data::DocumentKind::Video | tg_data::DocumentKind::RoundVideo
    );
    let is_audio = matches!(
        data.kind,
        tg_data::DocumentKind::Audio | tg_data::DocumentKind::Voice
    );
    let meta = DocumentMeta {
        id: data.id.0 as i64,
        mime_type: data.mime_type.clone(),
        file_name: data.file_name.clone(),
        width: data.width.unwrap_or(0),
        height: data.height.unwrap_or(0),
        duration: data.duration.map_or(0, |d| d.round() as i32),
        title: data.audio_title.clone().unwrap_or_default(),
        performer: data.audio_performer.clone().unwrap_or_default(),
        is_video,
        is_animation,
        is_audio,
    };
    Some((meta, data))
}

/// Converts a TL `Page` (`page#98657f0d`) with webpage metadata into `tg_iv::Page`.
#[must_use]
pub fn tl_page_to_iv_page(
    webpage_id: u64,
    hash: i32,
    fallback_url: &str,
    site_name: Option<String>,
    tl_page: &tl_enums::Page,
) -> Page {
    let tl_enums::Page::Page(p) = tl_page;
    let url = if p.url.is_empty() {
        fallback_url.to_owned()
    } else {
        p.url.clone()
    };
    let blocks = p.blocks.iter().map(|b| tl_page_block_to_iv(b, 0)).collect();
    let photos = p
        .photos
        .iter()
        .filter_map(|ph| photo_meta_from_tl(ph).map(|(meta, _)| meta))
        .collect();
    let documents = p
        .documents
        .iter()
        .filter_map(|doc| document_meta_from_tl(doc).map(|(meta, _)| meta))
        .collect();
    Page {
        id: webpage_id,
        hash,
        url,
        site_name,
        part: p.part,
        rtl: p.rtl,
        v2: p.v2,
        views: p.views,
        blocks,
        photos,
        documents,
    }
}

/// Converts a TL `WebPage` (`webPage#e89c45b2`) containing a `cached_page` into `tg_iv::Page`.
///
/// Returns `None` if `webpage.cached_page` is absent.
#[must_use]
pub fn tl_webpage_to_iv_page(webpage: &tl_types::WebPage) -> Option<Page> {
    let tl_page = webpage.cached_page.as_ref()?;
    Some(tl_page_to_iv_page(
        webpage.id as u64,
        webpage.hash,
        &webpage.url,
        webpage.site_name.clone(),
        tl_page,
    ))
}

/// Live [`MediaProvider`] for Instant View pages, downloading referenced photos and documents
/// via `upload.getFile` with an in-memory cache.
struct LiveIvMediaProvider<C: RpcClient + Clone + 'static> {
    rpc: C,
    handle: Handle,
    photos: HashMap<i64, PhotoData>,
    documents: HashMap<i64, DocumentData>,
    cache: Mutex<HashMap<(u8, i64), MediaBlob>>,
}

impl<C: RpcClient + Clone + 'static> LiveIvMediaProvider<C> {
    fn from_webpage(rpc: C, handle: Handle, webpage: &tl_types::WebPage) -> Self {
        let mut photos = HashMap::new();
        let mut documents = HashMap::new();
        if let Some(ref ph) = webpage.photo
            && let Some((_, data)) = photo_meta_from_tl(ph)
        {
            photos.insert(data.id.0 as i64, data);
        }
        if let Some(ref doc) = webpage.document
            && let Some((_, data)) = document_meta_from_tl(doc)
        {
            documents.insert(data.id.0 as i64, data);
        }
        if let Some(tl_enums::Page::Page(p)) = &webpage.cached_page {
            for ph in &p.photos {
                if let Some((_, data)) = photo_meta_from_tl(ph) {
                    photos.insert(data.id.0 as i64, data);
                }
            }
            for doc in &p.documents {
                if let Some((_, data)) = document_meta_from_tl(doc) {
                    documents.insert(data.id.0 as i64, data);
                }
            }
        }
        Self {
            rpc,
            handle,
            photos,
            documents,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<C: RpcClient + Clone + 'static> MediaProvider for LiveIvMediaProvider<C> {
    fn photo(&self, id: i64) -> Option<MediaBlob> {
        if let Ok(guard) = self.cache.lock()
            && let Some(blob) = guard.get(&(1, id))
        {
            return Some(blob.clone());
        }
        let photo = self.photos.get(&id)?;
        let loc = photo_to_location(photo);
        let bytes = download_file_bytes(
            &self.rpc,
            &self.handle,
            loc.dc_id,
            loc.location,
            (loc.size > 0).then_some(loc.size),
        )
        .ok()
        .or_else(|| {
            photo
                .stripped_thumb
                .as_deref()
                .and_then(tg_media::image::inline::expand_inline_bytes)
        })?;
        let blob = MediaBlob {
            content_type: "image/jpeg".to_owned(),
            bytes,
        };
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert((1, id), blob.clone());
        }
        Some(blob)
    }

    fn document(&self, id: i64) -> Option<MediaBlob> {
        if let Ok(guard) = self.cache.lock()
            && let Some(blob) = guard.get(&(2, id))
        {
            return Some(blob.clone());
        }
        let doc = self.documents.get(&id)?;
        let loc = document_to_location(doc);
        let bytes = download_file_bytes(
            &self.rpc,
            &self.handle,
            loc.dc_id,
            loc.location,
            (loc.size > 0).then_some(loc.size),
        )
        .ok()?;
        let content_type = if doc.mime_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            doc.mime_type.clone()
        };
        let blob = MediaBlob {
            content_type,
            bytes,
        };
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert((2, id), blob.clone());
        }
        Some(blob)
    }
}

// ============================================================================
// LiveSession WebView & Mini App RPC helpers
// ============================================================================

impl LiveSession {
    fn wv_read_session<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    fn wv_write_session<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(mut guard) = session_lock.try_write() {
            return f(&mut guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = session_arc.write().await;
            f(&mut guard)
        })
    }

    fn wv_invoke_rpc<R>(&self, req: R) -> Result<R::Return, WebViewBackendError>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(WEBVIEW_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })
        .map_err(|msg| WebViewBackendError::new("RPC_ERROR", msg))
    }

    fn wv_input_user(&self, bot_id: u64) -> Result<tl_enums::InputUser, WebViewBackendError> {
        let u = self.wv_read_session(|s| input_user_for(s, DataUserId(bot_id)));
        if matches!(u, tl_enums::InputUser::Empty) {
            Err(WebViewBackendError::new(
                "BOT_INVALID",
                format!("Bot user {bot_id} is not loaded in session."),
            ))
        } else {
            Ok(u)
        }
    }

    fn wv_input_peer(
        &self,
        peer_id: DataPeerId,
    ) -> Result<tl_enums::InputPeer, WebViewBackendError> {
        let p = self.wv_read_session(|s| input_peer_for(s, peer_id));
        if matches!(p, tl_enums::InputPeer::Empty) {
            Err(WebViewBackendError::new(
                "PEER_ID_INVALID",
                format!("Peer {} is not loaded in session.", peer_id.0),
            ))
        } else {
            Ok(p)
        }
    }

    fn wv_input_channel(
        &self,
        channel_id: i64,
    ) -> Result<tl_enums::InputChannel, WebViewBackendError> {
        let pid = to_data_peer(PeerId::from_channel(channel_id.unsigned_abs()));
        self.wv_read_session(|s| {
            let peer = s.peer(pid)?;
            let ch = peer.as_channel()?;
            Some(tl_enums::InputChannel::InputChannel(
                tl_types::InputChannel {
                    channel_id: ch.channel_id().0 as i64,
                    access_hash: ch.access_hash as i64,
                },
            ))
        })
        .ok_or_else(|| {
            WebViewBackendError::new("CHANNEL_INVALID", "Channel is not loaded in session.")
        })
    }

    fn wv_apply_updates(&self, updates: &tl_enums::Updates) {
        let _ = self.apply_updates_to_session(updates);
    }

    /// Generates compact Telegram Mini App `theme_params` JSON from the active `tg-theme` palette.
    #[must_use]
    pub fn theme_params_json_from_palette(&self, mode: ThemeMode) -> String {
        let palette = tg_palette_for_mode(mode);
        build_webview_theme_params(&palette).json
    }

    fn resolve_theme_params_json(&self, provided: Option<&str>) -> tl_enums::DataJSON {
        let data = match provided {
            Some(raw) if !raw.trim().is_empty() && raw.trim() != "{}" => raw.to_owned(),
            _ => self.theme_params_json_from_palette(ThemeMode::Day),
        };
        tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data })
    }

    fn bot_info_for_webview(&self, bot_id: u64, is_game: bool) -> BotInfo {
        let (in_attach_menu, in_main_menu) =
            attach_menu_flags_for_bot(self.runtime().session(), bot_id);
        self.wv_read_session(|s| {
            let pid = DataPeerId::from_user(DataUserId(bot_id));
            let title = peer_display_title(s, pid);
            let (username, verified, attach_flag) = s
                .peer(pid)
                .and_then(Peer::as_user)
                .map(|u| {
                    (
                        u.usernames.username().to_owned(),
                        u.is_verified(),
                        u.bot_info.as_ref().is_some_and(|b| b.supports_attach_menu),
                    )
                })
                .unwrap_or_default();
            let effective_attach = in_attach_menu || attach_flag;
            BotInfo {
                id: bot_id,
                username,
                title,
                verified,
                in_attach_menu: effective_attach,
                in_main_menu,
                allow_clipboard_read: effective_attach || in_main_menu,
                is_game,
            }
        })
    }

    fn finish_webview_launch(
        &self,
        peer_id: DataPeerId,
        bot_id: u64,
        is_game: bool,
        tl_result: &tl_enums::WebViewResult,
    ) -> WebViewLaunchInfo {
        self.finish_webview_launch_with_prolong(
            peer_id,
            bot_id,
            is_game,
            tl_result,
            Some((false, None)),
        )
    }

    fn finish_webview_launch_with_prolong(
        &self,
        peer_id: DataPeerId,
        bot_id: u64,
        is_game: bool,
        tl_result: &tl_enums::WebViewResult,
        prolong_options: Option<(bool, Option<tl_enums::InputReplyTo>)>,
    ) -> WebViewLaunchInfo {
        let parsed = web_view_result_from_tl(tl_result);
        if parsed.query_id != 0
            && let Some((silent, reply_to)) = prolong_options
        {
            let handle = self.start_prolong_web_view_with_options(
                PeerId(peer_id.0),
                bot_id,
                parsed.query_id,
                Duration::from_millis(WEBVIEW_PROLONG_TIMEOUT_MS),
                silent,
                reply_to,
            );
            let state = webview_state_for_session(self.runtime().session());
            if let Ok(mut guard) = state.lock()
                && let Some(prev) = guard
                    .prolong_tasks
                    .insert(parsed.query_id, handle.stop_tx.clone())
            {
                let _ = prev.send(true);
            }
            // Keep the sender registered in `prolong_tasks` rather than cancelling on local drop.
            std::mem::forget(handle);
        }
        WebViewLaunchInfo {
            bot: self.bot_info_for_webview(bot_id, is_game),
            url: parsed.url,
            query_id: parsed.query_id,
            fullscreen: parsed.fullscreen,
            same_origin: parsed.same_origin,
            local_routes: None,
        }
    }

    /// Dispatches a Mini App launch across `messages.requestWebView`, `messages.requestSimpleWebView`,
    /// `messages.requestMainWebView`, `messages.requestAppWebView`, and
    /// `messages.requestChatJoinWebView` based on [`MiniAppLaunchParams`].
    pub fn request_web_view_full(
        &self,
        params: &MiniAppLaunchParams,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let bot_id = params.bot_id.0;
        let peer_id = params
            .peer_id
            .map(|p| DataPeerId(p.0))
            .unwrap_or_else(|| DataPeerId::from_user(DataUserId(bot_id)));
        let theme_json = if params.theme_params.json.trim().is_empty() {
            None
        } else {
            Some(params.theme_params.json.as_str())
        };
        let platform = if params.platform.is_empty() {
            MINI_APP_PLATFORM.to_owned()
        } else {
            params.platform.clone()
        };

        match &params.source {
            WebViewSource::JoinChat { query_id } => {
                // Port of `WebViewInstance::requestChatJoin` (`bot_attach_web_view.cpp:1369-1387`,
                // `1552-1563`): join-chat verification uses `messages.requestChatJoinWebView`
                // and never starts a `messages.prolongWebView` loop.
                self.request_chat_join_web_view_with_platform(
                    bot_id, *query_id, theme_json, platform,
                )
            }
            WebViewSource::Button { simple: true }
            | WebViewSource::SwitchWebView
            | WebViewSource::MainMenu => {
                // Port of `WebViewInstance::requestSimple` (`bot_attach_web_view.cpp:1280-1306`):
                // `SwitchWebView` sets `f_url | f_from_switch_webview` (no `start_param`),
                // `MainMenu` sets `f_from_side_menu` + optional non-empty `start_param` (no `url`),
                // and `SimpleWebView` button sets `f_url` (no `start_param`).
                let from_switch_webview = matches!(params.source, WebViewSource::SwitchWebView);
                let from_side_menu = matches!(params.source, WebViewSource::MainMenu);
                let url = if from_side_menu {
                    None
                } else {
                    params.url.clone().filter(|s| !s.is_empty())
                };
                let start_param = if from_side_menu {
                    params.start_param.clone().filter(|s| !s.is_empty())
                } else {
                    None
                };
                self.request_simple_web_view_with_platform(
                    bot_id,
                    url,
                    start_param,
                    from_switch_webview,
                    from_side_menu,
                    params.compact,
                    params.fullscreen,
                    theme_json,
                    platform,
                )
            }
            WebViewSource::LinkBotProfile {
                start_param,
                compact,
            } => {
                let sp = params
                    .start_param
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!start_param.is_empty()).then(|| start_param.clone()));
                self.request_main_web_view_with_platform(
                    PeerId(peer_id.0),
                    bot_id,
                    sp,
                    *compact || params.compact,
                    params.fullscreen,
                    theme_json,
                    platform,
                )
            }
            WebViewSource::BotProfile => self.request_main_web_view_with_platform(
                PeerId(peer_id.0),
                bot_id,
                params.start_param.clone().filter(|s| !s.is_empty()),
                params.compact,
                params.fullscreen,
                theme_json,
                platform,
            ),
            WebViewSource::LinkApp {
                app_name,
                start_param,
            } => {
                let app = if let Some((app_id, access_hash)) = params.app_id {
                    tl_enums::InputBotApp::ID(tl_types::InputBotAppID {
                        id: app_id.0 as i64,
                        access_hash: access_hash as i64,
                    })
                } else {
                    tl_enums::InputBotApp::ShortName(tl_types::InputBotAppShortName {
                        bot_id: self.wv_input_user(bot_id)?,
                        short_name: app_name.clone(),
                    })
                };
                let sp = params
                    .start_param
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!start_param.is_empty()).then(|| start_param.clone()));
                self.request_app_web_view_with_platform(
                    PeerId(peer_id.0),
                    bot_id,
                    app,
                    sp,
                    params.write_allowed,
                    params.compact,
                    params.fullscreen,
                    theme_json,
                    platform,
                )
            }
            _ => {
                let peer = self.wv_input_peer(peer_id)?;
                let bot = self.wv_input_user(bot_id)?;
                let reply_to = params.reply_to.map(|mid| {
                    tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
                        reply_to_msg_id: mid.0,
                        top_msg_id: None,
                        reply_to_peer_id: None,
                        quote_text: None,
                        quote_entities: None,
                        quote_offset: None,
                        monoforum_peer_id: None,
                        todo_item_id: None,
                        poll_option: None,
                        quote_rich: None,
                    })
                });
                // Port of `WebViewInstance::requestButton` (`bot_attach_web_view.cpp:1244-1267`):
                // omit `f_url` / `f_start_param` when strings are empty, and propagate `silent` +
                // `reply_to` to `messages.prolongWebView` (`bot_attach_web_view.cpp:1584-1595`).
                let resp = self.wv_invoke_rpc(tl_fns::messages::RequestWebView {
                    from_bot_menu: matches!(params.source, WebViewSource::BotMenu),
                    silent: params.silent,
                    compact: params.compact,
                    fullscreen: params.fullscreen,
                    peer,
                    bot,
                    url: params.url.clone().filter(|s| !s.is_empty()),
                    start_param: params.start_param.clone().filter(|s| !s.is_empty()),
                    theme_params: Some(self.resolve_theme_params_json(theme_json)),
                    platform,
                    reply_to: reply_to.clone(),
                    send_as: None,
                })?;
                Ok(self.finish_webview_launch_with_prolong(
                    peer_id,
                    bot_id,
                    matches!(params.source, WebViewSource::Game { .. }),
                    &resp,
                    Some((params.silent, reply_to)),
                ))
            }
        }
    }

    /// Invokes `messages.requestSimpleWebView`.
    #[allow(clippy::too_many_arguments)]
    pub fn request_simple_web_view(
        &self,
        bot_id: u64,
        url: Option<String>,
        start_param: Option<String>,
        from_switch_webview: bool,
        from_side_menu: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.request_simple_web_view_with_platform(
            bot_id,
            url,
            start_param,
            from_switch_webview,
            from_side_menu,
            false,
            fullscreen,
            theme_params_json,
            MINI_APP_PLATFORM.to_owned(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_simple_web_view_with_platform(
        &self,
        bot_id: u64,
        url: Option<String>,
        start_param: Option<String>,
        from_switch_webview: bool,
        from_side_menu: bool,
        compact: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
        platform: String,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let bot = self.wv_input_user(bot_id)?;
        let resp = self.wv_invoke_rpc(tl_fns::messages::RequestSimpleWebView {
            from_switch_webview,
            from_side_menu,
            compact,
            fullscreen,
            bot,
            url: url.filter(|s| !s.is_empty()),
            start_param: start_param.filter(|s| !s.is_empty()),
            theme_params: Some(self.resolve_theme_params_json(theme_params_json)),
            platform,
        })?;
        Ok(self.finish_webview_launch(
            DataPeerId::from_user(DataUserId(bot_id)),
            bot_id,
            false,
            &resp,
        ))
    }

    fn request_chat_join_web_view_with_platform(
        &self,
        bot_id: u64,
        query_id: u64,
        theme_params_json: Option<&str>,
        platform: String,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        // Port of `WebViewInstance::requestChatJoin` (`bot_attach_web_view.cpp:1369-1387`,
        // `1552-1563`): passes `f_theme_params`, `query_id`, `platform`, and skips `prolongWebView`.
        let resp = self.wv_invoke_rpc(tl_fns::messages::RequestChatJoinWebView {
            query_id: query_id as i64,
            theme_params: Some(self.resolve_theme_params_json(theme_params_json)),
            platform,
        })?;
        Ok(self.finish_webview_launch_with_prolong(
            DataPeerId::from_user(DataUserId(bot_id)),
            bot_id,
            false,
            &resp,
            None,
        ))
    }

    /// Invokes `messages.requestMainWebView`.
    #[allow(clippy::too_many_arguments)]
    pub fn request_main_web_view(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        start_param: Option<String>,
        compact: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.request_main_web_view_with_platform(
            peer_id,
            bot_id,
            start_param,
            compact,
            fullscreen,
            theme_params_json,
            MINI_APP_PLATFORM.to_owned(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_main_web_view_with_platform(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        start_param: Option<String>,
        compact: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
        platform: String,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let data_peer = to_data_peer(peer_id);
        let peer = self.wv_input_peer(data_peer)?;
        let bot = self.wv_input_user(bot_id)?;
        let resp = self.wv_invoke_rpc(tl_fns::messages::RequestMainWebView {
            compact,
            fullscreen,
            peer,
            bot,
            start_param: start_param.filter(|s| !s.is_empty()),
            theme_params: Some(self.resolve_theme_params_json(theme_params_json)),
            platform,
        })?;
        Ok(self.finish_webview_launch(data_peer, bot_id, false, &resp))
    }

    /// Invokes `messages.requestAppWebView`.
    #[allow(clippy::too_many_arguments)]
    pub fn request_app_web_view(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        app: tl_enums::InputBotApp,
        start_param: Option<String>,
        write_allowed: bool,
        compact: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.request_app_web_view_with_platform(
            peer_id,
            bot_id,
            app,
            start_param,
            write_allowed,
            compact,
            fullscreen,
            theme_params_json,
            MINI_APP_PLATFORM.to_owned(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_app_web_view_with_platform(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        app: tl_enums::InputBotApp,
        start_param: Option<String>,
        write_allowed: bool,
        compact: bool,
        fullscreen: bool,
        theme_params_json: Option<&str>,
        platform: String,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let data_peer = to_data_peer(peer_id);
        let peer = self.wv_input_peer(data_peer)?;
        let resp = self.wv_invoke_rpc(tl_fns::messages::RequestAppWebView {
            write_allowed,
            compact,
            fullscreen,
            peer,
            app,
            start_param: start_param.filter(|s| !s.is_empty()),
            theme_params: Some(self.resolve_theme_params_json(theme_params_json)),
            platform,
        })?;
        Ok(self.finish_webview_launch(data_peer, bot_id, false, &resp))
    }

    /// Spawns a periodic `messages.prolongWebView` keep-alive loop on the runtime's Tokio handle.
    ///
    /// Port of `WebViewInstance::show` `kProlongTimeout` timer (`bot_attach_web_view.cpp:1580-1599`).
    #[must_use]
    pub fn start_prolong_web_view(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        query_id: u64,
        interval: Duration,
    ) -> ProlongWebViewHandle {
        self.start_prolong_web_view_with_options(peer_id, bot_id, query_id, interval, false, None)
    }

    /// Spawns a periodic `messages.prolongWebView` keep-alive loop forwarding `silent` and
    /// `reply_to` flags (`bot_attach_web_view.cpp:1582-1599`).
    #[must_use]
    pub fn start_prolong_web_view_with_options(
        &self,
        peer_id: PeerId,
        bot_id: u64,
        query_id: u64,
        interval: Duration,
        silent: bool,
        reply_to: Option<tl_enums::InputReplyTo>,
    ) -> ProlongWebViewHandle {
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let rpc = self.runtime().rpc().clone();
        let data_peer = to_data_peer(peer_id);
        let (input_peer, input_bot) = self.wv_read_session(|s| {
            (
                input_peer_for(s, data_peer),
                input_user_for(s, DataUserId(bot_id)),
            )
        });
        let period = interval.max(Duration::from_millis(5));
        self.runtime().tokio_handle().spawn(async move {
            let mut ticker = tokio::time::interval(period);
            // Skip immediate tick so prolong fires after the first interval (matching tdesktop).
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => {
                        if *stop_rx.borrow() {
                            break;
                        }
                    }
                    _ = ticker.tick() => {
                        let req = tl_fns::messages::ProlongWebView {
                            silent,
                            peer: input_peer.clone(),
                            bot: input_bot.clone(),
                            query_id: query_id as i64,
                            reply_to: reply_to.clone(),
                            send_as: None,
                        };
                        match rpc.invoke(&req).await {
                            Ok(true) => {}
                            Ok(false) => {
                                debug!(query_id, "messages.prolongWebView returned false; stopping loop");
                                break;
                            }
                            Err(err) => {
                                let inv: tg_api::rpc::InvocationError = err;
                                debug!(query_id, error = %inv, "messages.prolongWebView failed; stopping loop");
                                break;
                            }
                        }
                    }
                }
            }
        });
        ProlongWebViewHandle { query_id, stop_tx }
    }

    /// Sends selected peers in reply to a Mini App `web_app_request_chat` button via
    /// `messages.sendBotRequestedPeer` with `f_webapp_req_id` (`bot_attach_web_view.cpp:2169-2186`).
    pub fn send_requested_web_view_peers(
        &self,
        bot_id: u64,
        webapp_req_id: &str,
        button_id: i32,
        selected_peers: &[PeerId],
    ) -> Result<(), WebViewBackendError> {
        if webapp_req_id.is_empty() {
            return Err(WebViewBackendError::new(
                "REQUEST_ID_INVALID",
                "WebView request id is empty.",
            ));
        }
        let bot_peer = self.wv_input_peer(DataPeerId::from_user(DataUserId(bot_id)))?;
        let mut requested_peers = Vec::with_capacity(selected_peers.len());
        for &pid in selected_peers {
            requested_peers.push(self.wv_input_peer(to_data_peer(pid))?);
        }
        let updates = self.wv_invoke_rpc(tl_fns::messages::SendBotRequestedPeer {
            peer: bot_peer,
            msg_id: None,
            webapp_req_id: Some(webapp_req_id.to_owned()),
            button_id,
            requested_peers,
        })?;
        self.wv_apply_updates(&updates);
        Ok(())
    }

    /// Stops an active `messages.prolongWebView` loop tracked for `query_id`.
    pub fn stop_prolong_web_view(&self, query_id: u64) {
        let state = webview_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock()
            && let Some(tx) = guard.prolong_tasks.remove(&query_id)
        {
            let _ = tx.send(true);
        }
    }

    /// Stops all active `messages.prolongWebView` loops associated with this session.
    pub fn stop_all_prolong_web_views(&self) {
        let state = webview_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            for (_, tx) in guard.prolong_tasks.drain() {
                let _ = tx.send(true);
            }
        }
    }

    /// Attaches a `tdata` directory for persisting per-bot Mini App [`tg_app_core::core_types::misc::BotLocalStorage`]
    /// scoped to this account (`account_key`, or `user_<self_user_id>` resolved when a Mini App opens).
    pub fn attach_webview_tdata_dir(&self, tdata_dir: PathBuf, account_key: Option<String>) {
        let state = webview_state_for_session(self.runtime().session());
        if let Ok(mut guard) = state.lock() {
            guard.tdata_dir = Some(tdata_dir);
            guard.account_key = account_key;
        }
    }

    /// Returns the per-account Mini App `BotLocalStorage` directory inside `tdata`, if attached.
    /// Returns `None` until the signed-in user is known, so no account writes into another's folder.
    #[must_use]
    pub fn webview_bot_storage_dir(&self) -> Option<PathBuf> {
        let (tdata, explicit_key) = {
            let state = webview_state_for_session(self.runtime().session());
            let guard = state.lock().ok()?;
            (guard.tdata_dir.clone()?, guard.account_key.clone())
        };
        let key = match explicit_key {
            Some(key) => key,
            None => {
                let uid = self.wv_read_session(|s| s.self_user_id().0);
                if uid == 0 {
                    return None;
                }
                format!("user_{uid}")
            }
        };
        let dir = account_bot_storage_dir(&tdata, &key);
        // The bot storage files are encrypted with the account storage key (SEC3-B2).
        self.media_cache().register_account_dir(&dir);
        Some(dir)
    }
}

fn extract_inline_message_preview(msg: &tl_enums::BotInlineMessage) -> String {
    match msg {
        tl_enums::BotInlineMessage::Text(m) => m.message.clone(),
        tl_enums::BotInlineMessage::MediaAuto(m) => m.message.clone(),
        tl_enums::BotInlineMessage::MediaGeo(_) => "Location".to_owned(),
        tl_enums::BotInlineMessage::MediaVenue(m) => {
            if m.address.is_empty() {
                m.title.clone()
            } else {
                format!("{} — {}", m.title, m.address)
            }
        }
        tl_enums::BotInlineMessage::MediaContact(m) => format!("{} {}", m.first_name, m.last_name)
            .trim()
            .to_owned(),
        tl_enums::BotInlineMessage::MediaInvoice(m) => {
            if m.description.is_empty() {
                m.title.clone()
            } else {
                format!("{}: {}", m.title, m.description)
            }
        }
        tl_enums::BotInlineMessage::MediaWebPage(m) => {
            if m.message.is_empty() {
                m.url.clone()
            } else {
                m.message.clone()
            }
        }
        tl_enums::BotInlineMessage::RichMessage(_) => "Rich message".to_owned(),
        tl_enums::BotInlineMessage::Widget(_) => "Interactive widget".to_owned(),
    }
}

fn map_tl_peer_types(types: &[tl_enums::InlineQueryPeerType]) -> Vec<tg_webview::InlineChatType> {
    let mut out = Vec::new();
    for ty in types {
        let mapped = match ty {
            tl_enums::InlineQueryPeerType::PM => tg_webview::InlineChatType::Users,
            // `PeerTypesFromMTP` maps `SameBotPM` to no type for prepared messages.
            tl_enums::InlineQueryPeerType::SameBotPM => continue,
            tl_enums::InlineQueryPeerType::BotPM => tg_webview::InlineChatType::Bots,
            tl_enums::InlineQueryPeerType::Chat | tl_enums::InlineQueryPeerType::Megagroup => {
                tg_webview::InlineChatType::Groups
            }
            tl_enums::InlineQueryPeerType::Broadcast => tg_webview::InlineChatType::Channels,
        };
        if !out.contains(&mapped) {
            out.push(mapped);
        }
    }
    out
}

fn map_prepared_rpc_error(err: WebViewBackendError) -> WebViewBackendError {
    let upper = err.message.to_ascii_uppercase();
    if upper.contains("MESSAGE_ID_INVALID")
        || upper.contains("ID_EXPIRED")
        || upper.contains("MESSAGE_EXPIRED")
    {
        WebViewBackendError::new("MESSAGE_EXPIRED", err.message)
    } else if upper.contains("BOT_INVALID") || upper.contains("USER_ID_INVALID") {
        WebViewBackendError::new("BOT_INVALID", err.message)
    } else {
        err
    }
}

impl WebViewBackend for LiveSession {
    fn fetch_iv_page(
        &self,
        url: &str,
        _webpage_id: Option<i64>,
    ) -> Result<Option<IvPageBundle>, WebViewBackendError> {
        const IV_CACHE_TTL: Duration = Duration::from_secs(10);
        let state = webview_state_for_session(self.runtime().session());
        if let Ok(guard) = state.lock()
            && let Some((fetched_at, cached_wp)) = guard.iv_page_cache.get(url)
            && fetched_at.elapsed() <= IV_CACHE_TTL
        {
            return Ok(cached_wp.as_ref().and_then(|wp| {
                let page = tl_webpage_to_iv_page(wp)?;
                let provider = LiveIvMediaProvider::from_webpage(
                    self.runtime().rpc().clone(),
                    self.runtime().tokio_handle().clone(),
                    wp,
                );
                Some(IvPageBundle {
                    page,
                    media: Arc::new(provider),
                })
            }));
        }

        let tl_enums::messages::WebPage::WebPage(resp) =
            self.wv_invoke_rpc(tl_fns::messages::GetWebPage {
                url: url.to_owned(),
                hash: 0,
            })?;
        self.wv_write_session(|session| {
            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
        });
        let tl_enums::WebPage::WebPage(wp) = resp.webpage else {
            if let Ok(mut guard) = state.lock() {
                guard
                    .iv_page_cache
                    .insert(url.to_owned(), (std::time::Instant::now(), None));
            }
            return Ok(None);
        };
        let Some(page) = tl_webpage_to_iv_page(&wp) else {
            if let Ok(mut guard) = state.lock() {
                guard
                    .iv_page_cache
                    .insert(url.to_owned(), (std::time::Instant::now(), None));
            }
            return Ok(None);
        };
        if let Ok(mut guard) = state.lock() {
            guard.iv_page_cache.insert(
                url.to_owned(),
                (std::time::Instant::now(), Some(wp.clone())),
            );
        }
        let provider = LiveIvMediaProvider::from_webpage(
            self.runtime().rpc().clone(),
            self.runtime().tokio_handle().clone(),
            &wp,
        );
        Ok(Some(IvPageBundle {
            page,
            media: Arc::new(provider),
        }))
    }

    fn request_web_view(
        &mut self,
        request: WebViewRequest,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let bot_id = request.bot_id;
        let peer_id = DataPeerId::from_user(DataUserId(bot_id));
        let peer = self.wv_input_peer(peer_id)?;
        let bot = self.wv_input_user(bot_id)?;
        let theme_params = Some(self.resolve_theme_params_json(Some(&request.theme_params_json)));
        let has_main_app = self.wv_read_session(|s| {
            s.user(DataUserId(bot_id))
                .and_then(|u| u.bot_info.as_ref())
                .is_some_and(|b| b.has_main_app)
        });

        let url_override = request.url_override.filter(|s| !s.is_empty());
        let start_param = request.start_param.filter(|s| !s.is_empty());
        let resp = if url_override.is_none() && has_main_app {
            self.wv_invoke_rpc(tl_fns::messages::RequestMainWebView {
                compact: false,
                fullscreen: request.fullscreen,
                peer,
                bot,
                start_param,
                theme_params,
                platform: MINI_APP_PLATFORM.to_owned(),
            })?
        } else {
            self.wv_invoke_rpc(tl_fns::messages::RequestWebView {
                from_bot_menu: false,
                silent: false,
                compact: false,
                fullscreen: request.fullscreen,
                peer,
                bot,
                url: url_override,
                start_param,
                theme_params,
                platform: MINI_APP_PLATFORM.to_owned(),
                reply_to: None,
                send_as: None,
            })?
        };
        Ok(self.finish_webview_launch(peer_id, bot_id, false, &resp))
    }

    fn send_web_app_data(
        &mut self,
        bot_id: u64,
        button_text: &str,
        data: &str,
    ) -> Result<(), WebViewBackendError> {
        let bot = self.wv_input_user(bot_id)?;
        let random_id = webview_state_for_session(self.runtime().session())
            .lock()
            .map(|mut s| s.alloc_random_id())
            .unwrap_or_else(|_| now_unix_i32() as i64);
        let updates = self.wv_invoke_rpc(tl_fns::messages::SendWebViewData {
            bot,
            random_id,
            button_text: button_text.to_owned(),
            data: data.to_owned(),
        })?;
        self.wv_apply_updates(&updates);
        Ok(())
    }

    fn invoke_custom_method(
        &mut self,
        bot_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let bot = self.wv_input_user(bot_id).map_err(|e| e.to_string())?;
        let tl_enums::DataJSON::DataJSON(res) = self
            .wv_invoke_rpc(tl_fns::bots::InvokeWebViewCustomMethod {
                bot,
                custom_method: method.to_owned(),
                params: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: params.to_string(),
                }),
            })
            .map_err(|e| e.message)?;
        serde_json::from_str(&res.data).map_err(|e| format!("Invalid custom method JSON: {e}"))
    }

    fn join_channel(&mut self, channel_id: i64) -> Result<(), WebViewBackendError> {
        let channel = self.wv_input_channel(channel_id)?;
        let res = self.wv_invoke_rpc(tl_fns::channels::JoinChannel { channel })?;
        match res {
            tl_enums::messages::ChatInviteJoinResult::Ok(ok) => {
                self.wv_apply_updates(&ok.updates);
            }
            tl_enums::messages::ChatInviteJoinResult::WebView(wv) => {
                self.wv_write_session(|session| {
                    apply_tl_users_and_chats(session, &wv.users, &[]);
                });
                self.bridge_state().notify_ui();
            }
        }
        Ok(())
    }

    fn remove_bot_from_menu(&mut self, bot_id: u64) -> Result<(), WebViewBackendError> {
        let bot = self.wv_input_user(bot_id)?;
        let _ = self.wv_invoke_rpc(tl_fns::messages::ToggleBotInAttachMenu {
            write_allowed: false,
            bot,
            enabled: false,
        })?;
        mark_attach_menu_bot_removed(self.runtime().session(), bot_id);
        self.bridge_state().notify_ui();
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
                "Prepared inline message ID is empty.",
            ));
        }
        let bot = self.wv_input_user(bot_id)?;
        let tl_enums::messages::PreparedInlineMessage::PreparedInlineMessage(prepared) = self
            .wv_invoke_rpc(tl_fns::messages::GetPreparedInlineMessage {
                bot,
                id: prepared_id.to_owned(),
            })
            .map_err(map_prepared_rpc_error)?;

        self.wv_write_session(|session| {
            apply_tl_users_and_chats(session, &prepared.users, &[]);
        });

        let (result_id, preview_title, preview_text) = match &prepared.result {
            tl_enums::BotInlineResult::BotInlineResult(res) => {
                let title = res.title.clone().unwrap_or_default();
                let msg_text = extract_inline_message_preview(&res.send_message);
                let preview = if !msg_text.is_empty() {
                    msg_text
                } else {
                    res.description.clone().unwrap_or_default()
                };
                (res.id.clone(), title, preview)
            }
            tl_enums::BotInlineResult::BotInlineMediaResult(res) => {
                let title = res.title.clone().unwrap_or_default();
                let msg_text = extract_inline_message_preview(&res.send_message);
                let preview = if !msg_text.is_empty() {
                    msg_text
                } else if let Some(desc) = &res.description
                    && !desc.is_empty()
                {
                    desc.clone()
                } else {
                    format!("[{}]", res.r#type)
                };
                (res.id.clone(), title, preview)
            }
        };

        Ok(PreparedInlineMessageInfo {
            prepared_id: prepared_id.to_owned(),
            query_id: prepared.query_id,
            result_id,
            preview_title,
            preview_text,
            allowed_peer_types: map_tl_peer_types(&prepared.peer_types),
        })
    }

    fn send_prepared_inline_message(
        &mut self,
        _bot_id: u64,
        peer_id: PeerId,
        query_id: i64,
        result_id: &str,
    ) -> Result<(), WebViewBackendError> {
        if peer_id.is_zero() {
            return Err(WebViewBackendError::new(
                "PEER_ID_INVALID",
                "Choose a chat to send the prepared message to.",
            ));
        }
        let peer = self.wv_input_peer(to_data_peer(peer_id))?;
        let random_id = webview_state_for_session(self.runtime().session())
            .lock()
            .map(|mut s| s.alloc_random_id())
            .unwrap_or_else(|_| now_unix_i32() as i64);
        let updates = self.wv_invoke_rpc(tl_fns::messages::SendInlineBotResult {
            silent: false,
            background: false,
            clear_draft: false,
            hide_via: false,
            peer,
            reply_to: None,
            random_id,
            query_id,
            id: result_id.to_owned(),
            schedule_date: None,
            send_as: None,
            quick_reply_shortcut: None,
            allow_paid_stars: None,
        })?;
        self.wv_apply_updates(&updates);
        Ok(())
    }

    fn prepared_message_recipients(
        &self,
        bot_id: u64,
        allowed: &[tg_webview::InlineChatType],
    ) -> Vec<(PeerId, String)> {
        let bot_peer = DataPeerId::from_user(DataUserId(bot_id));
        let now = now_unix_i32();
        // Only chats already in the loaded main list: the chooser never blocks the UI on RPC.
        self.wv_read_session(move |session| {
            session
                .main_dialogs()
                .iter()
                .filter_map(|key| match key {
                    tg_data::DialogKey::History(pid) => {
                        crate::convert::dialog_entry_from_session(session, *pid, now)
                    }
                    _ => None,
                })
                .filter(|entry| entry.peer.can_send)
                .filter_map(|entry| {
                    let data_peer = to_data_peer(entry.peer.id);
                    let peer = session.peer(data_peer)?;
                    // `PeerMatchesTypes` (bot_attach_web_view.cpp): the bot's own chat is
                    // `SameBot`, which prepared messages never allow.
                    let ty = match peer {
                        Peer::User(_) if data_peer == bot_peer => return None,
                        Peer::User(_) if peer.is_bot() => tg_webview::InlineChatType::Bots,
                        Peer::User(_) => tg_webview::InlineChatType::Users,
                        Peer::Channel(_) if peer.is_broadcast() => {
                            tg_webview::InlineChatType::Channels
                        }
                        Peer::Chat(_) | Peer::Channel(_) => tg_webview::InlineChatType::Groups,
                    };
                    (allowed.is_empty() || allowed.contains(&ty))
                        .then_some((entry.peer.id, entry.peer.title))
                })
                .collect()
        })
    }

    fn bot_storage_dir(&self) -> Option<PathBuf> {
        self.webview_bot_storage_dir()
    }

    fn get_bot_app(
        &mut self,
        bot_id: u64,
        short_name: &str,
        cached_hash: i64,
    ) -> Result<BotAppLookupResult, WebViewBackendError> {
        // Port of `WebViewInstance::resolveApp` (`bot_attach_web_view.cpp:1096-1146`).
        let bot = self.wv_input_user(bot_id)?;
        let state = webview_state_for_session(self.runtime().session());
        let effective_hash = if cached_hash != 0 {
            cached_hash
        } else {
            state
                .lock()
                .ok()
                .map(|s| s.bot_app_cache.hash_for(BotUserId(bot_id), short_name))
                .unwrap_or(0)
        };
        let resp = self.wv_invoke_rpc(tl_fns::messages::GetBotApp {
            app: tl_enums::InputBotApp::ShortName(tl_types::InputBotAppShortName {
                bot_id: bot,
                short_name: short_name.to_owned(),
            }),
            hash: effective_hash,
        })?;
        let parsed = bot_app_from_tl(BotUserId(bot_id), &resp);
        let resolved = if let Ok(mut guard) = state.lock() {
            guard
                .bot_app_cache
                .apply_lookup(BotUserId(bot_id), short_name, parsed)
        } else {
            parsed
        };
        Ok(resolved)
    }

    fn request_chat_join_web_view(
        &mut self,
        bot_id: u64,
        query_id: u64,
        theme_params_json: Option<&str>,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.request_chat_join_web_view_with_platform(
            bot_id,
            query_id,
            theme_params_json,
            MINI_APP_PLATFORM.to_owned(),
        )
    }

    fn can_send_message(&mut self, bot_id: u64) -> Result<bool, WebViewBackendError> {
        // Port of `WebViewInstance::botCheckWriteAccess` (`bot_attach_web_view.cpp:1891-1899`).
        let bot = self.wv_input_user(bot_id)?;
        self.wv_invoke_rpc(tl_fns::bots::CanSendMessage { bot })
    }

    fn allow_send_message(&mut self, bot_id: u64) -> Result<bool, WebViewBackendError> {
        // Port of `WebViewInstance::botAllowWriteAccess` (`bot_attach_web_view.cpp:1901-1910`).
        let bot = self.wv_input_user(bot_id)?;
        let updates = self.wv_invoke_rpc(tl_fns::bots::AllowSendMessage { bot })?;
        self.wv_apply_updates(&updates);
        Ok(true)
    }

    fn check_download_file_params(
        &mut self,
        bot_id: u64,
        file_name: &str,
        url: &str,
    ) -> Result<bool, WebViewBackendError> {
        // Port of `WebViewInstance::botDownloadFile` (`bot_attach_web_view.cpp:2297-2311`).
        if file_name.is_empty() || url.is_empty() {
            return Err(WebViewBackendError::new(
                "DOWNLOAD_PARAMS_INVALID",
                "File name and URL must both be non-empty.",
            ));
        }
        let bot = self.wv_input_user(bot_id)?;
        self.wv_invoke_rpc(tl_fns::bots::CheckDownloadFileParams {
            bot,
            file_name: file_name.to_owned(),
            url: url.to_owned(),
        })
    }

    fn get_popular_app_bots(
        &mut self,
        offset: &str,
        limit: i32,
    ) -> Result<PopularAppBotsPage, WebViewBackendError> {
        // Port of `AttachWebView::loadPopularAppBots` (`bot_attach_web_view.cpp:2660-2683`).
        let effective_limit = if limit > 0 {
            limit
        } else {
            POPULAR_APP_BOTS_LIMIT
        };
        let resp = self.wv_invoke_rpc(tl_fns::bots::GetPopularAppBots {
            offset: offset.to_owned(),
            limit: effective_limit,
        })?;
        let tl_enums::bots::PopularAppBots::PopularAppBots(ref page) = resp;
        if !page.users.is_empty() {
            self.wv_write_session(|session| {
                apply_tl_users_and_chats(session, &page.users, &[]);
            });
            self.bridge_state().notify_ui();
        }
        Ok(popular_app_bots_from_tl(&resp))
    }

    fn get_requested_web_view_button(
        &mut self,
        bot_id: u64,
        webapp_req_id: &str,
    ) -> Result<RequestedWebViewButton, WebViewBackendError> {
        // Port of `WebViewInstance::botRequestChat` (`bot_attach_web_view.cpp:2144-2228`).
        if webapp_req_id.is_empty() {
            return Err(WebViewBackendError::new(
                "REQUEST_ID_INVALID",
                "WebView button request id is empty.",
            ));
        }
        let bot = self.wv_input_user(bot_id)?;
        let resp = self.wv_invoke_rpc(tl_fns::bots::GetRequestedWebViewButton {
            bot,
            webapp_req_id: webapp_req_id.to_owned(),
        })?;
        Ok(requested_webview_button_from_tl(&resp))
    }

    fn toggle_user_emoji_status_permission(
        &mut self,
        bot_id: u64,
        enabled: bool,
    ) -> Result<bool, WebViewBackendError> {
        // Port of `WebViewInstance::botRequestEmojiStatusAccess` (`bot_attach_web_view.cpp:1926-1951`).
        let bot = self.wv_input_user(bot_id)?;
        let ok =
            self.wv_invoke_rpc(tl_fns::bots::ToggleUserEmojiStatusPermission { bot, enabled })?;
        if ok {
            let state = webview_state_for_session(self.runtime().session());
            if let Ok(mut guard) = state.lock() {
                guard.emoji_status_permissions.insert(bot_id, enabled);
            }
        }
        Ok(ok)
    }
}
