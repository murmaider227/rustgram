//! Known-answer and unit tests for WP-17 (`tg-iv` rich document engine):
//! - `RichPage` <-> Layer-233 TL `PageBlock` / `InputRichMessage` / `RichMessage` serialization
//! - Markdown validation, math extraction, parsing, and canonical Markdown round-trip
//! - Pure-Rust MicroTeX-equivalent formula layout (fractions, sub/superscripts, roots, matrices, big operators)
//! - Headless rich document editor (blocks, inline styles, undo/redo, cursor/selection, tables, lists, auto-pair)

use tg_iv::editor::{
    ApplyResult, BlockPath, EditSelection, EditTableCellRange, EditorState, FieldMode,
    InsertAction, InsertBlockType, ListStyle, OrderedListType, TextFormattingAction,
    build_table_grid,
};
use tg_iv::markdown::{
    FormulaLayoutRequest, MarkdownParseLimits, MathFontStyle, MathKind, MathLayoutEngine, NodeKind,
    ParseOptions, anchor_id_base_from_text, extract_math_regions, has_strong_math_signal,
    layout_formula, looks_binary, looks_like_currency, looks_like_prose, markdown_to_rich_page,
    parse_markdown_for_iv, rewrite_full_width_environments, rich_page_to_markdown,
    strip_blockquote_markers, validate_markdown_source_for_iv,
};
use tg_iv::rich_page::{
    DirectIdMediaResolver, EntityInText, EntityType, GroupedMediaIntent, GroupedMediaItem,
    InMemoryMediaResolver, InlineTextObjectEntity, InlineTextObjectFormulaData,
    OBJECT_REPLACEMENT_CHAR, RichBlock, RichBlockKind, RichButton, RichButtonAlignment,
    RichButtonColor, RichButtonType, RichMessageLimitError, RichMessageLimits, RichPage,
    RichParseMode, RichTableAlignment, RichTaskState, RichTextNode, SerializeRichMessageMode,
    SerializeRichMessageStatus, TextWithEntities, decode_rich_page_link_url,
    determine_rich_page_rtl, encode_rich_page_link_url, expand_inline_text_objects,
    flatten_rich_page_summary, flatten_rich_page_to_simple_text, parse_inline_text_object_entity,
    rich_page_from_model_page, rich_page_from_tl_blocks, rich_page_from_tl_rich_message,
    rich_page_to_model_page, rich_page_to_tl_blocks, serialize_inline_text_object_entity,
    serialize_input_rich_message, split_grouped_media_block, split_text_into_rich_page,
    validate_rich_message,
};
use tg_tl::{Deserializable, Serializable, enums as tl_enums, types as tl_types};

// ============================================================================
// 1. RichPage <-> TL PageBlock / InputRichMessage / Binary Wire Round-Trip
// ============================================================================

#[test]
fn rich_page_tl_wire_roundtrip_all_editor_blocks() {
    let md = "\
# Article Title

Paragraph with **bold**, *italic*, ~~strike~~, `code`, ||secret||, <u>under</u>, <sub>sub</sub>, <sup>sup</sup>, <mark>hi</mark>, [link](https://example.org), and inline math $x^2 + y^2$.

```rust
fn answer() -> i32 { 42 }
```

> Quoted insight

---

- [ ] Task one
- [x] Task two

1. First
2. Second

$$
\\frac{a+b}{c}
$$

| Name | Score |
| :--- | ---: |
| Alpha | 10 |
| Beta | 20 |

<details open>
<summary>More details</summary>

Hidden explanation paragraph.
</details>";

    let mut page = markdown_to_rich_page(md).expect("markdown parses");

    // Also attach a ButtonRow, Photo, Map, and GroupedMedia to exercise full TL coverage.
    let mut photo = RichBlock::new(RichBlockKind::Photo);
    photo.photo_id = 12345;
    photo.spoiler = true;
    photo.caption = RichTextNode::plain("Sample photo");
    page.blocks.push(photo);

    let mut map = RichBlock::new(RichBlockKind::Map);
    map.latitude = 47.3769;
    map.longitude = 8.5417;
    map.zoom = 15;
    map.caption = RichTextNode::plain("Zurich");
    page.blocks.push(map);

    let mut btn_row = RichBlock::new(RichBlockKind::ButtonRow);
    btn_row.button_alignment = RichButtonAlignment::Center;
    btn_row.buttons.push(RichButton {
        text: RichTextNode::plain("Visit"),
        button_type: RichButtonType::Url,
        color: RichButtonColor::Primary,
        data: b"https://example.org".to_vec(),
        button_id: 0,
        peer_types: 0,
    });
    page.blocks.push(btn_row);

    assert_eq!(
        validate_rich_message(&page, &RichMessageLimits::default()),
        None
    );

    let ser = serialize_input_rich_message(
        &page,
        SerializeRichMessageMode::FinalSubmit,
        &DirectIdMediaResolver,
    );
    assert_eq!(ser.status, SerializeRichMessageStatus::Success);
    let input_msg = ser.value.expect("serialized InputRichMessage");

    // Round-trip through real binary TL wire encoding!
    let wire_bytes = input_msg.to_bytes();
    let decoded_input =
        tl_types::InputRichMessage::from_bytes(&wire_bytes).expect("TL wire decode succeeds");
    assert_eq!(decoded_input, input_msg);

    // Reconstruct RichPage from the decoded TL blocks and verify structural fidelity.
    let reparsed = rich_page_from_tl_blocks(&decoded_input.blocks, RichParseMode::Normal);
    assert_eq!(reparsed.blocks.len(), page.blocks.len());
    assert_eq!(reparsed.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(reparsed.blocks[0].heading_level, 1);
    assert_eq!(reparsed.blocks[0].text.text.text, "Article Title");

    // Verify all inline styles survived TL round-trip
    let p_entities: Vec<EntityType> = reparsed.blocks[1]
        .text
        .text
        .entities
        .iter()
        .map(|e| e.kind)
        .collect();
    for expected in [
        EntityType::Bold,
        EntityType::Italic,
        EntityType::StrikeOut,
        EntityType::Code,
        EntityType::Spoiler,
        EntityType::Underline,
        EntityType::Subscript,
        EntityType::Superscript,
        EntityType::Marked,
        EntityType::CustomUrl,
        EntityType::CustomEmoji, // inline math object
    ] {
        assert!(
            p_entities.contains(&expected),
            "missing entity {expected:?} after TL round-trip"
        );
    }

    // Verify code block, math, table, details, photo, map, button row
    assert_eq!(reparsed.blocks[2].kind, RichBlockKind::Code);
    assert_eq!(reparsed.blocks[2].language, "rust");
    assert_eq!(
        reparsed.blocks[2].text.text.text,
        "fn answer() -> i32 { 42 }"
    );

    let math_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::Math)
        .expect("math block");
    assert_eq!(math_blk.formula, "\\frac{a+b}{c}");

    let table_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::Table)
        .expect("table block");
    assert_eq!(table_blk.table_rows.len(), 3);
    assert_eq!(
        table_blk.table_rows[0].cells[1].alignment,
        RichTableAlignment::Right
    );

    let details_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::Details)
        .expect("details block");
    assert!(details_blk.open);
    assert_eq!(details_blk.text.text.text, "More details");
    assert_eq!(details_blk.blocks.len(), 1);

    let photo_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::Photo)
        .expect("photo block");
    assert_eq!(photo_blk.photo_id, 12345);
    assert!(photo_blk.spoiler);

    let map_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::Map)
        .expect("map block");
    assert_eq!(map_blk.zoom, 15);
    assert!((map_blk.latitude - 47.3769).abs() < 1e-6);

    let btn_blk = reparsed
        .blocks
        .iter()
        .find(|b| b.kind == RichBlockKind::ButtonRow)
        .expect("button row");
    assert_eq!(btn_blk.button_alignment, RichButtonAlignment::Center);
    assert_eq!(btn_blk.buttons.len(), 1);
    assert_eq!(btn_blk.buttons[0].color, RichButtonColor::Primary);
    assert_eq!(btn_blk.buttons[0].button_type, RichButtonType::Url);
}

