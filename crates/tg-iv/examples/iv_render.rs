//! Renders a comprehensive sample Instant View page to a standalone HTML file.
//!
//! Usage:
//! ```text
//! cargo run -p tg-iv --example iv_render -- out.html
//! ```

use std::env;
use std::fs;
use std::process;

use tg_iv::{
    DateFlags, DocumentMeta, EmbedPolicy, GeoPoint, ListItemContent, Page, PageBlock, PageCaption,
    PageChannel, PageListItem, PageListOrderedItem, PageRelatedArticle, PageTableCell,
    PageTableRow, PhotoMeta, RenderOptions, RichText, TableAlign, TableValign, TaskState,
    prepare_page,
};

fn build_sample_page() -> Page {
    Page {
        id: 9_876_543_210,
        hash: 42,
        url: "https://telegra.ph/Rustgram-Instant-View-Sample-11-22".to_string(),
        site_name: Some("Telegraph".to_string()),
        part: false,
        rtl: false,
        v2: true,
        views: Some(12_345),
        photos: vec![
            PhotoMeta {
                id: 101,
                width: 1200,
                height: 630,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 102,
                width: 800,
                height: 600,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 103,
                width: 600,
                height: 800,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 104,
                width: 900,
                height: 600,
                stripped_thumb: None,
            },
            PhotoMeta {
                id: 105,
                width: 128,
                height: 128,
                stripped_thumb: None,
            },
        ],
        documents: vec![
            DocumentMeta {
                id: 201,
                mime_type: "video/mp4".to_string(),
                file_name: "clip.mp4".to_string(),
                width: 1280,
                height: 720,
                duration: 18,
                is_video: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 202,
                mime_type: "audio/mpeg".to_string(),
                file_name: "podcast.mp3".to_string(),
                duration: 245,
                title: "Building Rustgram in Rust".to_string(),
                performer: "Rustgram Contributors".to_string(),
                is_audio: true,
                ..Default::default()
            },
            DocumentMeta {
                id: 203,
                mime_type: "image/png".to_string(),
                file_name: "badge.png".to_string(),
                width: 24,
                height: 24,
                ..Default::default()
            },
        ],
        blocks: vec![
            PageBlock::Cover(Box::new(PageBlock::Photo {
                photo_id: 101,
                caption: PageCaption {
                    text: RichText::plain("Cover photograph for the Instant View showcase."),
                    credit: RichText::plain("Photo: Rustgram"),
                },
                url: None,
                webpage_id: None,
                spoiler: false,
            })),
            PageBlock::Kicker(RichText::plain("Engineering Notes")),
            PageBlock::Title(RichText::plain("Rustgram Instant View Renderer")),
            PageBlock::Subtitle(RichText::plain(
                "A faithful Rust port of Telegram Desktop's Instant View pipeline",
            )),
            PageBlock::AuthorDate {
                author: RichText::plain("Rustgram Team"),
                published_date: 1_795_305_600,
            },
            PageBlock::Channel(PageChannel {
                id: 1_000_042,
                title: "Rustgram Updates".to_string(),
                username: Some("rustgram_news".to_string()),
                joined: false,
            }),
            PageBlock::Anchor {
                name: "intro".to_string(),
            },
            PageBlock::Header(RichText::plain("Rich Text Formatting")),
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
                RichText::fixed(RichText::plain("inline_code()")),
                RichText::plain(", "),
                RichText::marked(RichText::plain("highlighted")),
                RichText::plain(", H"),
                RichText::subscript(RichText::plain("2")),
                RichText::plain("O, E=mc"),
                RichText::superscript(RichText::plain("2")),
                RichText::plain(", inline image "),
                RichText::Image {
                    document_id: 203,
                    width: 24,
                    height: 24,
                },
                RichText::plain(", an "),
                RichText::Anchor {
                    text: Box::new(RichText::plain("anchored span")),
                    name: "inline-anchor".to_string(),
                },
                RichText::plain(", a "),
                RichText::Url {
                    text: Box::new(RichText::plain("link to details")),
                    url: "#more-details".to_string(),
                    webpage_id: 0,
                },
                RichText::plain(", email "),
                RichText::Email {
                    text: Box::new(RichText::plain("dev@example.org")),
                    email: "dev@example.org".to_string(),
                },
                RichText::plain(", phone "),
                RichText::Phone {
                    text: Box::new(RichText::plain("+1-555-0100")),
                    phone: "+15550100".to_string(),
                },
                RichText::plain(", and a formatted date "),
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
            PageBlock::Subheader(RichText::plain("Code & Quotes")),
            PageBlock::Preformatted {
                text: RichText::plain("fn main() {\n    println!(\"Hello, Instant View!\");\n}"),
                language: "rust".to_string(),
            },
            PageBlock::Blockquote {
                text: RichText::plain("Port behaviour faithfully, but write idiomatic Rust."),
                caption: RichText::plain("Rustgram Architecture"),
                collapsed: false,
            },
            PageBlock::Pullquote {
                text: RichText::plain("Fast, safe HTML generation with zero C++ wrappers."),
                caption: RichText::plain("Design Principle"),
            },
            PageBlock::Divider,
            PageBlock::Header(RichText::plain("Lists & Nested Blocks")),
            PageBlock::List {
                items: vec![
                    PageListItem {
                        task_state: TaskState::Checked,
                        content: ListItemContent::Text(RichText::plain(
                            "Model all PageBlock kinds",
                        )),
                    },
                    PageListItem {
                        task_state: TaskState::Unchecked,
                        content: ListItemContent::Blocks(vec![
                            PageBlock::Paragraph(RichText::plain("Nested block inside list item:")),
                            PageBlock::Preformatted {
                                text: RichText::plain("cargo test -p tg-iv"),
                                language: "sh".to_string(),
                            },
                        ]),
                    },
                ],
            },
            PageBlock::OrderedList {
                reversed: false,
                start: Some(1),
                list_type: Some("1".to_string()),
                items: vec![
                    PageListOrderedItem::text(
                        Some("1".to_string()),
                        RichText::plain("Parse TL Page"),
                    ),
                    PageListOrderedItem::text(
                        Some("2".to_string()),
                        RichText::plain("Generate HTML"),
                    ),
                    PageListOrderedItem::text(
                        Some("3".to_string()),
                        RichText::plain("Serve via WebView2"),
                    ),
                ],
            },
            PageBlock::Header(RichText::plain("Media, Collage, Slideshow & Audio")),
            PageBlock::Video {
                video_id: 201,
                caption: PageCaption {
                    text: RichText::plain("Demo video clip"),
                    credit: RichText::Empty,
                },
                autoplay: false,
                loop_video: true,
                spoiler: false,
            },
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
                    text: RichText::plain("Three-photo collage using Telegram's LayoutMediaGroup."),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Slideshow {
                items: vec![
                    PageBlock::Photo {
                        photo_id: 102,
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
                    text: RichText::plain("Interactive slideshow"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Audio {
                audio_id: 202,
                caption: PageCaption {
                    text: RichText::plain("Episode 1"),
                    credit: RichText::Empty,
                },
            },
            PageBlock::Header(RichText::plain("Table, Details, Embed, Map & Related")),
            PageBlock::Table {
                title: RichText::plain("Crate Overview"),
                bordered: true,
                striped: true,
                compact: false,
                rows: vec![
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::plain("Module")),
                                header: true,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Middle,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Responsibility")),
                                header: true,
                                colspan: 2,
                                rowspan: 1,
                                align: TableAlign::Center,
                                valign: TableValign::Middle,
                            },
                        ],
                    },
                    PageTableRow {
                        cells: vec![
                            PageTableCell {
                                text: Some(RichText::fixed(RichText::plain("prepare"))),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("Page to HTML conversion")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Left,
                                valign: TableValign::Top,
                            },
                            PageTableCell {
                                text: Some(RichText::plain("XSS-safe")),
                                header: false,
                                colspan: 1,
                                rowspan: 1,
                                align: TableAlign::Right,
                                valign: TableValign::Bottom,
                            },
                        ],
                    },
                ],
            },
            PageBlock::Anchor {
                name: "more-details".to_string(),
            },
            PageBlock::Details {
                title: RichText::plain("Technical Details"),
                open: true,
                blocks: vec![PageBlock::Paragraph(RichText::plain(
                    "Collapsible content rendered with native HTML5 <details>.",
                ))],
            },
            PageBlock::Embed {
                url: Some("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ".to_string()),
                html: None,
                poster_photo_id: None,
                width: Some(640),
                height: Some(360),
                full_width: true,
                allow_scrolling: false,
                caption: PageCaption {
                    text: RichText::plain("Sandboxed allowlisted iframe embed."),
                    credit: RichText::Empty,
                },
            },
            PageBlock::EmbedPost {
                url: "https://t.me/telegram/1".to_string(),
                webpage_id: 555,
                author_photo_id: 105,
                author: "Telegram".to_string(),
                date: 1_795_305_600,
                blocks: vec![PageBlock::Paragraph(RichText::plain(
                    "Embedded post content.",
                ))],
                caption: PageCaption::default(),
            },
            PageBlock::Map {
                geo: GeoPoint {
                    lat: 37.7749,
                    lon: -122.4194,
                    access_hash: 0,
                },
                zoom: 13,
                width: 600,
                height: 300,
                caption: PageCaption {
                    text: RichText::plain("San Francisco"),
                    credit: RichText::plain("OpenStreetMap"),
                },
            },
            PageBlock::RelatedArticles {
                title: RichText::plain("Related Articles"),
                articles: vec![PageRelatedArticle {
                    url: "https://telegra.ph/Another-Article-01-01".to_string(),
                    webpage_id: 777,
                    title: Some("Another Instant View Article".to_string()),
                    description: Some("A short summary of the related article.".to_string()),
                    photo_id: Some(102),
                    author: Some("Jane Doe".to_string()),
                    published_date: Some(1_795_305_600),
                }],
            },
            PageBlock::Footer(RichText::plain("End of sample Instant View page.")),
        ],
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let Some(out_path) = args.next() else {
        eprintln!("Usage: cargo run -p tg-iv --example iv_render -- <output.html>");
        process::exit(2);
    };

    let page = build_sample_page();
    let options = RenderOptions {
        inline_assets: true,
        include_bridge: true,
        embed_policy: EmbedPolicy::default(),
        ..Default::default()
    };
    let prepared = prepare_page(&page, &options);

    if let Err(err) = fs::write(&out_path, &prepared.html) {
        eprintln!("Failed to write '{out_path}': {err}");
        process::exit(1);
    }

    println!(
        "Wrote {} ({} bytes, {} anchors, {} photos, {} documents)",
        out_path,
        prepared.html.len(),
        prepared.anchors.len(),
        prepared.photo_ids.len(),
        prepared.document_ids.len(),
    );
}
