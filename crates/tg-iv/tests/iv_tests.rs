//! Integration tests for `tg-iv`: golden HTML, XSS/escaping safety, link classification,
//! controller/JS bridge protocol, zoom, collage layout, and custom-scheme serving.

use std::collections::HashMap;

use tg_iv::grouped_layout::{MediaSize, layout_media_group};
use tg_iv::{
    BridgeMessage, ControllerAction, DateFlags, DocumentMeta, EmbedPolicy, GeoPoint, HostCommand,
    IvController, IvServer, LinkTarget, ListItemContent, MediaKind, MemoryMediaStore,
    NavigationDecision, Page, PageBlock, PageCaption, PageChannel, PageData, PageListItem,
    PageListOrderedItem, PageRelatedArticle, PageTableCell, PageTableRow, PhotoMeta, RenderOptions,
    RichText, ShortcutAction, TableAlign, TableValign, TaskState, ZOOM_MAX, ZOOM_MIN, ZoomState,
    ZoomStep, classify_link, default_iv_zoom, escape_html, format_utc_date, https_to_tonsite,
    is_allowed_embed_url, is_good_tonsite_url, normalize_fragment_id, ordered_alpha_text,
    ordered_roman_text, parse_range_header, prefer_iv_for_uri, prepare_page, resolve_iv_zoom,
    safe_url, site_name_from_url, tonsite_to_https,
};