#[test]
fn rich_page_final_submit_normalization_and_limits() {
    // Empty paragraphs at edges are trimmed; empty page returns EmptyContent.
    let mut empty_page = RichPage::default();
    empty_page.blocks.push(RichBlock::paragraph("   "));
    empty_page.blocks.push(RichBlock::heading(1, ""));
    let res = serialize_input_rich_message(
        &empty_page,
        SerializeRichMessageMode::FinalSubmit,
        &DirectIdMediaResolver,
    );
    assert_eq!(res.status, SerializeRichMessageStatus::EmptyContent);
    assert!(res.value.is_none());

    // Draft mode skips unuploaded media when resolver returns None.
    let mut draft_page = RichPage::default();
    draft_page.blocks.push(RichBlock::paragraph("Draft text"));
    let mut unuploaded = RichBlock::new(RichBlockKind::Photo);
    unuploaded.photo_id = 999;
    draft_page.blocks.push(unuploaded);
    let empty_resolver = InMemoryMediaResolver::default();
    let draft_res = serialize_input_rich_message(
        &draft_page,
        SerializeRichMessageMode::Draft,
        &empty_resolver,
    );
    assert_eq!(draft_res.status, SerializeRichMessageStatus::Success);
    assert_eq!(draft_res.value.unwrap().blocks.len(), 1);

    // Limits validation catches length, blocks, depth, media, table cols, buttons.
    let tight = RichMessageLimits {
        length_limit: 5,
        ..RichMessageLimits::default()
    };
    let mut p = RichPage::default();
    p.blocks.push(RichBlock::paragraph("123456"));
    assert_eq!(
        validate_rich_message(&p, &tight),
        Some(RichMessageLimitError::Length)
    );

    // Collage splitting at COLLAGE_MAX_ITEMS = 10
    let mut big_collage = RichBlock::new(RichBlockKind::GroupedMedia);
    big_collage.media_intent = GroupedMediaIntent::Collage;
    big_collage.caption = RichTextNode::plain("All 12");
    for id in 1..=12u64 {
        big_collage.media_items.push(GroupedMediaItem {
            kind: RichBlockKind::Photo,
            photo_id: id,
            ..GroupedMediaItem::default()
        });
    }
    let split = split_grouped_media_block(big_collage);
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].media_items.len(), 10);
    assert_eq!(split[1].media_items.len(), 2);
    assert!(split[0].caption.text.is_empty());
    assert_eq!(split[1].caption.text.text, "All 12");

    // Link URL encoding/decoding & inline text object encoding/decoding & RTL & model bridge
    let encoded_link = encode_rich_page_link_url("https://example.com/a?b=1&c=2", 42);
    let decoded_link = decode_rich_page_link_url(&encoded_link).expect("decode link");
    assert_eq!(decoded_link.url, "https://example.com/a?b=1&c=2");
    assert_eq!(decoded_link.webpage_id, 42);

    let obj = InlineTextObjectEntity::Formula(InlineTextObjectFormulaData {
        copy_source: "$a+b$".to_string(),
        trimmed_tex: "a+b".to_string(),
    });
    let enc_obj = serialize_inline_text_object_entity(&obj);
    assert_eq!(parse_inline_text_object_entity(&enc_obj), Some(obj));

    let mut expanded = TextWithEntities {
        text: format!("Value: {OBJECT_REPLACEMENT_CHAR}!"),
        entities: vec![EntityInText::with_data(
            EntityType::CustomEmoji,
            7,
            1,
            enc_obj,
        )],
    };
    expand_inline_text_objects(&mut expanded);
    assert_eq!(expanded.text, "Value: a+b!");

    // RTL detection
    let rtl_page = RichPage {
        blocks: vec![RichBlock::paragraph("مرحبا بالعالم")],
        ..RichPage::default()
    };
    assert!(determine_rich_page_rtl(&rtl_page));

    // RichMessage with textDiff in DisplayTextDiff mode
    let diff_msg = tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: vec![tl_enums::PageBlock::Paragraph(
            tl_types::PageBlockParagraph {
                text: tl_enums::RichText::TextDiff(tl_types::TextDiff {
                    text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                        text: "new".to_string(),
                    })),
                    old_text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                        text: "old".to_string(),
                    })),
                }),
            },
        )],
        photos: Vec::new(),
        documents: Vec::new(),
    };
    let normal_p = rich_page_from_tl_rich_message(&diff_msg, RichParseMode::Normal);
    assert_eq!(normal_p.blocks[0].text.text.text, "new");
    let diff_p = rich_page_from_tl_rich_message(&diff_msg, RichParseMode::DisplayTextDiff);
    assert_eq!(diff_p.blocks[0].text.text.text, "oldnew");
    assert!(
        diff_p.blocks[0]
            .text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::StrikeOut && e.offset == 0 && e.length == 3)
    );
    assert!(
        diff_p.blocks[0]
            .text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Underline && e.offset == 3 && e.length == 3)
    );

    // Bridge to/from `crate::model::Page` and flattening helpers
    let model_page = rich_page_to_model_page(&normal_p);
    let back = rich_page_from_model_page(&model_page);
    assert_eq!(back.blocks[0].text.text.text, "new");

    let split_page = split_text_into_rich_page(TextWithEntities {
        text: "Intro\nlet x = 1;\nOutro".to_string(),
        entities: vec![EntityInText::with_data(EntityType::Pre, 6, 10, "rust")],
    });
    assert_eq!(split_page.blocks.len(), 3);
    assert_eq!(split_page.blocks[1].kind, RichBlockKind::Code);
    assert_eq!(split_page.blocks[1].language, "rust");
    let summary = flatten_rich_page_summary(&split_page, true);
    assert!(summary.text.contains("Intro"));
    let simple = flatten_rich_page_to_simple_text(&split_page);
    assert!(simple.entities.iter().any(|e| e.kind == EntityType::Pre));
}

