//! Integration tests for WP-49 (`tg-iv` extras):
//! - `cached_media` + `IvServer` photo/document/geo-map tile serving & disk LRU persistence
//! - `html_export` rich page HTML export & XSS/script sanitization
//! - `search` (`IvSearchController`) Unicode, overlapping, multi-block, details & snippet highlighting
//! - `editor::clipboard_import` HTML/Markdown/TSV sanitization & local media safety
//! - `editor::math` LaTeX delimiter validation & live preview state

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tg_iv::cached_media::{
    CachedMediaKey, GEO_POINT_ZOOM_MIN, IvCachedMedia, cached_page_document_copy_text,
    normalize_geo_zoom, serialize_native_iv_channel_context, sniff_content_type,
};
use tg_iv::editor::clipboard_import::{
    ClipboardPayload, blocks_from_html, blocks_from_markdown, can_paste_without_confirmation,
    data_uri_image_content, import_from_clipboard, imported_media_paths_to_confirm,
    imported_media_placeholder_index, is_network_path, looks_like_exported_html,
    table_from_delimited_text, table_import_limits_for,
};
use tg_iv::editor::math::{
    FORMULA_SAMPLES, MathBoxState, MathValidationError, normalize_math_source,
    strip_math_delimiters, validate_math_formula,
};
use tg_iv::html_export::{
    HtmlExportOptions, encode_base64, export_rich_page_html, export_rich_page_to_dir,
    rich_blocks_clipboard_html, safe_export_url, sanitize_file_name_part,
    sanitize_raw_html_fragment,
};
use tg_iv::markdown::MathKind;
use tg_iv::model::{Page, PageBlock, RichText};
use tg_iv::prepare::{RenderOptions, prepare_page};
use tg_iv::rich_page::{
    EntityInText, EntityType, GroupedMediaIntent, GroupedMediaItem, RICH_EXPORT_GENERATOR_MARKER,
    RichBlock, RichBlockKind, RichListKind, RichMessageLimits, RichPage, RichRelatedArticle,
    RichTableAlignment, RichTableCell, RichTableRow, RichTableVerticalAlignment, RichTaskState,
    RichTextNode, TextWithEntities,
};
use tg_iv::search::{IvSearchController, SearchOptions, SearchSource};
use tg_iv::serve::IvServer;

static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

fn make_temp_dir(prefix: &str) -> PathBuf {
    let id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("tg_iv_wp49_{prefix}_{pid}_{id}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn sample_png_bytes() -> Vec<u8> {
    // Minimal 1x1 PNG header + signature bytes sufficient for content-type sniffing & byte checks.
    vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00,
    ]
}

fn sample_jpeg_bytes() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0xFF, 0xD9,
    ]
}

// ============================================================================
// 1. IvCachedMedia + IvServer tests
// ============================================================================