fn build_all_blocks_page() -> Page {
    Page {
        id: 4242,
        hash: 7,
        url: "https://telegra.ph/All-Blocks-Test".to_string(),
        site_name: Some("Telegraph".to_string()),
        part: false,
        rtl: false,
        v2: true,
        views: Some(99),
        photos: vec![
            PhotoMeta {
                id: 1,
                width: 800,
                height: 600,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 2,
                width: 600,
                height: 800,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 3,
                width: 1200,
                height: 600,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 4,
                width: 64,
                height: 64,
                stripped_thumb: None,
            },
        ],
        documents: vec![
            DocumentMeta {
                id: 10,
                mime_type: "video/mp4".to_string(),
                file_name: "demo.mp4".to_string(),
                width: 640,
                height: 360,
                duration: 12,
                is_video: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 11,
                mime_type: "audio/mpeg".to_string(),
                file_name: "track.mp3".to_string(),
                duration: 180,
                title: "Track One".to_string(),
                performer: "Artist".to_string(),
                is_audio: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 12,
                mime_type: "image/png".to_string(),
                file_name: "icon.png".to_string(),
                width: 16,
                height: 16,
                ..Default::default()
            },
            DocumentMeta {
                id: 13,
                mime_type: "application/pdf".to_string(),
                file_name: "spec.pdf".to_string(),
                ..Default::default()
            },
        ],
        blocks: vec![
            PageBlock::Cover(Box::new(PageBlock::Photo {
                photo_id: 3,
                caption: PageCaption {
                    text: RichText::plain("Cover caption"),
                    credit: RichText::plain("Cover credit"),
                },
                url: None,
                webpage_id: None,
                spoiler: false,
            })),
            PageBlock::Kicker(RichText::plain("Kicker")),
            PageBlock::Title(RichText::plain("Golden Title")),
            PageBlock::Subtitle(RichText::plain("Golden Subtitle")),
            PageBlock::AuthorDate {
                author: RichText::plain("Alice"),
                published_date: 1_700_000_000,
            },
            PageBlock::Header(RichText::plain("Header")),
            PageBlock::Subheader(RichText::plain("Subheader")),
            PageBlock::Heading {
                level: 5,
                text: RichText::plain("Heading 5"),
            },
            PageBlock::Anchor {
                name: "Sec One".to_string(),
            },
            PageBlock::Paragraph(RichText::concat(vec![
                RichText::plain("Plain "),
                RichText::bold(RichText::plain("Bold")),
                RichText::plain(" "),
                RichText::italic(RichText::plain("Italic")),
                RichText::plain(" "),
                RichText::underline(RichText::plain("Under")),
                RichText::plain(" "),
                RichText::strike(RichText::plain("Strike")),
                RichText::plain(" "),
                RichText::fixed(RichText::plain("code")),
                RichText::plain(" "),
                RichText::subscript(RichText::plain("sub")),
                RichText::plain(" "),
                RichText::superscript(RichText::plain("sup")),
                RichText::plain(" "),
                RichText::marked(RichText::plain("mark")),
                RichText::plain(" "),
                RichText::Url {
                    text: Box::new(RichText::plain("Link")),
                    url: "https://example.org/a".to_string(),
                    webpage_id: 77,
                },
                RichText::plain(" "),
                RichText::Email {
                    text: Box::new(RichText::plain("mail")),
                    email: "a@example.org".to_string(),
                },
                RichText::plain(" "),
                RichText::Phone {
                    text: Box::new(RichText::plain("call")),
                    phone: "+12345".to_string(),
                },
                RichText::plain(" "),
                RichText::Image {
                    document_id: 12,
                    width: 16,
                    height: 16,
                },
                RichText::plain(" "),
                RichText::Anchor {
                    text: Box::new(RichText::plain("Anchored")),
                    name: "InlineAnchor".to_string(),
                },
                RichText::plain(" "),
                RichText::Spoiler(Box::new(RichText::plain("secret"))),
                RichText::plain(" "),
                RichText::Math {
                    source: "$x^2$".to_string(),
                },
                RichText::plain(" "),
                RichText::Date {
                    text: Box::new(RichText::plain("Nov 14, 2023")),
                    date: 1_700_000_000,
                    flags: DateFlags {
                        long_date: true,
                        ..Default::default()
                    },
                },
            ])),
            PageBlock::Preformatted {
                text: RichText::plain("let x = 1 < 2;\nprintln!(\"{x}\");"),
                language: "rust".to_string(),
            },
            PageBlock::Divider,
            PageBlock::List {
                items: vec![
                    PageListItem {
                        task_state: TaskState::Checked,
                        content: ListItemContent::Text(RichText::plain("Done item")),
                    },
                    PageListItem {
                        task_state: TaskState::None,
                        content: ListItemContent::Blocks(vec![PageBlock::Paragraph(
                            RichText::plain("Nested paragraph"),
                        )]),
                    },
                ],
            },
            PageBlock::OrderedList {
                reversed: true,
                start: Some(5),
                list_type: Some("a".to_string()),
                items: vec![
                    PageListOrderedItem {
                        task_state: TaskState::None,
                        num: Some("e".to_string()),
                        value: Some(5),
                        list_type: None,
                        content: ListItemContent::Text(RichText::plain("Fifth")),
                    },
                    PageListOrderedItem {
                        task_state: TaskState::Unchecked,
                        num: None,
                        value: None,
                        list_type: None,
                        content: ListItemContent::Blocks(vec![PageBlock::Paragraph(
                            RichText::plain("Nested ordered"),
                        )]),
                    },
                ],
            },
            PageBlock::Blockquote {
                text: RichText::plain("Quote text"),
                caption: RichText::plain("Quote author"),
                collapsed: true,
            },
            PageBlock::BlockquoteBlocks {
                blocks: vec![PageBlock::Paragraph(RichText::plain("Block quote child"))],
                caption: RichText::plain("Source"),
            },
            PageBlock::Pullquote {
                text: RichText::plain("Pull text"),
                caption: RichText::plain("Pull cite"),
            },
            PageBlock::Photo {
                photo_id: 1,
                caption: PageCaption {
                    text: RichText::plain("Photo cap"),
                    credit: RichText::Empty,
                },
                url: Some("https://example.org/photo".to_string()),
                webpage_id: Some(88),
                spoiler: true,
            },
            PageBlock::Video {
                video_id: 10,
                caption: PageCaption {
                    text: RichText::plain("Video cap"),
                    credit: RichText::Empty,
                },
                autoplay: true,
                loop_video: true,
                spoiler: false,
            },
            PageBlock::Embed {
                url: Some("https://www.youtube.com/embed/abc".to_string()),
                html: None,
                poster_photo_id: None,
                width: Some(560),
                height: Some(315),
                full_width: true,
                allow_scrolling: false,
                caption: PageCaption {
                    text: RichText::plain("YouTube embed"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::EmbedPost {
                url: "https://t.me/channel/10".to_string(),
                webpage_id: 99,
                author_photo_id: 4,
                author: "Channel Author".to_string(),
                date: 1_700_000_000,
                blocks: vec![PageBlock::Paragraph(RichText::plain("Post body"))],
                caption: PageCaption {
                    text: RichText::plain("Post cap"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Collage {
                items: vec![
                    PageBlock::Photo {
                        photo_id: 1,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                    PageBlock::Photo {
                        photo_id: 2,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                ],
                caption: PageCaption {
                    text: RichText::plain("Collage cap"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Slideshow {
                items: vec![
                    PageBlock::Photo {
                        photo_id: 1,
                        caption: PageCaption::default(),
                        url: None,
                        webpage_id: None,
                        spoiler: false,
                    },
                    PageBlock::Video {
                        video_id: 10,
                        caption: PageCaption::default(),
                        autoplay: false,
                        loop_video: false,
                        spoiler: false,
                    },
                ],
                caption: PageCaption {
                    text: RichText::plain("Slideshow cap"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Channel(PageChannel {
                id: 500,
                title: "News Channel".to_string(),
                username: Some("news_ch".to_string()),
                joined: false,
            }),
            PageBlock::Audio {
                audio_id: 11,
                caption: PageCaption {
                    text: RichText::plain("Audio cap"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Document {
                document_id: 13,
                caption: PageCaption {
                    text: RichText::plain("PDF doc"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Table {
                title: RichText::plain("Scores"),
                bordered: true,
                striped: true,
                compact: true,
                rows: vec![
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::plain("Name")),
                                header: true,
                                colspan: 1,
                                rowspan: 2,
                                align: TableAlign::Left,
                                valign: TableValign::Middle,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Points")),
                                header: true,
                                colspan: 2,
                                rowspan: 1,
                                align: TableAlign::Center,
                                valign: TableValign::Top,
                            },
                        ],
                    },
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::plain("A")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Right,
                                valign: TableValign::Bottom,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("B")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Center,
                                valign: TableValign::Middle,
                            },
                        ],
                    },
                ],
            },
            PageBlock::Details {
                title: RichText::plain("Expand me"),
                open: true,
                blocks: vec![PageBlock::Paragraph(RichText::plain("Inside details"))],
            },
            PageBlock::RelatedArticles {
                title: RichText::plain("Related"),
                articles: vec![PageRelatedArticle {
                    url: "https://telegra.ph/Rel-1".to_string(),
                    webpage_id: 321,
                    title: Some("Related One".to_string()),
                    description: Some("Summary".to_string()),
                    photo_id: Some(1),
                    author: Some("Bob".to_string()),
                    published_date: Some(1_700_000_000),
                }],
            },
            PageBlock::Map {
                geo: GeoPoint {
                    lat: 51.5074,
                    lon: -0.1278,
                    access_hash: 0,
                },
                zoom: 12,
                width: 400,
                height: 200,
                caption: PageCaption {
                    text: RichText::plain("London"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Math {
                source: "$$a^2 + b^2 = c^2$$".to_string(),
            },
            PageBlock::Thinking(RichText::plain("Internal thought")),
            PageBlock::Footer(RichText::plain("Footer text")),
            PageBlock::Unsupported,
        ],
    }
}

#[test]
fn golden_all_blocks_html() {
    let page = build_all_blocks_page();
    let options = RenderOptions {
        base_url: "tg-iv://localhost".to_string(),
        inline_assets: false,
        include_bridge: true,
        embed_policy: EmbedPolicy::default(),
        ..Default::default()
    };
    let prepared = prepare_page(&page, &options);
    let expected = include_str!("fixtures/all_blocks.golden.html").replace("\r\n", "\n");
    assert_eq!(prepared.html, expected);
    assert_eq!(prepared.title, "Golden Title");
    assert_eq!(prepared.site_name, "Telegraph");
    assert!(!prepared.rtl);
    assert!(prepared.has_slideshow);
    assert_eq!(prepared.anchors, vec!["sec one", "inlineanchor"]);
    assert_eq!(prepared.photo_ids, vec![3, 1, 4, 2]);
    assert_eq!(prepared.document_ids, vec![12, 10, 11, 13]);
}

#[test]
fn rtl_and_dark_mode_attributes() {
    let page = Page {
        rtl: true,
        url: "https://example.org/rtl".to_string(),
        blocks: vec![PageBlock::Paragraph(RichText::plain("مرحبا"))],
        ..Default::default()
    };
    let prepared = prepare_page(
        &page,
        &RenderOptions {
            dark_mode: Some(true),
            zoom_percent: 125,
            ..Default::default()
        },
    );
    assert!(
        prepared
            .html
            .contains("<html dir=\"rtl\" data-theme=\"dark\" style=\"zoom:125%\">")
    );
}

#[test]
fn xss_escaping_and_url_sanitization() {
    let malicious_page = Page {
        id: 1,
        url: "https://example.org/xss".to_string(),
        blocks: vec![
            PageBlock::Title(RichText::plain("<script>alert('title')</script>")),
            PageBlock::Paragraph(RichText::concat(vec![
                RichText::plain("\"><img src=x onerror=alert(1)>"),
                RichText::Url {
                    text: Box::new(RichText::plain("Click me")),
                    url: "javascript:alert(1)".to_string(),
                    webpage_id: 0,
                },
                RichText::Url {
                    text: Box::new(RichText::plain("Mixed")),
                    url: "  JaVaScRiPt:alert(2)".to_string(),
                    webpage_id: 0,
                },
                RichText::Url {
                    text: Box::new(RichText::plain("ControlChar")),
                    url: "java\tscript:alert(3)".to_string(),
                    webpage_id: 0,
                },
                RichText::Url {
                    text: Box::new(RichText::plain("DataUrl")),
                    url: "data:text/html,<script>alert(4)</script>".to_string(),
                    webpage_id: 0,
                },
                RichText::Url {
                    text: Box::new(RichText::plain("VbScript")),
                    url: "vbscript:msgbox(1)".to_string(),
                    webpage_id: 0,
                },
                RichText::Url {
                    text: Box::new(RichText::plain("FileUrl")),
                    url: "file:///C:/Windows/System32/calc.exe".to_string(),
                    webpage_id: 0,
                },
            ])),
            PageBlock::Preformatted {
                text: RichText::plain("<script>evil()</script>"),
                language: "rust\" onclick=\"alert(1)".to_string(),
            },
            PageBlock::Anchor {
                name: "x\" onmouseover=\"alert(1)".to_string(),
            },
            PageBlock::Embed {
                url: Some("https://evil.example.com/payload".to_string()),
                html: Some("<script>alert('raw-embed')</script>".to_string()),
                poster_photo_id: None,
                width: Some(400),
                height: Some(300),
                full_width: false,
                allow_scrolling: false,
                caption: PageCaption::default(),
            },
            PageBlock::Channel(PageChannel {
                id: 99,
                title: "<b>Bad Channel</b>".to_string(),
                username: Some("bad\" onclick=\"x".to_string()),
                joined: false,
            }),
        ],
        ..Default::default()
    };

    let prepared = prepare_page(&malicious_page, &RenderOptions::default());
    let html = &prepared.html;

    assert!(!html.contains("<script>alert"));
    assert!(!html.contains("<script>evil"));
    assert!(!html.contains("<img src=x"));
    assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
    assert!(!html.contains("href=\"javascript:"));
    assert!(!html.contains("href=\"JaVaScRiPt:"));
    assert!(!html.contains("href=\"data:"));
    assert!(!html.contains("href=\"vbscript:"));
    assert!(!html.contains("href=\"file:"));
    assert!(!html.contains("onclick=\"alert"));
    assert!(!html.contains("onmouseover=\"alert"));
    assert!(!html.contains("<iframe"));
    assert!(html.contains("&lt;script&gt;alert(&#39;title&#39;)&lt;/script&gt;"));
    assert!(html.contains("class=\"language-rustonclickalert1\""));

    // Verify safe_url unit cases directly.
    assert_eq!(
        safe_url("https://example.com/a"),
        Some("https://example.com/a".to_string())
    );
    assert_eq!(safe_url("#MySection"), Some("#mysection".to_string()));
    assert_eq!(
        safe_url("tg://resolve?domain=telegram"),
        Some("tg://resolve?domain=telegram".to_string())
    );
    assert_eq!(safe_url("javascript:alert(1)"), None);
    assert_eq!(safe_url("https://example.com/\nfoo"), None);
    assert_eq!(safe_url("data:image/png;base64,AAAA"), None);
    assert_eq!(escape_html("<&\"'>"), "&lt;&amp;&quot;&#39;&gt;");
}

#[test]
fn embed_policy_modes() {
    let page = Page {
        url: "https://example.org/embed".to_string(),
        blocks: vec![PageBlock::Embed {
            url: Some("https://www.youtube.com/embed/xyz".to_string()),
            html: Some("<div>custom</div>".to_string()),
            poster_photo_id: None,
            width: Some(500),
            height: Some(300),
            full_width: false,
            allow_scrolling: true,
            caption: PageCaption::default(),
        }],
        ..Default::default()
    };

    let blocked = prepare_page(
        &page,
        &RenderOptions {
            embed_policy: EmbedPolicy::Blocked,
            ..Default::default()
        },
    );
    assert!(!blocked.html.contains("<iframe"));
    assert!(blocked.html.contains("Embedded content blocked by policy"));

    let link_only = prepare_page(
        &page,
        &RenderOptions {
            embed_policy: EmbedPolicy::LinkOnly,
            ..Default::default()
        },
    );
    assert!(!link_only.html.contains("<iframe"));
    assert!(
        link_only
            .html
            .contains("<a href=\"https://www.youtube.com/embed/xyz\">")
    );

    assert!(is_allowed_embed_url(
        "https://www.youtube.com/embed/1",
        &["www.youtube.com".to_string()]
    ));
    assert!(!is_allowed_embed_url(
        "http://www.youtube.com/embed/1",
        &["www.youtube.com".to_string()]
    ));
    assert!(!is_allowed_embed_url(
        "https://www.youtube.com.evil.org/embed/1",
        &["www.youtube.com".to_string()]
    ));
}

#[test]
fn deep_nesting_does_not_stack_overflow() {
    let mut rt = RichText::plain("leaf");
    for _ in 0..200 {
        rt = RichText::bold(rt);
    }
    let mut blk = PageBlock::Paragraph(rt);
    for _ in 0..200 {
        blk = PageBlock::Details {
            title: RichText::plain("d"),
            blocks: vec![blk],
            open: false,
        };
    }
    let page = Page {
        blocks: vec![blk],
        ..Default::default()
    };
    let prepared = prepare_page(&page, &RenderOptions::default());
    assert!(prepared.html.contains("<details>"));
}

#[test]
fn link_classification_all_categories() {
    let current = Some("https://telegra.ph/My-Article-01-01");

    // In-page anchors
    assert_eq!(
        classify_link("#Section%20One", current, None),
        LinkTarget::Anchor {
            fragment: "section one".to_string()
        }
    );
    assert_eq!(
        classify_link("https://telegra.ph/My-Article-01-01#Notes", current, None),
        LinkTarget::Anchor {
            fragment: "notes".to_string()
        }
    );

    // Other IV pages (via preferred domain or explicit webpage_id)
    assert_eq!(
        classify_link("https://telegra.ph/Other-Page#Top", current, None),
        LinkTarget::InstantViewPage {
            url: "https://telegra.ph/Other-Page#Top".to_string(),
            webpage_id: None,
            fragment: Some("top".to_string()),
        }
    );
    assert_eq!(
        classify_link("https://example.com/news", current, Some(12345)),
        LinkTarget::InstantViewPage {
            url: "https://example.com/news".to_string(),
            webpage_id: Some(12345),
            fragment: None,
        }
    );
    assert!(prefer_iv_for_uri("https://telegra.ph/foo"));
    assert!(prefer_iv_for_uri("https://te.legra.ph/foo"));
    assert!(prefer_iv_for_uri("https://graph.org/foo"));
    assert!(prefer_iv_for_uri("https://telegram.org/blog/instant-view"));
    assert!(prefer_iv_for_uri("https://telegram.org/faq"));
    assert!(prefer_iv_for_uri("https://telegram.org/privacy"));
    assert!(!prefer_iv_for_uri("https://telegram.org/apps"));

    // Telegram links
    assert_eq!(
        classify_link("tg://resolve?domain=telegram", current, None),
        LinkTarget::TelegramLink {
            uri: "tg://resolve?domain=telegram".to_string()
        }
    );
    assert_eq!(
        classify_link("https://t.me/telegram/123", current, None),
        LinkTarget::TelegramLink {
            uri: "https://t.me/telegram/123".to_string()
        }
    );

    // TON sites (port of TonsiteToHttps / HttpsToTonsite)
    assert!(is_good_tonsite_url("tonsite://foundation.ton/about"));
    assert_eq!(
        tonsite_to_https("tonsite://my-site.ton/page"),
        Some("https://my-hsite-dton.magic.org/page".to_string())
    );
    assert_eq!(
        https_to_tonsite("https://my-hsite-dton.magic.org/page"),
        "tonsite://my-site.ton/page"
    );
    assert_eq!(
        classify_link("tonsite://foundation.ton/docs", current, None),
        LinkTarget::TonSite {
            uri: "tonsite://foundation.ton/docs".to_string(),
            https_gateway_url: "https://foundation-dton.magic.org/docs".to_string(),
        }
    );

    // Email & phone
    assert_eq!(
        classify_link("mailto:hello@example.org?subject=Hi", current, None),
        LinkTarget::Email {
            address: "hello@example.org".to_string()
        }
    );
    assert_eq!(
        classify_link("tel:+15551234", current, None),
        LinkTarget::Phone {
            number: "+15551234".to_string()
        }
    );

    // External URL
    assert_eq!(
        classify_link("https://rust-lang.org/", current, None),
        LinkTarget::ExternalUrl {
            url: "https://rust-lang.org/".to_string()
        }
    );

    // Rejected
    assert!(matches!(
        classify_link("javascript:alert(1)", current, None),
        LinkTarget::Rejected { .. }
    ));
    assert!(matches!(
        classify_link("file:///etc/passwd", current, None),
        LinkTarget::Rejected { .. }
    ));
    assert!(matches!(
        classify_link("", current, None),
        LinkTarget::Rejected { .. }
    ));
}

#[test]
fn controller_bridge_zoom_and_history() {
    let page1 = Page {
        id: 100,
        url: "https://telegra.ph/Page-1".to_string(),
        blocks: vec![
            PageBlock::Title(RichText::plain("Page One")),
            PageBlock::Anchor {
                name: "intro".to_string(),
            },
        ],
        ..Default::default()
    };
    let page2 = Page {
        id: 200,
        url: "https://telegra.ph/Page-2".to_string(),
        blocks: vec![PageBlock::Title(RichText::plain("Page Two"))],
        ..Default::default()
    };
    let prep1 = prepare_page(&page1, &RenderOptions::default());
    let prep2 = prepare_page(&page2, &RenderOptions::default());

    let mut ctrl = IvController::new("tg-iv://localhost", 100);
    let init_actions = ctrl.load_page(&page1.url, page1.id, &prep1, Some("#Intro"));
    assert_eq!(
        init_actions,
        vec![ControllerAction::Eval(HostCommand::ScrollToAnchor {
            fragment: "intro".to_string()
        })]
    );
    assert!(ctrl.has_anchor("INTRO"));

    // Scroll update
    let actions = ctrl
        .handle_bridge_json(
            r#"{"type":"scroll_position","y":420.5,"max_y":1000.0,"progress":0.42}"#,
        )
        .expect("valid scroll json");
    assert!(actions.is_empty());
    assert!((ctrl.scroll_position().y - 420.5).abs() < 0.01);

    // Link click to another IV page
    let actions = ctrl
        .handle_bridge_json(
            r#"{"type":"link_click","href":"https://telegra.ph/Page-2","webpage_id":200,"new_window":false}"#,
        )
        .expect("valid link click");
    assert_eq!(
        actions,
        vec![ControllerAction::OpenPage {
            url: "https://telegra.ph/Page-2".to_string(),
            webpage_id: Some(200),
            fragment: None,
        }]
    );

    // Media click
    let actions = ctrl
        .handle_bridge_json(r#"{"type":"media_click","kind":"photo","id":555}"#)
        .expect("valid media click");
    assert_eq!(
        actions,
        vec![ControllerAction::OpenMedia {
            kind: MediaKind::Photo,
            id: 555
        }]
    );

    // Context menu
    let actions = ctrl
        .handle_bridge_json(
            r#"{"type":"context_menu","x":12.0,"y":34.0,"selection":"hi","link_url":"https://example.org"}"#,
        )
        .expect("valid menu");
    assert_eq!(
        actions,
        vec![ControllerAction::ShowContextMenu {
            x: 12.0,
            y: 34.0,
            selection: "hi".to_string(),
            link_url: Some("https://example.org".to_string()),
            media: None,
        }]
    );

    // Legacy TelegramWebviewProxy array format
    let actions = ctrl
        .handle_bridge_json(r#"["resize_frame", {"height": "480px"}]"#)
        .expect("valid resize_frame");
    assert_eq!(actions, vec![ControllerAction::ResizeEmbed { height: 480 }]);

    // Keyboard shortcuts & zoom
    let actions = ctrl.handle_bridge_message(BridgeMessage::KeyShortcut {
        shortcut: ShortcutAction::ZoomIn,
    });
    assert_eq!(ctrl.zoom().percent(), 110);
    assert_eq!(actions.len(), 2);

    // History navigation preserves scroll position
    ctrl.load_page(&page2.url, page2.id, &prep2, None);
    assert!(ctrl.can_go_back());
    assert!(!ctrl.can_go_forward());
    let prev = ctrl.go_back().expect("can go back");
    assert_eq!(prev.url, "https://telegra.ph/Page-1");
    assert!((prev.scroll.y - 420.5).abs() < 0.01);
    assert!(ctrl.can_go_forward());
    let next = ctrl.go_forward().expect("can go forward");
    assert_eq!(next.url, "https://telegra.ph/Page-2");

    // Navigation policy
    assert_eq!(
        ctrl.navigation_policy("tg-iv://localhost/index.html", false),
        NavigationDecision::Allow
    );
    assert_eq!(
        ctrl.navigation_policy("about:blank", false),
        NavigationDecision::Allow
    );
    assert_eq!(
        ctrl.navigation_policy("https://foundation-dton.magic.org/", false),
        NavigationDecision::Allow
    );
    assert_eq!(
        ctrl.navigation_policy("https://external.example/x", false),
        NavigationDecision::Cancel(Some(LinkTarget::ExternalUrl {
            url: "https://external.example/x".to_string()
        }))
    );

    // HostCommand JS eval serialization
    let js = HostCommand::SetTheme { dark: true }
        .to_eval_js()
        .expect("serialize host command");
    assert!(js.contains("window.IvBridge.dispatch"));
    assert!(js.contains("\"set_theme\""));
}

#[test]
fn zoom_levels_and_snapping() {
    assert_eq!(default_iv_zoom(100, 100), 100);
    assert_eq!(default_iv_zoom(125, 100), 125);
    assert_eq!(default_iv_zoom(150, 100), 150);
    assert_eq!(resolve_iv_zoom(0, 125), 125);
    assert_eq!(resolve_iv_zoom(10, 100), ZOOM_MIN);
    assert_eq!(resolve_iv_zoom(900, 100), ZOOM_MAX);

    let mut zoom = ZoomState::new(100);
    assert_eq!(zoom.zoom_in(ZoomStep::Normal), 110);
    assert_eq!(zoom.zoom_in(ZoomStep::Small), 115);
    assert_eq!(zoom.zoom_in(ZoomStep::Tiny), 116);
    assert_eq!(zoom.zoom_out(ZoomStep::Tiny), 115);
    assert_eq!(zoom.reset(), 100);
    assert!((zoom.factor() - 1.0).abs() < 1e-9);
}

#[test]
fn serve_assets_media_range_and_traversal_defense() {
    let page = Page {
        url: "https://telegra.ph/Serve".to_string(),
        blocks: vec![
            PageBlock::Title(RichText::plain("Serve Test")),
            PageBlock::Embed {
                url: None,
                html: Some("<p>sandboxed</p>".to_string()),
                poster_photo_id: None,
                width: None,
                height: None,
                full_width: false,
                allow_scrolling: false,
                caption: PageCaption::default(),
            },
        ],
        ..Default::default()
    };
    let prepared = prepare_page(&page, &RenderOptions::default());
    let server = IvServer::new(prepared);
    let mut store = MemoryMediaStore::new();
    store.insert_photo(7, "image/jpeg", b"JPEGDATA".to_vec());
    store.insert_document(9, "video/mp4", b"0123456789".to_vec());

    // HTML index
    let html_resp = server.serve("tg-iv://localhost/", None, &store);
    assert_eq!(html_resp.status, 200);
    assert_eq!(html_resp.content_type, "text/html; charset=utf-8");
    assert!(
        html_resp
            .headers
            .iter()
            .any(|(k, _)| k == "Content-Security-Policy")
    );

    // CSS & JS assets
    let css_resp = server.serve("/css/page.css", None, &store);
    assert_eq!(css_resp.status, 200);
    assert_eq!(css_resp.content_type, "text/css; charset=utf-8");
    assert!(!css_resp.body.is_empty());

    let js_resp = server.serve("/js/page.js", None, &store);
    assert_eq!(js_resp.status, 200);
    assert_eq!(
        js_resp.content_type,
        "application/javascript; charset=utf-8"
    );
    assert!(!js_resp.body.is_empty());

    // Cached photo
    let photo_resp = server.serve("/media/photo/7", None, &store);
    assert_eq!(photo_resp.status, 200);
    assert_eq!(photo_resp.content_type, "image/jpeg");
    assert_eq!(photo_resp.body, b"JPEGDATA");

    // Missing photo returns placeholder SVG by default
    let missing_photo = server.serve("/media/photo/999", None, &store);
    assert_eq!(missing_photo.status, 200);
    assert_eq!(missing_photo.content_type, "image/svg+xml; charset=utf-8");

    // Video with Range header (206 Partial Content)
    let partial = server.serve("/media/video/9", Some("bytes=2-5"), &store);
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, b"2345");
    assert!(
        partial
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Range" && v == "bytes 2-5/10")
    );

    // Unsatisfiable range (416)
    let bad_range = server.serve("/media/video/9", Some("bytes=50-60"), &store);
    assert_eq!(bad_range.status, 416);

    // Sandboxed embed
    let embed_resp = server.serve("/embed/0", None, &store);
    assert_eq!(embed_resp.status, 200);
    assert_eq!(embed_resp.body, b"<p>sandboxed</p>");

    // Path traversal rejected (403)
    assert_eq!(server.serve("/../etc/passwd", None, &store).status, 403);
    assert_eq!(server.serve("/%2e%2e/secret", None, &store).status, 403);
    assert_eq!(server.serve("/css\\page.css", None, &store).status, 403);

    // Unknown route (404)
    assert_eq!(server.serve("/does-not-exist", None, &store).status, 404);

    // Direct range parser checks
    assert_eq!(
        parse_range_header("bytes=0-3", 10),
        tg_iv::RangeParse::Satisfied(0, 3)
    );
    assert_eq!(
        parse_range_header("bytes=6-", 10),
        tg_iv::RangeParse::Satisfied(6, 9)
    );
    assert_eq!(
        parse_range_header("bytes=-4", 10),
        tg_iv::RangeParse::Satisfied(6, 9)
    );
    assert_eq!(
        parse_range_header("bytes=10-12", 10),
        tg_iv::RangeParse::Unsatisfiable
    );
}

#[test]
fn model_helpers_and_collage_layout() {
    assert_eq!(
        site_name_from_url("https://www.example.org/post/1"),
        "Example.org"
    );
    assert_eq!(site_name_from_url("https://telegra.ph/abc"), "Telegra.ph");
    assert_eq!(normalize_fragment_id("  ##Hello%20World "), "hello world");
    assert_eq!(ordered_alpha_text(1, false), "a");
    assert_eq!(ordered_alpha_text(27, true), "AA");
    assert_eq!(ordered_roman_text(1999, true), "MCMXCIX");
    assert_eq!(ordered_roman_text(42, false), "xlii");
    assert_eq!(
        format_utc_date(1_700_000_000),
        Some("November 14, 2023".to_string())
    );
    assert_eq!(format_utc_date(0), None);

    let data = PageData::new(Page {
        id: 5,
        hash: 11,
        url: "https://sub.domain.net/a".to_string(),
        part: true,
        ..Default::default()
    });
    assert_eq!(data.id(), "https://sub.domain.net/a");
    assert_eq!(data.name(), "Domain.net");
    assert_eq!(data.page_id(), 5);
    assert_eq!(data.hash(), 11);
    assert!(data.partial());

    // Collage layout for 1..=6 items
    for count in 1..=6 {
        let sizes: Vec<MediaSize> = (0..count)
            .map(|i| {
                if i % 2 == 0 {
                    MediaSize::new(800, 600)
                } else {
                    MediaSize::new(600, 800)
                }
            })
            .collect();
        let layout = layout_media_group(&sizes, 430, 100, 4);
        assert_eq!(layout.len(), count);
        for item in layout {
            assert!(!item.geometry.is_empty());
        }
    }

    // Photo URL override works cleanly
    let mut photo_urls = HashMap::new();
    photo_urls.insert(1, "data:image/png;base64,AA==".to_string());
    let prepared = prepare_page(
        &Page {
            blocks: vec![PageBlock::Photo {
                photo_id: 1,
                caption: PageCaption::default(),
                url: None,
                webpage_id: None,
                spoiler: false,
            }],
            ..Default::default()
        },
        &RenderOptions {
            photo_urls,
            ..Default::default()
        },
    );
    assert!(prepared.html.contains("src=\"data:image/png;base64,AA==\""));
}