// ============================================================================
// 2. Known-Answer Tests: Markdown Validation, Math Extraction & Round-Trip
// ============================================================================

#[test]
fn markdown_validation_and_math_extraction_known_answers() {
    // Source validation error codes
    let utf16 = [0xFE, 0xFF, 0x00, b'A'];
    let v = validate_markdown_source_for_iv(&utf16, ParseOptions::default());
    assert!(!v.ok);
    assert_eq!(v.error, "source-unsupported-bom");

    let nul = b"hello\0world";
    assert!(looks_binary(nul));
    let v_bin = validate_markdown_source_for_iv(nul, ParseOptions::default());
    assert!(!v_bin.ok);
    assert_eq!(v_bin.error, "source-binary");

    let bad_utf8 = [0xFF, 0xFE, 0xFD]; // starts with UTF-16 LE BOM; test invalid utf8 without BOM
    let bad_utf8_no_bom = [0xC3, 0x28];
    let _ = bad_utf8;
    let v_utf8 = validate_markdown_source_for_iv(&bad_utf8_no_bom, ParseOptions::default());
    assert!(!v_utf8.ok);
    assert_eq!(v_utf8.error, "source-invalid-utf8");

    // UTF-8 BOM stripped cleanly
    let with_bom = b"\xEF\xBB\xBF# Hello";
    let v_ok = validate_markdown_source_for_iv(with_bom, ParseOptions::default());
    assert!(v_ok.ok);
    assert_eq!(v_ok.source.decoded, "# Hello");

    // Source size limit
    let small_opts = ParseOptions {
        source_name: "tiny.md".to_string(),
        limits: MarkdownParseLimits {
            max_source_bytes: 4,
            ..MarkdownParseLimits::IV_DEFAULT
        },
    };
    let v_big = validate_markdown_source_for_iv(b"12345", small_opts);
    assert!(!v_big.ok);
    assert_eq!(v_big.error, "source-too-large");

    // Math extraction heuristics (currency vs prose vs real math)
    assert!(looks_like_currency(b"100"));
    assert!(looks_like_currency(b"1,234.56"));
    assert!(!looks_like_currency(b"x+1"));
    assert!(has_strong_math_signal(b"\\alpha + 1"));
    assert!(has_strong_math_signal(b"x^2"));
    assert!(!has_strong_math_signal(b"hello world"));
    assert!(looks_like_prose(b"this is prose"));
    assert!(!looks_like_prose(b"x"));

    assert_eq!(
        strip_blockquote_markers(b"a + b\n> + c\n> > + d"),
        b"a + b\n+ c\n+ d"
    );

    let src = concat!(
        "Price is $100$.\n",
        "Costs $5 dollars and $10 total.\n",
        "Not $ordinary words here$ either.\n",
        "Escaped \\$x^2\\$ is literal.\n",
        "Real math $x^2 + 1$ and $$\\sum_{i=1}^n i$$ work."
    )
    .as_bytes();
    let line_starts = tg_iv::markdown::build_line_starts(src);
    let mask = vec![false; src.len()];
    let formulas = extract_math_regions(src, &mask, &line_starts, &[], 65536, 1000).unwrap();
    assert_eq!(formulas.len(), 2);
    assert_eq!(formulas[0].kind, MathKind::Inline);
    assert_eq!(formulas[0].tex, "x^2 + 1");
    assert_eq!(formulas[1].kind, MathKind::Display);
    assert_eq!(formulas[1].tex, "\\sum_{i=1}^n i");

    // Heading anchor deduplication & local fragment validation warnings
    let parsed = parse_markdown_for_iv(
        b"# Intro\n\n## Intro\n\nSee [ok](#intro-2) and [bad](#missing).\n\nFoot[^a].\n\n[^a]: Note A.\n",
        ParseOptions::default(),
    );
    assert!(parsed.ok);
    assert_eq!(
        anchor_id_base_from_text("Hello, World! 2026"),
        "hello-world-2026"
    );
    let headings: Vec<_> = parsed
        .document
        .document
        .children
        .iter()
        .filter(|n| n.kind == NodeKind::Heading)
        .collect();
    assert_eq!(headings[0].anchor_id, "intro");
    assert_eq!(headings[1].anchor_id, "intro-2");
    assert!(
        parsed
            .document
            .warnings
            .iter()
            .any(|w| w.contains("Unresolved local fragment \"#missing\""))
    );
    assert!(parsed.document.stats.footnotes_seen);
}

