//! Panic regression and fuzz tests over untrusted-input entry points (TGB-3065).

use tg_fuzz::Fuzz;

use crate::editor::clipboard_import::{self, ClipboardPayload};
use crate::grouped_layout::MediaSize;
use crate::rich_page::{EntityInText, EntityType, RichMessageLimits, TextWithEntities};
use crate::search::SearchOptions;

const ITERATIONS: usize = 3_000;

#[test]
fn scheme_checks_with_multibyte_char_at_the_boundary_do_not_panic() {
    // Old code sliced `trimmed[..10]`, `trimmed[..8]` and `trimmed[..5]` unchecked.
    assert_eq!(
        crate::controller::tonsite_to_https("tonsite:/\u{e9}x"),
        None
    );
    assert_eq!(
        crate::controller::https_to_tonsite("https:/\u{e9}x"),
        "https:/\u{e9}x"
    );
    assert!(crate::editor::clipboard_import::resolve_local_media("abcd\u{e9}x", None).is_none());
}

const URL_TOKENS: &[&str] = &[
    "tonsite://",
    "https://",
    "http://",
    "mailto:",
    "tel:",
    "tg://",
    "t.me/",
    "file:",
    "data:",
    "data:image/png;base64,",
    "iVBORw0KGgo=",
    "#",
    "#anchor",
    "?",
    "&",
    "=",
    "magic.org",
    "-h",
    "-d",
    "telegra.ph/",
    "javascript:",
    "//",
    "..",
    "%2F",
    "\\\\server\\share",
];

const MARKUP_TOKENS: &[&str] = &[
    "<p>",
    "</p>",
    "<b>",
    "</b>",
    "<a href=\"",
    "\">",
    "</a>",
    "<img src=\"",
    "<table>",
    "<tr>",
    "<td colspan=\"",
    "\" rowspan=\"",
    "</td>",
    "<ul>",
    "<li>",
    "<pre>",
    "&amp;",
    "&#x",
    "&#",
    "&lt;",
    ";",
    "# ",
    "## ",
    "- ",
    "1. ",
    "> ",
    "```",
    "|",
    "---",
    "$$",
    "$",
    "\\(",
    "[^1]",
    "![",
    "](",
    "<!--",
    "-->",
    "{",
    "}",
];

const TYPES: &[EntityType] = &[
    EntityType::Bold,
    EntityType::Italic,
    EntityType::Code,
    EntityType::Pre,
    EntityType::Blockquote,
    EntityType::Spoiler,
    EntityType::CustomUrl,
    EntityType::Url,
    EntityType::MentionName,
    EntityType::Hashtag,
];

fn random_text_with_entities(fuzz: &mut Fuzz) -> TextWithEntities {
    let text = fuzz.text(MARKUP_TOKENS, 20);
    let len = text.encode_utf16().count();
    let entities = (0..fuzz.rng().below(5))
        .map(|_| EntityInText {
            kind: *fuzz.rng().pick(TYPES),
            offset: fuzz.rng().int(len) as usize,
            length: fuzz.rng().int(len) as usize,
            data: fuzz.text(URL_TOKENS, 3),
        })
        .collect();
    TextWithEntities { text, entities }
}

#[test]
fn links_bridge_json_and_strings_do_not_panic() {
    let mut fuzz = Fuzz::new(0x1F00_0001, ITERATIONS * 4);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(URL_TOKENS, 10);
        let json = fuzz.text(
            &[
                "{",
                "}",
                "\"event\"",
                ":",
                "\"",
                ",",
                "[",
                "]",
                "\"anchor\"",
                "1",
            ],
            16,
        );
        let total = fuzz.rng().int(1 << 20) as usize;
        let ts = fuzz.rng().int(2_000_000_000) as i32;
        fuzz.check("iv strings", i, &(&s, &json), || {
            let _ = crate::controller::tonsite_to_https(&s);
            let _ = crate::controller::https_to_tonsite(&s);
            let _ = crate::controller::prefer_iv_for_uri(&s);
            let _ = crate::controller::is_good_tonsite_url(&s);
            let _ = crate::controller::classify_link(&s, Some(&s), Some(1));
            let _ = crate::controller::BridgeMessage::parse_json(&json);
            let mut controller = crate::controller::IvController::new("https://iv.local", 100);
            let _ = controller.handle_bridge_json(&json);
            let _ = controller.navigation_policy(&s, false);
            let _ = crate::serve::parse_range_header(&s, total);
            let _ = crate::prepare::safe_url(&s);
            let _ = crate::prepare::format_utc_date(ts);
            let _ = crate::html_export::safe_export_url(&s);
            let _ = crate::model::site_name_from_url(&s);
            let _ = crate::rich_page::parse_inline_text_object_entity(&s);
            let _ = crate::rich_page::decode_rich_page_link_url(&s);
            let _ = crate::rich_page::inline_formula_copy_source(&s);
            let _ = clipboard_import::data_uri_image_content(&s);
            let _ = clipboard_import::resolve_local_media(&s, None);
        });
    }
    fuzz.finish();
}