#[test]
fn cached_media_content_type_sniffing_detects_common_formats() {
    assert_eq!(sniff_content_type(&sample_png_bytes()), "image/png");
    assert_eq!(sniff_content_type(&sample_jpeg_bytes()), "image/jpeg");
    assert_eq!(sniff_content_type(b"GIF89a\x01\x00"), "image/gif");
    assert_eq!(
        sniff_content_type(b"RIFF\x20\x00\x00\x00WEBP"),
        "image/webp"
    );
    assert_eq!(sniff_content_type(b"RIFF\x20\x00\x00\x00WAVE"), "audio/wav");
    assert_eq!(sniff_content_type(b"BM\x00\x00"), "image/bmp");
    assert_eq!(sniff_content_type(b"\x00\x00\x00\x18ftypmp42"), "video/mp4");
    assert_eq!(
        sniff_content_type(b"\x00\x00\x00\x18ftypavif"),
        "image/avif"
    );
    assert_eq!(sniff_content_type(b"\x1A\x45\xDF\xA3\x93"), "video/webm");
    assert_eq!(sniff_content_type(b"OggS\x00\x02"), "audio/ogg");
    assert_eq!(sniff_content_type(b"ID3\x04\x00"), "audio/mpeg");
    assert_eq!(sniff_content_type(b"fLaC\x00\x00"), "audio/flac");
    assert_eq!(sniff_content_type(b"%PDF-1.7"), "application/pdf");
    assert_eq!(
        sniff_content_type(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
        "image/svg+xml"
    );
    assert_eq!(
        sniff_content_type(b"\x01\x02\x03\x04"),
        "application/octet-stream"
    );
}

#[test]
fn iv_server_serves_real_cached_png_jpeg_and_map_tiles_and_falls_back_when_uncached() {
    let page = Page {
        url: "https://example.org/article".to_string(),
        blocks: vec![PageBlock::Paragraph(RichText::Plain("Hello".to_string()))],
        ..Page::default()
    };
    let prepared = prepare_page(&page, &RenderOptions::default());

    let cache = Arc::new(IvCachedMedia::with_defaults());
    let png = sample_png_bytes();
    let jpeg = sample_jpeg_bytes();
    let map_png = [sample_png_bytes(), b"MAPTILE".to_vec()].concat();

    // Insert with `None` content-type to verify automatic magic-byte sniffing.
    cache.insert_photo(101, None, png.clone());
    cache.insert_photo(102, None, jpeg.clone());
    cache.insert_map_tile(51.5074, -0.1278, 14, 400, 300, None, map_png.clone());

    let mut server = IvServer::with_cached_media(prepared, Arc::clone(&cache));

    let resp_png = server.serve_cached("/media/photo/101", None);
    assert_eq!(resp_png.status, 200);
    assert_eq!(resp_png.content_type, "image/png");
    assert_eq!(resp_png.body, png);

    let resp_jpeg = server.serve_cached("tg-iv://localhost/media/photo/102", None);
    assert_eq!(resp_jpeg.status, 200);
    assert_eq!(resp_jpeg.content_type, "image/jpeg");
    assert_eq!(resp_jpeg.body, jpeg);

    let resp_map = server.serve_cached("/media/map/51.5074,-0.1278,14,400x300", None);
    assert_eq!(resp_map.status, 200);
    assert_eq!(resp_map.content_type, "image/png");
    assert_eq!(resp_map.body, map_png);

    // HTTP Range request over cached photo bytes.
    let resp_range = server.serve_cached("/media/photo/101", Some("bytes=0-7"));
    assert_eq!(resp_range.status, 206);
    assert_eq!(resp_range.content_type, "image/png");
    assert_eq!(resp_range.body, &png[..8]);

    // Uncached photo and map tile return placeholder SVG by default.
    let missing_photo = server.serve_cached("/media/photo/999", None);
    assert_eq!(missing_photo.status, 200);
    assert_eq!(missing_photo.content_type, "image/svg+xml; charset=utf-8");

    let missing_map = server.serve_cached("/media/map/10.0,20.0,15,320x240", None);
    assert_eq!(missing_map.status, 200);
    assert_eq!(missing_map.content_type, "image/svg+xml; charset=utf-8");

    // When placeholder fallback is disabled, uncached photo and map tile return 404.
    server.set_placeholder_on_missing_photo(false);
    assert_eq!(server.serve_cached("/media/photo/999", None).status, 404);
    assert_eq!(
        server
            .serve_cached("/media/map/10.0,20.0,15,320x240", None)
            .status,
        404
    );
    // Cached entries still return 200 even with placeholder disabled.
    assert_eq!(server.serve_cached("/media/photo/101", None).status, 200);
    assert_eq!(
        server
            .serve_cached("/media/map/51.5074,-0.1278,14,400x300", None)
            .status,
        200
    );
}

#[test]
fn cached_media_lru_eviction_and_disk_backing_round_trip() {
    let dir = make_temp_dir("cache_disk");
    // Budget fits 2 small photos (each ~29 bytes) in RAM, evicting LRU on the 3rd.
    let cache = IvCachedMedia::with_disk_dir(&dir, 64, 2).expect("disk cache init");

    let p1 = sample_png_bytes();
    let p2 = sample_jpeg_bytes();
    let p3 = [sample_png_bytes(), vec![1, 2, 3]].concat();

    cache.insert_photo(1, None, p1.clone());
    cache.insert_photo(2, None, p2.clone());
    // Promote photo 1 to MRU so photo 2 becomes LRU.
    assert_eq!(cache.get_photo(1).expect("p1").bytes, p1);
    cache.insert_photo(3, None, p3.clone());

    assert!(cache.contains_in_memory(&CachedMediaKey::Photo(1)));
    assert!(cache.contains_in_memory(&CachedMediaKey::Photo(3)));
    assert!(!cache.contains_in_memory(&CachedMediaKey::Photo(2)));
    assert!(cache.stats().evictions >= 1);

    // Photo 2 was evicted from RAM but must reload transparently from disk with original MIME type!
    let reloaded = cache.get_photo(2).expect("reloaded p2 from disk");
    assert_eq!(reloaded.content_type, "image/jpeg");
    assert_eq!(reloaded.bytes, p2);
    assert_eq!(cache.stats().disk_hits, 1);

    // `release_heavy_data()` drops RAM entries while retaining disk files.
    cache.release_heavy_data();
    assert_eq!(cache.stats().entry_count, 0);
    assert_eq!(cache.get_photo(3).expect("reloaded p3").bytes, p3);
    assert_eq!(cache.stats().disk_hits, 2);

    // Helpers ported from iv_cached_media.cpp.
    assert_eq!(normalize_geo_zoom(8), GEO_POINT_ZOOM_MIN);
    assert_eq!(normalize_geo_zoom(16), 16);
    assert_eq!(serialize_native_iv_channel_context(42, "news"), "42\nnews");
    assert_eq!(
        cached_page_document_copy_text("Track", "Artist", "track.mp3", true),
        "Track\nArtist"
    );

    cache.clear();
    assert!(cache.get_photo(1).is_none());
    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// 2. html_export tests
// ============================================================================

#[test]
fn html_export_renders_rich_blocks_and_strips_script_and_javascript_vectors() {
    let mut heading = RichBlock::heading(2, "Overview <script>alert('xss')</script>");
    heading.anchor_id = "overview".to_string();

    let mut table = RichBlock::new(RichBlockKind::Table);
    table.bordered = true;
    table.striped = true;
    table.text = RichTextNode::plain("Metrics Table");
    table.table_rows = vec![
        RichTableRow {
            cells: vec![
                RichTableCell {
                    text: RichTextNode::plain("Metric"),
                    header: true,
                    alignment: RichTableAlignment::Left,
                    ..RichTableCell::default()
                },
                RichTableCell {
                    text: RichTextNode::plain("Value"),
                    header: true,
                    alignment: RichTableAlignment::Right,
                    vertical_alignment: RichTableVerticalAlignment::Middle,
                    ..RichTableCell::default()
                },
            ],
        },
        RichTableRow {
            cells: vec![
                RichTableCell {
                    text: RichTextNode::plain("Latency"),
                    colspan: 1,
                    rowspan: 1,
                    ..RichTableCell::default()
                },
                RichTableCell {
                    text: RichTextNode::plain("12ms"),
                    alignment: RichTableAlignment::Center,
                    ..RichTableCell::default()
                },
            ],
        },
    ];

    let mut quote = RichBlock::new(RichBlockKind::Quote);
    quote.text = RichTextNode::plain("Stay hungry, stay foolish.");
    quote.caption = RichTextNode::plain("Speaker");

    let code = RichBlock::code(
        "rust",
        "fn main() { println!(\"<script>bad()</script>\"); }",
    );

    // Paragraph with one safe link and two malicious injection links.
    let mut para_text = TextWithEntities::simple("Safe Evil ScriptLink");
    para_text.entities = vec![
        EntityInText::with_data(EntityType::CustomUrl, 0, 4, "https://example.org/ok"),
        EntityInText::with_data(
            EntityType::CustomUrl,
            5,
            4,
            "javascript:alert(document.cookie)",
        ),
        EntityInText::with_data(
            EntityType::CustomUrl,
            10,
            10,
            "data:text/html,<script>alert(1)</script>",
        ),
        EntityInText::new(EntityType::Bold, 0, 4),
    ];
    let mut para = RichBlock::new(RichBlockKind::Paragraph);
    para.text = RichTextNode::from_text(para_text);

    // Embed with unsafe URL and raw `<script>` HTML — neither may execute!
    let mut embed = RichBlock::new(RichBlockKind::Embed);
    embed.url = "javascript:alert(99)".to_string();
    embed.html = b"<script>evil()</script><iframe>bad</iframe>".to_vec();

    // Related article with malicious URL.
    let mut related = RichBlock::new(RichBlockKind::RelatedArticles);
    related.related_articles.push(RichRelatedArticle {
        url: "vbscript:msgbox(1)".to_string(),
        title: "Related <img src=x onerror=alert(1)>".to_string(),
        ..RichRelatedArticle::default()
    });

    let page = RichPage {
        blocks: vec![heading, table, quote, code, para, embed, related],
        ..RichPage::default()
    };

    let exported = export_rich_page_html(&page, &HtmlExportOptions::default());

    assert!(exported.html.contains("<!DOCTYPE html>"));
    assert!(exported.html.contains(RICH_EXPORT_GENERATOR_MARKER));
    assert!(exported.html.contains(
        "<h2 id=\"overview\">Overview &lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;</h2>"
    ));
    assert!(exported.html.contains("<table class=\"bordered striped\">"));
    assert!(
        exported
            .html
            .contains("<th style=\"text-align:right;vertical-align:middle;\">Value</th>")
    );
    assert!(
        exported
            .html
            .contains("<blockquote>Stay hungry, stay foolish.<cite>Speaker</cite></blockquote>")
    );
    assert!(exported.html.contains("<pre class=\"code\"><code class=\"language-rust\">fn main() { println!(&quot;&lt;script&gt;bad()&lt;/script&gt;&quot;); }</code></pre>"));
    assert!(
        exported
            .html
            .contains("<a href=\"https://example.org/ok\"><strong>Safe</strong></a>")
    );

    // No unescaped `<script>`, `<img>` tag injection, or `javascript:`/`vbscript:`/`data:text/html` links anywhere!
    assert!(!exported.html.contains("<script>"));
    assert!(!exported.html.to_ascii_lowercase().contains("javascript:"));
    assert!(!exported.html.to_ascii_lowercase().contains("vbscript:"));
    assert!(!exported.html.contains("<img src=x"));
    assert!(exported.html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert_eq!(exported.stripped_urls, 4);

    // Explicit raw HTML fragment sanitizer test.
    let cleaned = sanitize_raw_html_fragment(
        "<p onclick=\"alert(1)\">Hello <script>steal()</script><a href=\"javascript:void(0)\">bad</a><a href=\"https://ok.test\">good</a></p>",
    );
    assert!(!cleaned.contains("<script"));
    assert!(!cleaned.contains("onclick"));
    assert!(!cleaned.contains("javascript:"));
    assert!(cleaned.contains("href=\"https://ok.test\""));
    assert!(cleaned.contains("Hello "));

    // URL validator edge cases.
    assert_eq!(
        safe_export_url("https://example.org/a"),
        Some("https://example.org/a".to_string())
    );
    assert_eq!(
        safe_export_url("#Section One"),
        Some("#section one".to_string())
    );
    assert!(safe_export_url("javascript:alert(1)").is_none());
    assert!(safe_export_url("https://example.org/\nfoo").is_none());
    assert_eq!(
        sanitize_file_name_part("Hello: World/Test...", ""),
        "Hello_ World_Test"
    );
}

#[test]
fn html_export_to_dir_and_clipboard_embed_media_from_cached_store() {
    let dir = make_temp_dir("html_export");
    let cache = IvCachedMedia::with_defaults();
    let png = sample_png_bytes();
    cache.insert_photo(77, None, png.clone());

    let mut photo_block = RichBlock::new(RichBlockKind::Photo);
    photo_block.photo_id = 77;
    photo_block.width = 100;
    photo_block.height = 50;
    photo_block.caption = RichTextNode::plain("A cached diagram");

    let mut collage = RichBlock::new(RichBlockKind::GroupedMedia);
    collage.media_intent = GroupedMediaIntent::Collage;
    collage.media_items = vec![
        GroupedMediaItem {
            kind: RichBlockKind::Photo,
            photo_id: 77,
            width: 400,
            height: 300,
            ..GroupedMediaItem::default()
        },
        GroupedMediaItem {
            kind: RichBlockKind::Photo,
            photo_id: 77,
            width: 400,
            height: 300,
            ..GroupedMediaItem::default()
        },
    ];

    let page = RichPage {
        blocks: vec![RichBlock::heading(1, "Export Demo"), photo_block, collage],
        ..RichPage::default()
    };

    let disk1 = export_rich_page_to_dir(&page, &dir, Some(&cache), &HtmlExportOptions::default())
        .expect("export to dir #1");
    assert!(disk1.html_path.is_file());
    assert_eq!(disk1.media_files.len(), 1);
    assert_eq!(fs::read(&disk1.media_files[0]).expect("read media"), png);
    let written_html = fs::read_to_string(&disk1.html_path).expect("read html");
    assert!(written_html.contains("src=\"media/photo_77.png\""));
    assert!(written_html.contains("<tg-collage>"));

    // Second export to same dir picks a collision-free sibling folder ("Export Demo (2)").
    let disk2 = export_rich_page_to_dir(&page, &dir, Some(&cache), &HtmlExportOptions::default())
        .expect("export to dir #2");
    assert_ne!(disk1.folder, disk2.folder);
    assert!(disk2.html_path.is_file());

    // Clipboard HTML inlines data URI.
    let clip_html = rich_blocks_clipboard_html(&page.blocks, false, Some(&cache));
    let expected_prefix = format!("data:image/png;base64,{}", encode_base64(&png));
    assert!(clip_html.contains(&expected_prefix));
    assert!(clip_html.contains("data-tg-src=\"tg://photo?id=77\""));

    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// 3. IvSearchController tests
// ============================================================================

#[test]
fn search_controller_finds_overlapping_multiblock_unicode_matches_and_cycles_cleanly() {
    let mut details = RichBlock::new(RichBlockKind::Details);
    details.open = false;
    details.anchor_id = "sec-secret".to_string();
    details.text = RichTextNode::plain("Click to expand");
    details.blocks = vec![RichBlock::paragraph("Hidden banana inside details")];

    let page = RichPage {
        blocks: vec![
            RichBlock::heading(1, "Banana Split"),
            RichBlock::paragraph("bananas and Café приветствие"),
            details,
        ],
        ..RichPage::default()
    };

    let mut ctrl = IvSearchController::from_rich_page(&page);
    assert!(!ctrl.is_shown());
    assert!(ctrl.toggle());

    // Overlapping matches: "ana" appears twice in "Banana", twice in "bananas", and twice in hidden "banana" = 6!
    assert_eq!(ctrl.set_query("ANA"), 6);
    assert_eq!(ctrl.display_position(), (1, 6));
    assert_eq!(
        ctrl.current_match()
            .map(|m| (m.segment, m.byte_start, m.byte_end)),
        Some((0, 1, 4))
    );

    // Step through all 6 matches and verify wrap-around forward and backward.
    let next1 = ctrl.next_match().expect("match 2");
    assert_eq!((next1.segment, next1.byte_start, next1.byte_end), (0, 3, 6));
    assert_eq!(ctrl.display_position(), (2, 6));

    // Jump backward across 0 -> wraps to last match (index 5, inside hidden details).
    ctrl.prev_match();
    assert_eq!(ctrl.display_position(), (1, 6));
    let wrapped_back = ctrl.prev_match().expect("wrapped to last").clone();
    assert_eq!(ctrl.display_position(), (6, 6));
    assert_eq!(
        wrapped_back.hidden_details_id.as_deref(),
        Some("sec-secret")
    );

    // Expanding collapsed details resolves the hidden match into a visible segment.
    let resolved = ctrl
        .expand_details_and_resolve(|anchor| {
            assert_eq!(anchor, "sec-secret");
            Some(vec![
                SearchSource::visible("Banana Split"),
                SearchSource::visible("bananas and Café приветствие"),
                SearchSource::visible("Click to expand"),
                SearchSource::visible("Hidden banana inside details"),
            ])
        })
        .expect("resolved visible match");
    assert!(resolved.hidden_details_id.is_none());
    assert_eq!(resolved.segment, 3);
    assert_eq!(ctrl.display_position(), (6, 6));

    // Stepping forward from last wraps back to 1/6.
    ctrl.next_match();
    assert_eq!(ctrl.display_position(), (1, 6));

    // Highlighted HTML merges overlapping matches in segment 0 cleanly (`B<mark>anana</mark> Split`).
    let highlighted = ctrl.highlight_segment_html(0).expect("segment 0 html");
    assert_eq!(
        highlighted,
        "B<mark class=\"iv-search-hit iv-search-current\">anana</mark> Split"
    );

    // Snippet extraction works cleanly.
    let snippet = ctrl.current_snippet().expect("current snippet");
    assert_eq!(snippet.matched, "ana");
    assert!(
        snippet
            .highlighted_html
            .contains("<mark class=\"iv-search-hit\">ana</mark>")
    );

    // Unicode UTF-8 case-insensitive matching (accented Latin + Cyrillic).
    assert_eq!(ctrl.set_query("CAFÉ"), 1);
    let cafe_snip = ctrl.current_snippet().expect("cafe snippet");
    assert_eq!(cafe_snip.matched, "Café");

    assert_eq!(ctrl.set_query("ПРИВЕТ"), 1);
    let ru_snip = ctrl.current_snippet().expect("cyrillic snippet");
    assert_eq!(ru_snip.matched, "привет");

    // Non-overlapping option finds only 1 "ana" per "banana".
    ctrl.set_options(SearchOptions {
        allow_overlapping: false,
        ..SearchOptions::default()
    });
    assert_eq!(ctrl.set_query("ana"), 3);

    // Hide clears active matches.
    ctrl.hide();
    assert!(!ctrl.is_shown());
    assert_eq!(ctrl.match_count(), 0);
    assert_eq!(ctrl.display_position(), (0, 0));
}

// ============================================================================
// 4. editor::clipboard_import tests
// ============================================================================

#[test]
fn clipboard_import_converts_html_lists_links_bold_and_strips_scripts_and_unsafe_paths() {
    let limits = RichMessageLimits::default();
    let html = r#"
        <html>
        <head><title>Ignored</title><style>body { color: red; }</style></head>
        <body>
            <script>alert('xss')</script>
            <h1>Project Plan</h1>
            <p onclick="evil()">Read the <strong>important</strong> <a href="https://example.org/docs">guide</a> and <a href="javascript:alert(1)">unsafe</a>.</p>
            <ul>
                <li>First <b>bold</b> item</li>
                <li><input type="checkbox" checked> Done task</li>
            </ul>
            <ol start="3">
                <li>Third item</li>
            </ol>
            <img src="file://evil-server/share/steal.png">
        </body>
        </html>
    "#;

    let imported = blocks_from_html(html, None, &limits, 0).expect("html import");
    assert!(!imported.truncated);
    assert!(imported.local_media.is_empty()); // UNC/remote file URL rejected!
    assert_eq!(imported.blocks.len(), 4);

    // Block 0: Heading 1
    assert_eq!(imported.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(imported.blocks[0].heading_level, 1);
    assert_eq!(imported.blocks[0].text.text.text, "Project Plan");

    // Block 1: Paragraph with bold + safe link, javascript: link stripped
    let p = &imported.blocks[1];
    assert_eq!(p.kind, RichBlockKind::Paragraph);
    assert_eq!(p.text.text.text, "Read the important guide and unsafe.");
    assert!(
        p.text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Bold)
    );
    assert!(
        p.text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::CustomUrl && e.data == "https://example.org/docs")
    );
    assert!(
        !p.text
            .text
            .entities
            .iter()
            .any(|e| e.data.contains("javascript:"))
    );

    // Block 2: Unordered list with bold item + checked task item
    let ul = &imported.blocks[2];
    assert_eq!(ul.kind, RichBlockKind::List);
    assert_eq!(ul.list_kind, RichListKind::Bullet);
    assert_eq!(ul.list_items.len(), 2);
    assert_eq!(ul.list_items[0].text.text.text, "First bold item");
    assert!(
        ul.list_items[0]
            .text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Bold)
    );
    assert_eq!(ul.list_items[1].task_state, RichTaskState::Checked);
    assert_eq!(ul.list_items[1].text.text.text, "Done task");

    // Block 3: Ordered list starting at 3
    let ol = &imported.blocks[3];
    assert_eq!(ol.kind, RichBlockKind::List);
    assert_eq!(ol.list_kind, RichListKind::Ordered);
    assert_eq!(ol.ordered_list.start, Some(3));
    assert_eq!(ol.list_items[0].text.text.text, "Third item");

    // UNC path security check.
    assert!(is_network_path(r"\\fileserver\share\img.png"));
    assert!(is_network_path("//fileserver/share/img.png"));
    assert!(!is_network_path("media/photo_1.png"));
}

#[test]
fn clipboard_import_handles_exported_html_fixture_data_uri_markdown_and_tsv() {
    let dir = make_temp_dir("clip_fixture");
    let media_dir = dir.join("media");
    fs::create_dir_all(&media_dir).expect("media dir");
    let local_img = media_dir.join("pic.png");
    let png = sample_png_bytes();
    fs::write(&local_img, &png).expect("write local png");

    // Also write an exported HTML file fixture locally.
    let fixture_html = format!(
        "<!DOCTYPE html><html><head><meta name=\"generator\" content=\"{RICH_EXPORT_GENERATOR_MARKER}\"></head>\
         <body><img src=\"media/pic.png\"><img src=\"data:image/png;base64,{}\"></body></html>",
        encode_base64(&png)
    );
    let html_file = dir.join("export.html");
    fs::write(&html_file, &fixture_html).expect("write fixture html");
    assert!(looks_like_exported_html(&fixture_html));

    let limits = RichMessageLimits::default();
    let payload = ClipboardPayload {
        html: Some(fs::read_to_string(&html_file).expect("read fixture")),
        base_path: Some(dir.clone()),
        ..ClipboardPayload::default()
    };
    let imported = import_from_clipboard(&payload, &limits, 0).expect("import fixture");
    assert_eq!(imported.blocks.len(), 2);
    assert_eq!(imported.local_media.len(), 2);
    assert!(can_paste_without_confirmation(&imported));
    assert!(imported_media_paths_to_confirm(&imported).is_empty());
    assert_eq!(
        imported_media_placeholder_index(imported.blocks[0].photo_id),
        Some(0)
    );
    assert_eq!(
        imported_media_placeholder_index(imported.blocks[1].photo_id),
        Some(1)
    );
    assert_eq!(imported.local_media[1].content, png);

    // Non-image data URI rejected.
    assert!(data_uri_image_content("data:image/png;base64,SGVsbG8gd29ybGQ=").is_none());

    // Markdown clipboard import.
    let md =
        "## Checklist\n- [x] Ship **WP-49**\n- [ ] Follow up with [docs](https://example.org)\n";
    let (md_res, _) = blocks_from_markdown(md, &limits, 0).expect("md import");
    assert_eq!(md_res.blocks.len(), 2);
    assert_eq!(md_res.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(md_res.blocks[1].kind, RichBlockKind::List);
    assert_eq!(
        md_res.blocks[1].list_items[0].task_state,
        RichTaskState::Checked
    );
    assert!(
        md_res.blocks[1].list_items[0]
            .text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Bold)
    );

    // Spreadsheet TSV table import.
    let tsv = "Name\tScore\nAlpha\t10\nBeta\t20\n";
    let tlimits = table_import_limits_for(&limits, 0);
    let table = table_from_delimited_text(tsv, '\t', &tlimits).expect("tsv table");
    assert_eq!(table.block.table_rows.len(), 3);
    assert_eq!(table.block.table_rows[2].cells[1].text.text.text, "20");

    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// 5. editor::math validation & preview tests
// ============================================================================

#[test]
fn math_validator_accepts_valid_latex_and_rejects_malformed_delimiter_mismatches() {
    for sample in FORMULA_SAMPLES {
        assert!(
            validate_math_formula(sample, MathKind::Display).is_ok(),
            "sample should be valid: {sample}"
        );
    }

    assert_eq!(
        validate_math_formula(r"$$ \frac{a+b}{c} $$", MathKind::Display).expect("display"),
        r"\frac{a+b}{c}"
    );
    assert_eq!(
        validate_math_formula(r"\( x^2 + \sqrt[3]{y} \)", MathKind::Inline).expect("inline"),
        r"x^2 + \sqrt[3]{y}"
    );
    assert!(
        validate_math_formula(
            r"\left\{ \begin{pmatrix} 1 & 0 \\ 0 & 1 \end{pmatrix} \right.",
            MathKind::Display
        )
        .is_ok()
    );
    assert_eq!(normalize_math_source("  a +\r\nb  "), "a + b");
    assert_eq!(
        strip_math_delimiters(r"\[E=mc^2\]").expect("strip"),
        ("E=mc^2".to_string(), Some(MathKind::Display))
    );

    // Malformed delimiter mismatches must all be rejected!
    assert!(matches!(
        validate_math_formula("", MathKind::Inline),
        Err(MathValidationError::Empty)
    ));
    assert!(matches!(
        validate_math_formula(r"{x + 1)", MathKind::Inline),
        Err(MathValidationError::MismatchedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"(a + b]", MathKind::Inline),
        Err(MathValidationError::MismatchedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"\begin{pmatrix} 1 & 2 \end{bmatrix}", MathKind::Display),
        Err(MathValidationError::MismatchedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"$$x + 1$", MathKind::Display),
        Err(MathValidationError::MismatchedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"\(x + 1\]", MathKind::Inline),
        Err(MathValidationError::MismatchedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"\frac{1}{2", MathKind::Display),
        Err(MathValidationError::UnclosedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"\left( x + 1", MathKind::Display),
        Err(MathValidationError::UnclosedDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"x + 1 \right)", MathKind::Display),
        Err(MathValidationError::UnexpectedClosingDelimiter { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"x + \", MathKind::Inline),
        Err(MathValidationError::TrailingBackslash { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"\left", MathKind::Inline),
        Err(MathValidationError::MissingCommandArgument { .. })
    ));
    assert!(matches!(
        validate_math_formula(r"a + $b", MathKind::Inline),
        Err(MathValidationError::UnescapedDollar { .. })
    ));
}

#[test]
fn math_box_state_tracks_sample_preview_validation_errors_and_submission() {
    let mut box_state = MathBoxState::new("", false, Some(false), 320);
    assert!(box_state.preview.using_sample);
    assert!(box_state.preview.rendered.is_some());
    assert!(box_state.preview.validation_error.is_none());
    assert!(!box_state.is_valid());
    assert!(box_state.submit().is_err());

    // Enter malformed formula -> validation error surfaced and submit rejected.
    box_state.set_input(r"\frac{a}{b");
    assert!(!box_state.preview.using_sample);
    assert!(box_state.preview.validation_error.is_some());
    assert!(box_state.preview.rendered.is_none());
    assert!(box_state.submit().is_err());

    // Enter valid formula -> renders preview layout and submits cleanly.
    box_state.set_input(r"e^{i\pi} + 1 = 0");
    assert!(box_state.is_valid());
    assert!(box_state.preview.validation_error.is_none());
    assert!(box_state.preview.rendered.is_some());
    assert!(box_state.preview.desired_height >= 72);

    box_state.set_separate_line(true);
    assert_eq!(box_state.kind(), MathKind::Display);

    let block = box_state.to_rich_block().expect("math block");
    assert_eq!(block.kind, RichBlockKind::Math);
    assert_eq!(block.formula, r"e^{i\pi} + 1 = 0");

    let inline = box_state.to_inline_rich_text().expect("inline math");
    assert_eq!(inline.entities.len(), 1);
    assert_eq!(inline.entities[0].kind, EntityType::CustomEmoji);

    let sub = box_state.submit().expect("submit");
    assert_eq!(sub.source, r"e^{i\pi} + 1 = 0");
    assert!(sub.separate_line);
    assert!(!box_state.external_interaction_active);
}