#[test]
fn markdown_roundtrip_known_answers() {
    let cases = [
        "# Main Heading\n\n## Sub Heading",
        "Plain **bold** *italic* ~~strike~~ `code` ||spoiler|| <u>under</u> <sub>2</sub> <sup>3</sup> <mark>hl</mark> [site](https://example.org) $E=mc^2$",
        "```rust\nlet x = 1 + 2;\nprintln!(\"{x}\");\n```",
        "> Blockquote line",
        "---",
        "- Alpha\n- Beta\n- Gamma",
        "- [ ] Todo open\n- [x] Todo done",
        "1. One\n2. Two\n3. Three",
        "$$\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n$$",
        "| Col A | Col B | Col C |\n| --- | :---: | ---: |\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |",
        "<details open>\n<summary>Expand me</summary>\n\nInside details.\n</details>",
    ];

    for (idx, input) in cases.iter().enumerate() {
        let page1 = markdown_to_rich_page(input)
            .unwrap_or_else(|e| panic!("case {idx} parse 1 failed: {e}"));
        let serialized1 = rich_page_to_markdown(&page1);
        let page2 = markdown_to_rich_page(&serialized1)
            .unwrap_or_else(|e| panic!("case {idx} parse 2 failed on:\n{serialized1}\nerr: {e}"));
        let serialized2 = rich_page_to_markdown(&page2);

        assert_eq!(
            serialized1, serialized2,
            "case {idx} failed canonical Markdown round-trip:\n--- input ---\n{input}\n--- pass 1 ---\n{serialized1}\n--- pass 2 ---\n{serialized2}"
        );
        assert_eq!(
            page1.blocks.len(),
            page2.blocks.len(),
            "case {idx} block count diverged"
        );
        for (b1, b2) in page1.blocks.iter().zip(&page2.blocks) {
            assert_eq!(b1.kind, b2.kind, "case {idx} block kind diverged");
            assert_eq!(b1.text.text, b2.text.text, "case {idx} text diverged");
            assert_eq!(b1.formula, b2.formula, "case {idx} formula diverged");
            assert_eq!(b1.language, b2.language, "case {idx} language diverged");
        }
    }
}

// ============================================================================
// 3. Known-Answer Tests: Pure-Rust Math Formula Layout Engine
// ============================================================================