#[test]
fn markdown_html_and_rich_text_do_not_panic() {
    let mut fuzz = Fuzz::new(0x1F00_0002, ITERATIONS);
    let limits = RichMessageLimits::default();
    for i in 0..fuzz.iterations() {
        let markup = fuzz.text(MARKUP_TOKENS, 40);
        fuzz.check("markdown and clipboard", i, &markup, || {
            let _ = crate::markdown::parser::markdown_to_rich_page(&markup);
            let _ = crate::markdown::parser::decode_markdown_escapes(&markup);
            let _ = crate::markdown::parser::anchor_id_base_from_text(&markup);
            let _ = crate::markdown::parser::first_info_token(&markup);
            let _ = clipboard_import::decode_html_entities(&markup);
            let _ = clipboard_import::looks_like_exported_html(&markup);
            let payload = ClipboardPayload {
                html: Some(markup.clone()),
                plain_text: Some(markup.clone()),
                base_path: None,
                spreadsheet_hint: false,
            };
            let _ = clipboard_import::import_from_clipboard(&payload, &limits, 0);
            let _ = crate::search::find_matches_in_str(
                &markup,
                &markup.chars().take(1).collect::<String>(),
                SearchOptions::default(),
            );
        });
        let text = random_text_with_entities(&mut fuzz);
        let from = fuzz.rng().int(40) as usize;
        let till = fuzz.rng().int(40) as usize;
        fuzz.check("rich text", i, &text, || {
            let _ = crate::rich_page::slice_text_with_entities(&text, from, till);
            let mut t = text.clone();
            t.sort_entities();
            t.trim_in_place();
            let mut t = text.clone();
            crate::rich_page::expand_inline_text_objects(&mut t);
            let page = crate::rich_page::split_text_into_rich_page(text.clone());
            let _ = crate::rich_page::flatten_rich_page_summary(&page, true);
            let _ = crate::rich_page::flatten_rich_page_to_simple_text(&page);
            let _ = crate::rich_page::count_rich_page_blocks(&page);
            let _ = crate::rich_page::determine_rich_page_rtl(&page);
            let _ = crate::rich_page::utf16_to_byte_offset(&text.text, from);
            let _ = crate::rich_page::byte_to_utf16_offset(&text.text, till);
        });
        let sizes: Vec<MediaSize> = (0..fuzz.rng().below(12))
            .map(|_| MediaSize {
                width: fuzz.rng().int(4000) as i32,
                height: fuzz.rng().int(4000) as i32,
            })
            .collect();
        // Widths and spacing come from our own layout; only the media sizes come from the server.
        let max_width = 100 + fuzz.rng().below(2000) as i32;
        let min_width = fuzz.rng().below(200) as i32;
        let spacing = fuzz.rng().below(20) as i32;
        fuzz.check(
            "layout_media_group",
            i,
            &(&sizes, max_width, min_width, spacing),
            || {
                let _ = crate::grouped_layout::layout_media_group(
                    &sizes, max_width, min_width, spacing,
                );
            },
        );
    }
    fuzz.finish();
}

#[test]
fn single_media_with_huge_server_height_does_not_overflow() {
    // Old code computed `height * width` in i32 for the one-item group.
    let layout = crate::grouped_layout::layout_media_group(
        &[MediaSize {
            width: 2972,
            height: i32::MAX,
        }],
        430,
        100,
        4,
    );
    assert_eq!(layout.len(), 1);
}