#[test]
fn formula_layout_fractions_sub_sup_roots_matrices_big_ops() {
    // Environment rewriting matches tdesktop's exact 12-entry table
    let rewritten = rewrite_full_width_environments("\\begin{align*}a &= b\\end{align*}");
    assert_eq!(rewritten, "\\begin{aligned}a &= b\\end{aligned}");

    // 1. Fraction: `\frac{a+b}{c}` -> numerator above rule, denominator below rule, rule present
    let frac = layout_formula(&FormulaLayoutRequest::new(
        "\\frac{a+b}{c}",
        MathKind::Display,
        20,
    ));
    assert!(frac.measured.success, "frac error: {}", frac.measured.error);
    let fl = frac.layout.expect("frac layout");
    assert_eq!(fl.rules.len(), 1, "expected 1 fraction bar rule");
    let bar = fl.rules[0];
    let run_a = fl
        .glyph_runs
        .iter()
        .find(|r| r.text == "a")
        .expect("numerator 'a'");
    let run_c = fl
        .glyph_runs
        .iter()
        .find(|r| r.text == "c")
        .expect("denominator 'c'");
    assert!(
        run_a.y < bar.y,
        "numerator baseline ({}) must be above bar ({})",
        run_a.y,
        bar.y
    );
    assert!(
        run_c.y > bar.y + bar.height,
        "denominator baseline ({}) must be below bar ({})",
        run_c.y,
        bar.y
    );
    assert!(bar.width > 0.0 && bar.height >= 1.0);

    // 2. Subscripts & superscripts: `x_i^2` -> superscript above baseline, subscript below baseline, smaller font
    let sc = layout_formula(&FormulaLayoutRequest::new("x_i^2", MathKind::Inline, 20));
    assert!(sc.measured.success);
    let sl = sc.layout.expect("scripts layout");
    let run_x = sl
        .glyph_runs
        .iter()
        .find(|r| r.text == "x")
        .expect("base 'x'");
    let run_i = sl
        .glyph_runs
        .iter()
        .find(|r| r.text == "i")
        .expect("sub 'i'");
    let run_2 = sl
        .glyph_runs
        .iter()
        .find(|r| r.text == "2")
        .expect("sup '2'");
    assert!(
        run_2.y < run_x.y,
        "superscript y ({}) must be above base y ({})",
        run_2.y,
        run_x.y
    );
    assert!(
        run_i.y > run_x.y,
        "subscript y ({}) must be below base y ({})",
        run_i.y,
        run_x.y
    );
    assert!(run_2.font_size < run_x.font_size);
    assert!(run_i.font_size < run_x.font_size);
    assert!(run_2.x > run_x.x && run_i.x > run_x.x);

    // 3. Roots: `\sqrt[3]{x+1}` -> radical checkmark polyline + vinculum rule + index '3' to the left & above
    let root = layout_formula(&FormulaLayoutRequest::new(
        "\\sqrt[3]{x+1}",
        MathKind::Display,
        20,
    ));
    assert!(root.measured.success);
    let rl = root.layout.expect("root layout");
    assert_eq!(rl.polylines.len(), 1, "expected radical checkmark polyline");
    assert_eq!(rl.rules.len(), 1, "expected radical vinculum rule");
    let vinc = rl.rules[0];
    let run_idx = rl
        .glyph_runs
        .iter()
        .find(|r| r.text == "3")
        .expect("index '3'");
    let run_rad_x = rl
        .glyph_runs
        .iter()
        .find(|r| r.text == "x")
        .expect("radicand 'x'");
    assert!(
        run_idx.x < run_rad_x.x,
        "root index must sit left of radicand"
    );
    assert!(run_idx.font_size < run_rad_x.font_size);
    assert!(vinc.y < run_rad_x.y, "vinculum must sit above radicand");
    assert!(vinc.x + vinc.width >= run_rad_x.x + run_rad_x.width);

    // 4. Matrices: `\begin{pmatrix} a & b \\ c & d \end{pmatrix}` -> 2x2 grid alignment + delimiters
    let mat = layout_formula(&FormulaLayoutRequest::new(
        "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
        MathKind::Display,
        20,
    ));
    assert!(mat.measured.success);
    let ml = mat.layout.expect("matrix layout");
    let ma = ml.glyph_runs.iter().find(|r| r.text == "a").expect("a");
    let mb = ml.glyph_runs.iter().find(|r| r.text == "b").expect("b");
    let mc = ml.glyph_runs.iter().find(|r| r.text == "c").expect("c");
    let md = ml.glyph_runs.iter().find(|r| r.text == "d").expect("d");
    assert!((ma.y - mb.y).abs() < 1e-3, "row 0 entries share baseline");
    assert!((mc.y - md.y).abs() < 1e-3, "row 1 entries share baseline");
    assert!(mc.y > ma.y, "row 1 is below row 0");
    assert!(mb.x > ma.x && md.x > mc.x, "col 1 is right of col 0");
    assert!(ml.glyph_runs.iter().any(|r| r.text == "("));
    assert!(ml.glyph_runs.iter().any(|r| r.text == ")"));

    // Bracket matrix uses vertical & cap rules
    let bmat = layout_formula(&FormulaLayoutRequest::new(
        "\\begin{bmatrix} 1 & 0 \\\\ 0 & 1 \\end{bmatrix}",
        MathKind::Display,
        20,
    ));
    assert!(bmat.measured.success);
    assert!(bmat.layout.unwrap().rules.len() >= 6);

    // 5. Big operators: `\sum_{i=1}^{n} i` in Display vs Inline mode
    let sum_disp = layout_formula(&FormulaLayoutRequest::new(
        "\\sum_{i=1}^{n} i",
        MathKind::Display,
        20,
    ));
    assert!(sum_disp.measured.success);
    let sdl = sum_disp.layout.expect("display sum layout");
    let op = sdl
        .glyph_runs
        .iter()
        .find(|r| r.text == "∑" && r.style == MathFontStyle::Symbol)
        .expect("sigma symbol");
    let upper_n = sdl
        .glyph_runs
        .iter()
        .find(|r| r.text == "n")
        .expect("upper limit n");
    let lower_1 = sdl
        .glyph_runs
        .iter()
        .find(|r| r.text == "1")
        .expect("lower limit 1");
    assert!(upper_n.y < op.y, "display sum upper limit must be above ∑");
    assert!(lower_1.y > op.y, "display sum lower limit must be below ∑");
    // In display mode, limits are horizontally centered on ∑ (not to the right of ∑)
    assert!(upper_n.x < op.x + op.width);

    let sum_inline = layout_formula(&FormulaLayoutRequest::new(
        "\\sum_{i=1}^{n} i",
        MathKind::Inline,
        20,
    ));
    assert!(sum_inline.measured.success);
    let sil = sum_inline.layout.expect("inline sum layout");
    let op_in = sil
        .glyph_runs
        .iter()
        .find(|r| r.text == "∑")
        .expect("inline ∑");
    let n_in = sil
        .glyph_runs
        .iter()
        .find(|r| r.text == "n")
        .expect("inline n");
    assert!(
        n_in.x >= op_in.x + op_in.width * 0.9,
        "inline sum places limits to the right"
    );
    assert!(
        sdl.height > sil.height,
        "display sum should be taller than inline sum"
    );

    // 6. Integrals, Greek symbols, blackboard bold, LRU cache & error handling
    let mut engine = MathLayoutEngine::with_cache_budget(4096);
    let int_req = FormulaLayoutRequest::new(
        "\\int_0^{\\infty} e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}",
        MathKind::Display,
        18,
    );
    let r1 = engine.render(&int_req);
    assert!(r1.measured.success);
    let l1 = r1.layout.as_ref().unwrap();
    assert!(l1.glyph_runs.iter().any(|r| r.text == "∫"));
    assert!(l1.glyph_runs.iter().any(|r| r.text == "∞"));
    assert!(l1.glyph_runs.iter().any(|r| r.text == "π"));

    // Second render hits LRU cache
    let _r2 = engine.render(&int_req);
    assert_eq!(engine.debug_counters().misses, 1);
    assert_eq!(engine.debug_counters().hits, 1);

    // Invalid / oversized requests fail cleanly without panicking
    let empty_err = layout_formula(&FormulaLayoutRequest::new("   ", MathKind::Inline, 16));
    assert!(!empty_err.measured.success);
    assert_eq!(empty_err.measured.error, "empty-tex");

    let cap_err = layout_formula(&FormulaLayoutRequest {
        trimmed_tex: "\\frac{a+b+c+d+e+f}{g+h+i+j+k+l}".to_string(),
        kind: MathKind::Display,
        text_size: 20,
        render_width_cap: 10,
        render_height_cap: 10,
    });
    assert!(!cap_err.measured.success);
    assert!(cap_err.measured.too_large);
    assert_eq!(cap_err.measured.error, "logical-size-cap-exceeded");
}

// ============================================================================
// 4. Unit Tests: Headless Editor Document Model
// ============================================================================

#[test]
fn editor_text_formatting_cursor_and_undo_redo() {
    let mut ed = EditorState::new();
    assert_eq!(ed.text_node_count(), 1);
    assert!(!ed.can_undo());
    assert!(!ed.can_redo());

    // Type text
    assert_eq!(ed.insert_text("Hello world"), ApplyResult::Changed);
    assert_eq!(ed.active_raw_text(), "Hello world");
    assert_eq!(ed.cursor().head, 11);
    assert!(ed.can_undo());

    // Select "world" (6..11) and toggle Bold, then Italic, then toggle Bold off
    assert!(ed.set_selection(0, 6, 11));
    assert_eq!(
        ed.toggle_formatting(TextFormattingAction::Bold),
        ApplyResult::Changed
    );
    assert!(ed.toolbar_state().bold.active);
    assert_eq!(
        ed.toggle_formatting(TextFormattingAction::Italic),
        ApplyResult::Changed
    );
    assert!(ed.toolbar_state().italic.active);
    assert_eq!(ed.to_markdown(), "Hello ***world***");

    assert_eq!(
        ed.toggle_formatting(TextFormattingAction::Bold),
        ApplyResult::Changed
    );
    assert!(!ed.toolbar_state().bold.active);
    assert_eq!(ed.to_markdown(), "Hello *world*");

    // Apply link, then PlainText strips all formatting
    assert_eq!(
        ed.apply_formatting(
            TextFormattingAction::Link("https://example.org".to_string()),
            Some(true),
        ),
        ApplyResult::Changed
    );
    assert!(ed.toolbar_state().link.active);
    assert_eq!(
        ed.toggle_formatting(TextFormattingAction::PlainText),
        ApplyResult::Changed
    );
    assert!(ed.active_text().entities.is_empty());

    // Undo restores link, then italic, then bold+italic...
    assert!(ed.undo());
    assert!(ed.toolbar_state().link.active);
    assert!(ed.redo());
    assert!(ed.active_text().entities.is_empty());

    // Grouped transaction collapses multiple edits into one undo step
    let depth_before = ed.undo_depth();
    ed.begin_transaction();
    ed.set_cursor(0, 11);
    assert_eq!(ed.insert_text("!"), ApplyResult::Changed);
    assert_eq!(ed.insert_text("?"), ApplyResult::Changed);
    ed.commit_transaction();
    assert_eq!(ed.active_raw_text(), "Hello world!?");
    assert_eq!(ed.undo_depth(), depth_before + 1);
    assert!(ed.undo());
    assert_eq!(ed.active_raw_text(), "Hello world");

    // Auto-pair insertion, skip-over, wrap selection, and backspace pair deletion
    ed.set_cursor(0, 11);
    assert_eq!(ed.insert_text(" "), ApplyResult::Changed);
    assert_eq!(ed.handle_auto_pair_char('('), ApplyResult::Changed);
    assert_eq!(ed.active_raw_text(), "Hello world ()");
    assert_eq!(ed.cursor().head, 13);
    assert_eq!(ed.handle_auto_pair_backspace(), ApplyResult::Changed);
    assert_eq!(ed.active_raw_text(), "Hello world ");

    // Inline math insertion
    assert_eq!(ed.insert_inline_math("x^2"), ApplyResult::Changed);
    assert!(ed.to_markdown().contains("$x^2$"));
}

#[test]
fn editor_blocks_lists_tables_and_limit_rollback() {
    let mut ed = EditorState::new();
    ed.insert_text("First line");
    assert_eq!(
        ed.convert_active_block(InsertAction::heading(2)),
        ApplyResult::Changed
    );
    assert_eq!(ed.rich_page().blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(ed.rich_page().blocks[0].heading_level, 2);

    // Enter splits heading -> new paragraph below
    ed.set_cursor(0, 5);
    assert_eq!(ed.handle_enter(), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks.len(), 2);
    assert_eq!(ed.rich_page().blocks[0].text.text.text, "First");
    assert_eq!(ed.rich_page().blocks[1].text.text.text, " line");

    // Backspace at offset 0 of paragraph joins back into heading
    ed.set_cursor(1, 0);
    assert_eq!(ed.delete_backward(), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks.len(), 1);
    assert_eq!(ed.rich_page().blocks[0].text.text.text, "First line");

    // Cursor navigation across leaves
    assert_eq!(
        ed.insert_block_after_active(InsertAction::new(InsertBlockType::TaskList)),
        ApplyResult::Changed
    );
    assert_eq!(ed.insert_text("Item A"), ApplyResult::Changed);
    assert_eq!(ed.handle_enter(), ApplyResult::Changed);
    assert_eq!(ed.insert_text("Item B"), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks[1].list_items.len(), 2);
    assert_eq!(
        ed.rich_page().blocks[1].list_items[0].task_state,
        RichTaskState::Unchecked
    );

    // Toggle task checkbox & sink/lift list item & switch to ordered list
    let list_path = BlockPath::root(1);
    assert_eq!(ed.toggle_task_state(&list_path, 0), ApplyResult::Changed);
    assert_eq!(
        ed.rich_page().blocks[1].list_items[0].task_state,
        RichTaskState::Checked
    );

    assert_eq!(ed.sink_active_list_item(), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks[1].list_items.len(), 1);
    assert_eq!(ed.rich_page().blocks[1].list_items[0].blocks.len(), 1);
    assert_eq!(ed.lift_active_list_item(), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks[1].list_items.len(), 2);

    assert_eq!(
        ed.set_list_style(&list_path, ListStyle::Ordered),
        ApplyResult::Changed
    );
    assert_eq!(
        ed.set_list_ordered_type(&list_path, OrderedListType::UpperRoman),
        ApplyResult::Changed
    );
    assert_eq!(
        ed.rich_page().blocks[1].ordered_list.list_type.as_deref(),
        Some("I")
    );

    // Insert code block (Raw field mode) & display math block
    assert_eq!(
        ed.insert_block_after_active(InsertAction::code("rust")),
        ApplyResult::Changed
    );
    assert_eq!(ed.active_field_mode(), FieldMode::Raw);
    assert_eq!(ed.insert_text("let a = 1;"), ApplyResult::Changed);
    assert_eq!(ed.handle_enter(), ApplyResult::Changed);
    assert_eq!(ed.insert_text("let b = 2;"), ApplyResult::Changed);
    assert_eq!(ed.active_raw_text(), "let a = 1;\nlet b = 2;");

    // Insert 3x3 table, add row & col, merge cells, split cells, erase row keeping spans
    assert_eq!(
        ed.insert_block_after_active(InsertAction::new(InsertBlockType::Table)),
        ApplyResult::Changed
    );
    let table_path = BlockPath::root(ed.rich_page().blocks.len() - 1);
    let grid0 = build_table_grid(&ed.rich_page().blocks[table_path.index]);
    assert_eq!(grid0.row_count, 3);
    assert_eq!(grid0.column_count, 3);

    let full_cell = EditTableCellRange {
        block: table_path.clone(),
        row_from: 1,
        row_till: 3,
        column_from: 0,
        column_till: 2,
    };
    let info = ed.table_selection_info(&full_cell);
    assert!(info.valid && info.can_unite_cells);
    assert_eq!(ed.unite_table_cells(&full_cell), ApplyResult::Changed);
    let merged_cell = &ed.rich_page().blocks[table_path.index].table_rows[1].cells[0];
    assert_eq!(merged_cell.rowspan, 2);
    assert_eq!(merged_cell.colspan, 2);

    // Erasing row 1 keeps the merged cell relocated into row 2 with rowspan decremented to 1!
    let row1 = EditTableCellRange {
        block: table_path.clone(),
        row_from: 1,
        row_till: 2,
        column_from: 0,
        column_till: 3,
    };
    assert_eq!(ed.remove_table_rows(&row1), ApplyResult::Changed);
    assert_eq!(ed.rich_page().blocks[table_path.index].table_rows.len(), 2);
    assert_eq!(
        ed.rich_page().blocks[table_path.index].table_rows[1].cells[0].rowspan,
        1
    );
    assert_eq!(
        ed.rich_page().blocks[table_path.index].table_rows[1].cells[0].colspan,
        2
    );

    // Split merged cell back to 1x1
    let merged_sel = EditTableCellRange {
        block: table_path.clone(),
        row_from: 1,
        row_till: 2,
        column_from: 0,
        column_till: 2,
    };
    assert_eq!(ed.split_table_cell(&merged_sel), ApplyResult::Changed);
    assert_eq!(
        ed.rich_page().blocks[table_path.index].table_rows[1]
            .cells
            .len(),
        3
    );

    // Add row & column & structural selection removal
    assert_eq!(ed.add_table_row(&merged_sel, true), ApplyResult::Changed);
    assert_eq!(ed.add_table_column(&merged_sel, true), ApplyResult::Changed);
    let grid_after = build_table_grid(&ed.rich_page().blocks[table_path.index]);
    assert_eq!(grid_after.row_count, 3);
    assert_eq!(grid_after.column_count, 4);

    // Serialize directly to TL blocks from EditorState
    let tl_blocks = ed
        .to_tl_blocks(
            SerializeRichMessageMode::FinalSubmit,
            &DirectIdMediaResolver,
        )
        .expect("editor serializes to TL blocks");
    assert!(!tl_blocks.is_empty());

    // Limit violation rolls back cleanly and records last_limit_error
    let mut limited = EditorState::from_rich_page(
        RichPage::default(),
        RichMessageLimits {
            length_limit: 5,
            ..RichMessageLimits::default()
        },
    );
    assert_eq!(limited.insert_text("12345"), ApplyResult::Changed);
    assert_eq!(limited.insert_text("6"), ApplyResult::Failed);
    assert_eq!(
        limited.last_limit_error(),
        Some(RichMessageLimitError::Length)
    );
    assert_eq!(limited.active_raw_text(), "12345");

    // Structural selection deletion
    let mut sel_ed =
        EditorState::from_markdown("A\n\nB\n\nC", RichMessageLimits::default()).unwrap();
    sel_ed.set_structural_selection(EditSelection::Blocks(tg_iv::editor::EditBlockRange {
        container: tg_iv::editor::BlockContainerPath::root(),
        from: 1,
        till: 2,
    }));
    assert_eq!(sel_ed.delete_backward(), ApplyResult::Changed);
    assert_eq!(sel_ed.rich_page().blocks.len(), 2);
    assert_eq!(sel_ed.rich_page().blocks[0].text.text.text, "A");
    assert_eq!(sel_ed.rich_page().blocks[1].text.text.text, "C");
}

#[test]
fn external_input_safety_multibyte_utf8_and_deep_nesting() {
    // Multi-byte UTF-8 inputs (Cyrillic, CJK, Arabic, emoji) must never panic in parser/details/links.
    let utf8_inputs = [
        "Привет мир 😀\n\n<details open>\n<summary>Детали 🎉</summary>\n\nТекст **жирный** $x+1$.\n</details>",
        "<details>Привет без закрытия",
        "日本語テキスト [broken link](https://example.org/\\",
        "[emoji](https://example.org/\\😀)",
        "[title](https://example.org \"unclosed\\",
        "**мир**  \nследующая строка",
    ];
    for input in utf8_inputs {
        let parsed = parse_markdown_for_iv(input.as_bytes(), ParseOptions::default());
        assert!(parsed.ok, "failed on valid utf-8 input: {input}");
        let page = markdown_to_rich_page(input).expect("valid utf-8 parses to rich page");
        let _ = rich_page_to_markdown(&page);
        let _ = rich_page_to_tl_blocks(
            &page,
            SerializeRichMessageMode::FinalSubmit,
            &DirectIdMediaResolver,
        );
    }

    // Deeply nested TeX braces and commands must not stack-overflow.
    let deep_braces = format!("{}{}", "{".repeat(500), "x".to_string() + &"}".repeat(500));
    let _ = layout_formula(&FormulaLayoutRequest::new(
        deep_braces,
        MathKind::Inline,
        16,
    ));

    let deep_fracs = format!("{}{}", "\\frac".repeat(500), "1");
    let _ = layout_formula(&FormulaLayoutRequest::new(
        deep_fracs,
        MathKind::Display,
        16,
    ));
}

#[test]
fn formula_layout_extended_constructs_known_answers() {
    // Nested fractions shrink child style progressively and produce 2 fraction bars
    let nested = layout_formula(&FormulaLayoutRequest::new(
        "\\frac{1}{1 + \\frac{1}{x}}",
        MathKind::Display,
        20,
    ));
    assert!(nested.measured.success);
    let nl = nested.layout.expect("nested frac layout");
    assert_eq!(nl.rules.len(), 2, "expected outer + inner fraction bars");

    // \binom has no rule bar and wraps in scaled parentheses
    let binom = layout_formula(&FormulaLayoutRequest::new(
        "\\binom{n}{k}",
        MathKind::Display,
        20,
    ));
    assert!(binom.measured.success);
    let bl = binom.layout.expect("binom layout");
    assert!(
        bl.rules.is_empty(),
        "\\binom should not draw a fraction bar"
    );
    assert!(bl.glyph_runs.iter().any(|r| r.text == "("));
    assert!(bl.glyph_runs.iter().any(|r| r.text == ")"));

    // Overline, underline, accent (\hat, \vec), primes
    let decorated = layout_formula(&FormulaLayoutRequest::new(
        "\\overline{z} + \\underline{w} + \\hat{x} + \\vec{v} + f''(t)",
        MathKind::Display,
        20,
    ));
    assert!(decorated.measured.success);
    let dl = decorated.layout.expect("decorated layout");
    assert_eq!(dl.rules.len(), 2, "overline + underline rules");
    assert!(dl.glyph_runs.iter().any(|r| r.text == "^"));
    assert!(dl.glyph_runs.iter().any(|r| r.text == "→"));
    assert!(dl.glyph_runs.iter().any(|r| r.text == "′′"));

    // Cases environment, vmatrix, \lim, and explicit \int\limits
    let cases = layout_formula(&FormulaLayoutRequest::new(
        "\\begin{cases} x & x \\ge 0 \\\\ -x & x < 0 \\end{cases}",
        MathKind::Display,
        20,
    ));
    assert!(cases.measured.success);
    let cl = cases.layout.expect("cases layout");
    assert!(cl.glyph_runs.iter().any(|r| r.text == "{"));
    assert!(cl.glyph_runs.iter().any(|r| r.text == "≥"));

    let det = layout_formula(&FormulaLayoutRequest::new(
        "\\begin{vmatrix} 1 & 2 \\\\ 3 & 4 \\end{vmatrix}",
        MathKind::Display,
        20,
    ));
    assert!(det.measured.success);
    assert_eq!(det.layout.expect("vmatrix").rules.len(), 2);

    let lim = layout_formula(&FormulaLayoutRequest::new(
        "\\lim_{x \\to 0} \\frac{\\sin x}{x} = 1",
        MathKind::Display,
        20,
    ));
    assert!(lim.measured.success);
    let ll = lim.layout.expect("lim layout");
    let lim_run = ll.glyph_runs.iter().find(|r| r.text == "lim").expect("lim");
    let arrow_run = ll.glyph_runs.iter().find(|r| r.text == "→").expect("arrow");
    assert!(
        arrow_run.y > lim_run.y,
        "limit subscript sits below 'lim' in display mode"
    );

    let int_limits = layout_formula(&FormulaLayoutRequest::new(
        "\\int\\limits_a^b f(x)\\,dx",
        MathKind::Display,
        20,
    ));
    assert!(int_limits.measured.success);
    let il = int_limits.layout.expect("int limits layout");
    let int_run = il.glyph_runs.iter().find(|r| r.text == "∫").expect("∫");
    let upper_b = il.glyph_runs.iter().find(|r| r.text == "b").expect("b");
    let lower_a = il.glyph_runs.iter().find(|r| r.text == "a").expect("a");
    assert!(upper_b.y < int_run.y && lower_a.y > int_run.y);
}

#[test]
fn editor_autopair_middle_selection_skipover_and_navigation() {
    let mut ed = EditorState::new();
    assert_eq!(ed.insert_text("hello world"), ApplyResult::Changed);

    // Wrap a selection in the MIDDLE of the string ("hello") with '(' -> "(hello) world"
    assert!(ed.set_selection(0, 0, 5));
    assert_eq!(ed.handle_auto_pair_char('('), ApplyResult::Changed);
    assert_eq!(ed.active_raw_text(), "(hello) world");
    assert_eq!(ed.cursor().selection_range(), (1, 6));

    // Skip over existing ')' without changing text or pushing a spurious undo entry
    assert!(ed.set_cursor(0, 6));
    let undo_before = ed.undo_depth();
    assert_eq!(ed.handle_auto_pair_char(')'), ApplyResult::Unchanged);
    assert_eq!(ed.active_raw_text(), "(hello) world");
    assert_eq!(ed.cursor().head, 7);
    assert_eq!(ed.undo_depth(), undo_before);

    // Multi-leaf cursor movement, forward delete join, leaf_text(), and transaction rollback
    assert!(ed.set_cursor(0, 7));
    assert_eq!(ed.handle_enter(), ApplyResult::Changed);
    assert_eq!(ed.text_node_count(), 2);
    let leaf0 = ed.text_nodes()[0].leaf.clone();
    let leaf1 = ed.text_nodes()[1].leaf.clone();
    assert_eq!(ed.leaf_text(&leaf0).text, "(hello)");
    assert_eq!(ed.leaf_text(&leaf1).text, " world");

    // Moving left from leaf 1 offset 0 crosses to leaf 0 end
    assert_eq!(ed.active_text_ordinal(), 1);
    assert_eq!(ed.cursor().head, 0);
    assert!(ed.move_cursor_left(false));
    assert_eq!(ed.active_text_ordinal(), 0);
    assert_eq!(ed.cursor().head, 7);

    // Moving right from leaf 0 end crosses back to leaf 1 offset 0
    assert!(ed.move_cursor_right(false));
    assert_eq!(ed.active_text_ordinal(), 1);
    assert_eq!(ed.cursor().head, 0);

    // Forward delete from end of leaf 0 joins leaf 1 back into leaf 0
    assert!(ed.set_cursor(0, 7));
    assert_eq!(ed.delete_forward(), ApplyResult::Changed);
    assert_eq!(ed.text_node_count(), 1);
    assert_eq!(ed.active_raw_text(), "(hello) world");

    // Rollback transaction discards intermediate mutations cleanly
    ed.begin_transaction();
    assert_eq!(ed.insert_text(" TEMP"), ApplyResult::Changed);
    assert!(ed.active_raw_text().contains("TEMP"));
    ed.rollback_transaction();
    assert_eq!(ed.active_raw_text(), "(hello) world");

    // Model page preview conversion works cleanly
    let preview_page = ed.to_model_page();
    assert_eq!(preview_page.blocks.len(), 1);
}
