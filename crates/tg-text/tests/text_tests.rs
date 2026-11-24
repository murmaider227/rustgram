//! Comprehensive test suite for `tg-text`.
//!
//! Covers UTF-16 <-> UTF-8 conversion, link/domain/TLD/IDN/email/mention/hashtag/cashtag/
//! bot-command parsing, nested markdown and `[text](url)` links, cleaning/trimming/splitting,
//! tag <-> entity conversion and serialization, text utilities, message field helpers,
//! and the compiled emoji database + autocomplete suggestions.

use tg_text::char_class::*;
use tg_text::emoji::{self, Section};
use tg_text::entity::*;
use tg_text::markdown::*;
use tg_text::message_field::*;
use tg_text::parse::*;
use tg_text::tags::*;
use tg_text::url::*;
use tg_text::utf16::*;
use tg_text::utilities::*;

// ============================================================================
// 1. UTF-16 <-> UTF-8 Offset Conversion Utilities
// ============================================================================

#[test]
fn utf16_ascii_and_multibyte_conversions() {
    // ASCII (1 UTF-8 byte, 1 UTF-16 unit)
    // 'é' (2 UTF-8 bytes, 1 UTF-16 unit)
    // '中' (3 UTF-8 bytes, 1 UTF-16 unit)
    // '😀' (4 UTF-8 bytes, 2 UTF-16 units)
    let s = "aé中😀z";
    assert_eq!(utf16_len(s), 6); // 1 + 1 + 1 + 2 + 1

    assert_eq!(utf16_to_utf8_offset(s, 0), Some(0));
    assert_eq!(utf16_to_utf8_offset(s, 1), Some(1)); // 'é'
    assert_eq!(utf16_to_utf8_offset(s, 2), Some(3)); // '中'
    assert_eq!(utf16_to_utf8_offset(s, 3), Some(6)); // '😀'
    assert_eq!(utf16_to_utf8_offset(s, 4), None); // mid-surrogate!
    assert_eq!(utf16_to_utf8_offset(s, 5), Some(10)); // 'z'
    assert_eq!(utf16_to_utf8_offset(s, 6), Some(11)); // end
    assert_eq!(utf16_to_utf8_offset(s, 7), None); // out of bounds

    // Floor / ceil on mid-surrogate
    assert_eq!(utf16_to_utf8_floor(s, 4), 6);
    assert_eq!(utf16_to_utf8_ceil(s, 4), 10);
    assert_eq!(utf16_to_utf8_floor(s, 99), s.len());
    assert_eq!(utf16_to_utf8_ceil(s, 99), s.len());

    // Reverse direction
    assert_eq!(utf8_to_utf16_offset(s, 0), Some(0));
    assert_eq!(utf8_to_utf16_offset(s, 1), Some(1));
    assert_eq!(utf8_to_utf16_offset(s, 2), None); // mid-UTF8 of 'é'
    assert_eq!(utf8_to_utf16_offset(s, 6), Some(3));
    assert_eq!(utf8_to_utf16_offset(s, 10), Some(5));
    assert_eq!(utf8_to_utf16_offset(s, 11), Some(6));

    // Range & slicing
    assert_eq!(slice_utf16(s, 1, 4), Some("é中😀"));
    assert_eq!(slice_utf16(s, 3, 2), Some("😀"));
    assert_eq!(slice_utf16(s, 3, 1), None); // splits surrogate
    assert_eq!(utf8_range_to_utf16(s, 6..10), Some((3, 2)));
    assert_eq!(utf16_range_to_utf8(s, 3, 2), Some(6..10));
}

#[test]
fn utf16_offset_mapper_with_zwj_family_emoji() {
    // Family emoji: 👨(2) + ZWJ(1) + 👩(2) + ZWJ(1) + 👧(2) + ZWJ(1) + 👦(2) = 11 UTF-16 units
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let s = format!("Hi {family}!");
    let mapper = OffsetMapper::new(&s);
    assert_eq!(mapper.utf16_len(), 3 + 11 + 1);
    assert_eq!(mapper.utf16_to_utf8(3), Some(3));
    assert_eq!(mapper.utf16_to_utf8(4), None); // inside 👨 surrogate
    assert_eq!(mapper.utf16_to_utf8(14), Some(3 + family.len()));
    assert_eq!(mapper.utf16_to_utf8_floor(4), 3);
    assert_eq!(mapper.utf16_to_utf8_ceil(4), 7);
    assert_eq!(mapper.utf16_range_to_utf8(3, 11), Some(3..3 + family.len()));
    assert_eq!(
        mapper.utf8_range_to_utf16(3..3 + family.len()),
        Some((3, 11))
    );
}

// ============================================================================
// 2. Link Detection, Punctuation, IDN Domains, Emails, Mentions, Hashtags, Cashtags, Commands
// ============================================================================

#[test]
fn link_detection_with_punctuation_and_brackets() {
    let flags = TextParseFlags::LINKS;

    // Trailing punctuation stripped
    let parsed = parse_entities("Check https://example.com/path.", flags);
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(parsed.entities[0].entity_type(), EntityType::Url);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("https://example.com/path")
    );

    // Multiple trailing punctuation
    let parsed = parse_entities("Really?! https://example.com/q?a=1&b=2?!", flags);
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("https://example.com/q?a=1&b=2")
    );

    // Balanced parentheses inside Wikipedia URL + outer enclosing parens
    let parsed = parse_entities(
        "(see https://en.wikipedia.org/wiki/Rust_(programming_language))",
        flags,
    );
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
    );

    // Bare valid TLD domain vs invalid file extension
    let parsed = parse_entities("Visit telegram.org, not readme.txt or test.xyz!", flags);
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("telegram.org")
    );

    // Explicit valid protocol on localhost / port
    let parsed = parse_entities(
        "Local http://localhost:8080/api and tg://resolve?domain=durov",
        flags,
    );
    assert_eq!(parsed.entities.len(), 2);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("http://localhost:8080/api")
    );
    assert_eq!(
        parsed.slice_entity(&parsed.entities[1]),
        Some("tg://resolve?domain=durov")
    );

    // Invalid protocol ignored
    let parsed = parse_entities("javascript://alert(1) bad://example.com", flags);
    assert!(parsed.entities.is_empty());

    // Non-BMP emoji ends URL (matching Qt surrogate IsLinkEnd rule)
    let parsed = parse_entities("Go https://example.com/ok😀now", flags);
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("https://example.com/ok")
    );
}

#[test]
fn idn_domains_and_homograph_detection() {
    let flags = TextParseFlags::LINKS;

    // Cyrillic .рф domain and punycode TLD
    let parsed = parse_entities("Зайди на пример.рф и https://тест.xn--p1ai/ok", flags);
    assert_eq!(parsed.entities.len(), 2);
    assert_eq!(parsed.slice_entity(&parsed.entities[0]), Some("пример.рф"));
    assert_eq!(
        parsed.slice_entity(&parsed.entities[1]),
        Some("https://тест.xn--p1ai/ok")
    );
    assert!(is_valid_top_domain("рф"));
    assert!(is_valid_top_domain("xn--p1ai"));

    // Homograph attack: Cyrillic 'а' (U+0430) inside Latin .com domain
    let spoofy = "https://p\u{0430}ypal.com/login";
    assert!(is_suspicious(spoofy));
    let ranges = suspicious_ranges(spoofy);
    assert_eq!(ranges, vec![SuspiciousRange { from: 9, length: 1 }]);

    // Clean ASCII and clean Cyrillic .рф are NOT suspicious
    assert!(!is_suspicious("https://paypal.com/login"));
    assert!(!is_suspicious("https://пример.рф/страница"));
    // Plain ASCII subdomain under Cyrillic TLD is allowed
    assert!(!is_suspicious("https://www.пример.рф"));
}

#[test]
fn email_detection_and_mention_disambiguation() {
    let flags = TextParseFlags::LINKS | TextParseFlags::MENTIONS;

    let parsed = parse_entities(
        "Contact support@telegram.org or john.doe-1_2@sub.corp.local.",
        flags,
    );
    assert_eq!(parsed.entities.len(), 2);
    assert_eq!(parsed.entities[0].entity_type(), EntityType::Email);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("support@telegram.org")
    );
    assert_eq!(parsed.entities[1].entity_type(), EntityType::Email);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[1]),
        Some("john.doe-1_2@sub.corp.local")
    );

    // Email with custom TLD still detected as Email (not broken into mention)
    let parsed = parse_entities("Mail me at alice@internal.custom!", flags);
    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(parsed.entities[0].entity_type(), EntityType::Email);
    assert_eq!(
        parsed.slice_entity(&parsed.entities[0]),
        Some("alice@internal.custom")
    );
}

#[test]
fn mentions_hashtags_cashtags_bot_commands_and_word_boundaries() {
    let flags = TextParseFlags::MENTIONS
        | TextParseFlags::HASHTAGS
        | TextParseFlags::BOT_COMMANDS
        | TextParseFlags::CASHTAGS;

    // Mentions inside words must NOT match; valid mentions must start with letter & end with alnum
    let parsed = parse_entities("foo@bar @123 @bad_ @good_1 (@alice) hello", flags);
    let mentions: Vec<&str> = parsed
        .entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::Mention)
        .filter_map(|e| parsed.slice_entity(e))
        .collect();
    assert_eq!(mentions, vec!["@good_1", "@alice"]);

    // Hashtags: digits-only excluded, mid-word excluded, hashtag@channel allowed, Unicode allowed
    let parsed = parse_entities("#12345 foo#bar #rust #news@channel #привет", flags);
    let hashtags: Vec<&str> = parsed
        .entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::Hashtag)
        .filter_map(|e| parsed.slice_entity(e))
        .collect();
    assert_eq!(hashtags, vec!["#rust", "#news@channel", "#привет"]);

    // Cashtags
    let parsed = parse_entities("Buy $USD and $AAPL@market, not $usd or foo$BTC", flags);
    let cashtags: Vec<&str> = parsed
        .entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::Cashtag)
        .filter_map(|e| parsed.slice_entity(e))
        .collect();
    assert_eq!(cashtags, vec!["$USD", "$AAPL@market"]);

    // Bot commands
    let parsed = parse_entities("/start /help@my_test_bot path/to/file", flags);
    let cmds: Vec<&str> = parsed
        .entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::BotCommand)
        .filter_map(|e| parsed.slice_entity(e))
        .collect();
    assert_eq!(cmds, vec!["/start", "/help@my_test_bot"]);
}

#[test]
fn utf16_entity_offsets_with_emoji_and_non_bmp() {
    let flags = TextParseFlags::LINKS | TextParseFlags::MENTIONS | TextParseFlags::HASHTAGS;
    // "🔥 " = 2 + 1 = 3 UTF-16 units
    // "@durov" = offset 3, len 6 (end 9)
    // " 🚀 " = 1 + 2 + 1 = 4 UTF-16 units (end 13)
    // "#rust" = offset 13, len 5 (end 18)
    // " " = 1 (end 19)
    // "https://t.me/s" = offset 19, len 14
    let text = "🔥 @durov 🚀 #rust https://t.me/s";
    let parsed = parse_entities(text, flags);
    assert_eq!(parsed.entities.len(), 3);

    assert_eq!(
        parsed.entities[0],
        EntityInText::new(EntityType::Mention, 3, 6)
    );
    assert_eq!(
        parsed.entities[1],
        EntityInText::new(EntityType::Hashtag, 13, 5)
    );
    assert_eq!(
        parsed.entities[2],
        EntityInText::new(EntityType::Url, 19, 14)
    );

    assert_eq!(parsed.slice_entity(&parsed.entities[0]), Some("@durov"));
    assert_eq!(parsed.slice_entity(&parsed.entities[1]), Some("#rust"));
    assert_eq!(
        parsed.slice_entity(&parsed.entities[2]),
        Some("https://t.me/s")
    );
}

// ============================================================================
// 3. Markdown Parsing, Nested Markdown, Links, Pre Blocks, PrepareForSending
// ============================================================================

#[test]
fn markdown_all_inline_and_block_syntax() {
    let input = "**bold** __italic__ ~~strike~~ `code` ||spoiler|| [tg](https://t.me)";
    let parsed = parse_markdown(input);
    assert_eq!(parsed.text, "bold italic strike code spoiler tg");
    assert_eq!(
        parsed.entities,
        vec![
            EntityInText::new(EntityType::Bold, 0, 4),
            EntityInText::new(EntityType::Italic, 5, 6),
            EntityInText::new(EntityType::StrikeOut, 12, 6),
            EntityInText::new(EntityType::Code, 19, 4),
            EntityInText::new(EntityType::Spoiler, 24, 7),
            EntityInText::with_data(EntityType::CustomUrl, 32, 2, "https://t.me"),
        ]
    );
}

#[test]
fn markdown_nested_formatting_and_links() {
    let input = "**bold __and italic__** ||~~secret~~|| [**Rust** link](https://en.wikipedia.org/wiki/Rust_(programming_language))";
    let parsed = parse_markdown(input);
    assert_eq!(parsed.text, "bold and italic secret Rust link");

    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Bold, 0, 15))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Italic, 5, 10))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Spoiler, 16, 6))
    );
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::StrikeOut, 16, 6))
    );
    assert!(parsed.entities.contains(&EntityInText::with_data(
        EntityType::CustomUrl,
        23,
        9,
        "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    )));
    assert!(
        parsed
            .entities
            .contains(&EntityInText::new(EntityType::Bold, 23, 4))
    );
}

#[test]
fn markdown_code_and_pre_suppress_nesting_and_extract_language() {
    let input = "Check `**not bold** [x](https://a.com)`\n```rust\nfn main() {}\n```";
    let parsed = parse_markdown(input);
    assert_eq!(
        parsed.text,
        "Check **not bold** [x](https://a.com)\nfn main() {}"
    );
    assert_eq!(parsed.entities.len(), 2);
    assert_eq!(
        parsed.entities[0],
        EntityInText::new(EntityType::Code, 6, 31)
    );
    assert_eq!(
        parsed.entities[1],
        EntityInText::with_data(EntityType::Pre, 38, 12, "rust")
    );
    assert_eq!(parsed.entities[1].pre_language(), Some("rust"));
}

#[test]
fn markdown_preserves_underscores_in_words_and_urls() {
    let input = "foo__bar__baz https://example.com/__test__/page";
    let parsed = parse_markdown(input);
    assert_eq!(parsed.text, input);
    assert!(parsed.entities.is_empty());

    // Also via tdesktop's apply_markdown
    let twt = TextWithTags::simple(input);
    let applied = apply_markdown(&twt);
    assert_eq!(applied.text, input);
    assert!(applied.tags.is_empty());
}

#[test]
fn markdown_with_emoji_utf16_offsets() {
    let input = "😀 **hello 🌍** __end__";
    let parsed = parse_markdown(input);
    assert_eq!(parsed.text, "😀 hello 🌍 end");
    // "😀 " = 3 UTF-16 units; "hello 🌍" = 6 + 2 = 8 UTF-16 units; " " = 1; "end" = 3
    assert_eq!(
        parsed.entities,
        vec![
            EntityInText::new(EntityType::Bold, 3, 8),
            EntityInText::new(EntityType::Italic, 12, 3),
        ]
    );
    assert_eq!(parsed.slice_entity(&parsed.entities[0]), Some("hello 🌍"));
    assert_eq!(parsed.slice_entity(&parsed.entities[1]), Some("end"));
}

#[test]
fn prepare_for_sending_cleans_parses_and_trims() {
    let mut msg = TextWithEntities::simple("  \r\nHello\t**world** https://t.me \u{200B}  ");
    prepare_for_sending(&mut msg, TextParseFlags::MARKDOWN | TextParseFlags::LINKS);
    assert_eq!(msg.text, "Hello  world https://t.me");
    assert_eq!(
        msg.entities,
        vec![
            EntityInText::new(EntityType::Bold, 7, 5),
            EntityInText::new(EntityType::Url, 13, 12),
        ]
    );

    // Leading spaces inside a leading Pre block must be preserved by Trim!
    let mut code_msg = TextWithEntities {
        text: "   indented code  ".to_string(),
        entities: vec![EntityInText::with_data(EntityType::Pre, 0, 16, "py")],
    };
    trim(&mut code_msg);
    assert_eq!(code_msg.text, "   indented code");
    assert_eq!(code_msg.entities[0].offset(), 0);
    assert_eq!(code_msg.entities[0].length(), 16);
}

#[test]
fn server_cleaning_shifts_entities_around_tabs_and_cr() {
    let mut msg = TextWithEntities {
        text: "a\tb\r\n🔥c".to_string(),
        // "b" at offset 2 len 1; "🔥c" at offset 5 len 3
        entities: vec![
            EntityInText::new(EntityType::Bold, 2, 1),
            EntityInText::new(EntityType::Italic, 5, 3),
        ],
    };
    apply_server_cleaning(&mut msg);
    // "a  b\n🔥c"
    // 'a'=0, ' '=1, ' '=2, 'b'=3, '\n'=4, '🔥'=5..7, 'c'=7
    assert_eq!(msg.text, "a  b\n🔥c");
    assert_eq!(msg.entities[0], EntityInText::new(EntityType::Bold, 3, 1));
    assert_eq!(msg.entities[1], EntityInText::new(EntityType::Italic, 5, 3));
    assert_eq!(msg.slice_entity(&msg.entities[0]), Some("b"));
    assert_eq!(msg.slice_entity(&msg.entities[1]), Some("🔥c"));
}

// ============================================================================
// 4. Tags <-> Entities Conversion & QDataStream Serialization
// ============================================================================

#[test]
fn convert_entities_and_tags_round_trip() {
    let date_data = serialize_formatted_date_data(
        1_700_000_000,
        FormattedDateFlags::SHORT_DATE | FormattedDateFlags::SHORT_TIME,
    );
    let mention_data = mention_name_data_from_fields(MentionNameFields {
        self_id: 100,
        user_id: 200,
        access_hash: 300,
    });

    let mut entities = vec![
        EntityInText::with_data(EntityType::Blockquote, 0, 20, "1"),
        EntityInText::new(EntityType::Bold, 0, 10),
        EntityInText::with_data(EntityType::CustomUrl, 0, 5, "https://example.com"),
        EntityInText::new(EntityType::Italic, 5, 10),
        EntityInText::with_data(EntityType::MentionName, 21, 5, mention_data.clone()),
        EntityInText::with_data(EntityType::CustomEmoji, 27, 2, "5368324170671202286"),
        EntityInText::with_data(EntityType::Pre, 30, 8, "rust"),
        EntityInText::with_data(EntityType::FormattedDate, 40, 6, date_data.clone()),
        EntityInText::new(EntityType::Spoiler, 47, 4),
    ];
    sort_entities(&mut entities);

    let tags = convert_entities_to_text_tags(&entities);
    assert!(!tags.is_empty());

    let back = convert_text_tags_to_entities(&tags);
    // Blockquote wraps everything [0..20)
    assert!(back.contains(&EntityInText::with_data(EntityType::Blockquote, 0, 20, "1")));
    // CustomUrl [0..5) closes at 5, unwinding inner Bold so entities remain strictly nested:
    // Bold splits into [0..5) inside CustomUrl and [5..10) outside CustomUrl.
    assert!(back.contains(&EntityInText::with_data(
        EntityType::CustomUrl,
        0,
        5,
        "https://example.com",
    )));
    assert!(back.contains(&EntityInText::new(EntityType::Bold, 0, 5)));
    assert!(back.contains(&EntityInText::new(EntityType::Bold, 5, 5)));
    // Italic [5..15) is split cleanly at 10 if needed or stays nested around Bold [5..10)
    assert!(
        back.contains(&EntityInText::new(EntityType::Italic, 5, 10))
            || (back.contains(&EntityInText::new(EntityType::Italic, 5, 5))
                && back.contains(&EntityInText::new(EntityType::Italic, 10, 5)))
    );
    assert!(back.contains(&EntityInText::with_data(
        EntityType::MentionName,
        21,
        5,
        mention_data,
    )));
    assert!(back.contains(&EntityInText::with_data(
        EntityType::CustomEmoji,
        27,
        2,
        "5368324170671202286",
    )));
    assert!(back.contains(&EntityInText::with_data(EntityType::Pre, 30, 8, "rust")));
    assert!(back.contains(&EntityInText::with_data(
        EntityType::FormattedDate,
        40,
        6,
        date_data,
    )));
    assert!(back.contains(&EntityInText::new(EntityType::Spoiler, 47, 4)));
}

#[test]
fn serialize_and_deserialize_tags_binary_round_trip() {
    let tags = vec![
        TextTag::new(0, 5, TAG_BOLD),
        TextTag::new(6, 4, format!("{TAG_ITALIC}\\{TAG_SPOILER}")),
        TextTag::new(11, 2, "https://example.com/😀"),
    ];
    let bytes = serialize_tags(&tags);
    assert_eq!(bytes.len() + 4, serialize_tags_size(&tags));

    let decoded = deserialize_tags(&bytes, 20);
    assert_eq!(decoded, tags);

    // Out-of-bounds text length rejects invalid trailing tags safely without panicking
    let truncated = deserialize_tags(&bytes, 10);
    assert_eq!(truncated.len(), 2);

    // Corrupt / truncated buffers never panic
    assert!(deserialize_tags(&[], 10).is_empty());
    assert!(deserialize_tags(&[0, 0, 0, 5, 1, 2], 10).is_empty());
}

#[test]
fn tag_helpers_join_split_simplify_and_trim_full_coverage() {
    assert_eq!(tag_with_added(TAG_ITALIC, TAG_BOLD), "**\\__");
    assert_eq!(tag_with_removed("**\\__", TAG_BOLD), "__");
    // Code cannot combine with Bold, only with Blockquote
    assert_eq!(join_tag(&[TAG_BOLD, TAG_CODE]), TAG_BOLD);
    assert_eq!(join_tag(&[TAG_BLOCKQUOTE, TAG_CODE]), ">\\`");

    let simplified = simplify_tags(vec![
        TextTag::new(0, 3, "__\\**"),
        TextTag::new(3, 4, "**\\__"),
    ]);
    assert_eq!(simplified.len(), 1);
    assert_eq!(simplified[0].offset, 0);
    assert_eq!(simplified[0].length, 7);

    // TrimFullCoverageTags drops whole-text Bold, keeps partial Italic and whole-text Blockquote
    let mut twt = TextWithTags {
        text: "Hello world".to_string(),
        tags: vec![TextTag::new(0, 5, "**\\>"), TextTag::new(5, 6, "**\\__\\>")],
    };
    trim_full_coverage_tags(&mut twt);
    assert_eq!(twt.tags.len(), 2);
    assert_eq!(twt.tags[0], TextTag::new(0, 5, ">"));
    assert_eq!(twt.tags[1], TextTag::new(5, 6, "__\\>"));
}

// ============================================================================
// 5. CutPart, SingleLine, RemoveAccents, RemoveEmoji, SearchWords, Merge/Sort
// ============================================================================

#[test]
fn cut_part_splits_cleanly_on_paragraphs_and_breakable_entities() {
    let mut left = TextWithEntities::simple("First paragraph.\n\nSecond paragraph is much longer.");
    let mut sending = TextWithEntities::new();
    assert!(cut_part(&mut sending, &mut left, 25));
    assert_eq!(sending.text, "First paragraph.");
    assert_eq!(left.text, "\n\nSecond paragraph is much longer.");

    // Breakable Pre entity splits into both halves with adjusted offsets
    let mut left_pre = TextWithEntities {
        text: "line one\nline two\nline three".to_string(),
        entities: vec![EntityInText::with_data(EntityType::Pre, 0, 28, "txt")],
    };
    let mut part = TextWithEntities::new();
    assert!(cut_part(&mut part, &mut left_pre, 14));
    assert_eq!(part.text, "line one");
    assert_eq!(
        part.entities,
        vec![EntityInText::with_data(EntityType::Pre, 0, 8, "txt")]
    );
    assert_eq!(left_pre.text, "\nline two\nline three");
    assert_eq!(
        left_pre.entities,
        vec![EntityInText::with_data(EntityType::Pre, 0, 20, "txt")]
    );

    // Never splits a multi-unit emoji
    let mut left_emoji = TextWithEntities::simple("abc😀def");
    let mut part_emoji = TextWithEntities::new();
    assert!(cut_part(&mut part_emoji, &mut left_emoji, 4));
    assert!(std::str::from_utf8(part_emoji.text.as_bytes()).is_ok());
    assert!(std::str::from_utf8(left_emoji.text.as_bytes()).is_ok());
    assert_eq!(
        format!("{}{}", part_emoji.text, left_emoji.text),
        "abc😀def"
    );
}

#[test]
fn text_preprocessing_helpers() {
    assert_eq!(escape_for_rich_parsing(r"a\b[c]"), r"a\\b\[c]");
    assert_eq!(single_line("  hello\nworld  "), "hello world");
    assert_eq!(
        remove_accents("Café naïve ёлка ＡＢＣ１２３"),
        "Cafe naive елка ABC123"
    );
    assert_eq!(name_sort_key("Éclair"), "eclair");
    assert_eq!(
        prepare_search_words("  „Telegram“ Café-Bar! "),
        vec!["telegram", "cafe", "bar"]
    );
    assert_eq!(remove_emoji("Hello 😀 world 👨‍👩‍👧‍👦!"), "Hello  world !");
}

#[test]
fn entity_merging_and_canonical_sorting() {
    let merged = merge_and_sort_entities(vec![
        EntityInText::new(EntityType::Italic, 0, 5),
        EntityInText::new(EntityType::Bold, 5, 5),
        EntityInText::new(EntityType::Bold, 0, 5),
        EntityInText::new(EntityType::Blockquote, 0, 10),
        EntityInText::new(EntityType::Bold, 2, 0), // zero-len dropped
    ]);
    assert_eq!(
        merged,
        vec![
            EntityInText::new(EntityType::Blockquote, 0, 10),
            EntityInText::new(EntityType::Bold, 0, 10),
            EntityInText::new(EntityType::Italic, 0, 5),
        ]
    );
}

// ============================================================================
// 6. Utilities, Language-Specific Uppercasing, Song & Value Formatting
// ============================================================================

#[test]
fn text_utilities_and_c_plus_plus_test_text_scenarios() {
    // RichLangValue
    let rl = rich_lang_value("Click **here** or __there__");
    assert_eq!(rl.text, "Click here or there");
    assert_eq!(
        rl.entities,
        vec![
            EntityInText::new(EntityType::Bold, 6, 4),
            EntityInText::new(EntityType::Italic, 14, 5),
        ]
    );

    // Mid slicing across UTF-16 offsets and clipping entities
    let m = mid(&rl, 8, Some(8)); // "re or th"
    assert_eq!(m.text, "re or th");
    assert_eq!(
        m.entities,
        vec![
            EntityInText::new(EntityType::Bold, 0, 2),
            EntityInText::new(EntityType::Italic, 6, 2),
        ]
    );

    // Filtered
    let only_bold = filtered(&rl, &[EntityType::Bold]);
    assert_eq!(only_bold.entities.len(), 1);
    assert_eq!(only_bold.entities[0].entity_type(), EntityType::Bold);

    // WrapEmailPattern
    let email = wrap_email_pattern("a***b@example.com");
    assert_eq!(
        email.entities,
        vec![EntityInText::new(EntityType::Spoiler, 1, 3)]
    );

    // Colorized (from test_text.cpp scenarios)
    let c0 = colorized("Colorized link span", 0, 0);
    assert_eq!(c0.entities[0].data(), "");
    let c1 = colorized("Colorized link span", 1, 0);
    assert_eq!(c1.entities[0].data(), "\u{0001}");
    let c2 = colorized("Colorized link span", 1, 1);
    assert_eq!(c2.entities[0].data(), "\u{0001}\u{0001}");

    // SingleCustomEmoji (from test_text.cpp)
    let ce = single_custom_emoji("test-custom-emoji", "");
    assert_eq!(ce.text, "@");
    assert_eq!(
        ce.entities,
        vec![EntityInText::with_data(
            EntityType::CustomEmoji,
            0,
            1,
            "test-custom-emoji"
        )]
    );

    // TextForMimeData::with_expanded_links
    let mut linked = TextWithEntities::simple("Open ");
    linked.append(link("Telegram", "https://telegram.org"));
    linked.append_str(" now");
    let mime = TextForMimeData::with_expanded_links(&linked);
    assert_eq!(mime.expanded, "Open Telegram (https://telegram.org) now");
}

#[test]
fn language_specific_upper_and_capitalized() {
    // Greek tonos stripped, disjunctive ή preserved, dialytika inserted on άι -> ΑΪ
    assert_eq!(upper("αθήνα"), "ΑΘΗΝΑ");
    assert_eq!(upper("ναι ή όχι"), "ΝΑΙ Ή ΟΧΙ");
    assert_eq!(upper("μάιος"), "ΜΑΪΟΣ");

    // Turkic dotted I
    let tr = upper_rules_for_language("tr_TR");
    assert_eq!(tr, UpperRules::Turkic);
    assert_eq!(upper_with_rules("istanbul", tr), "İSTANBUL");
    assert_eq!(capitalized_with_rules("istanbul", tr), "İstanbul");

    // Georgian keeps Mkhedruli
    let ka = upper_rules_for_language("ka-GE");
    assert_eq!(upper_with_rules("გამარჯობა abc", ka), "გამარჯობა ABC");

    // Armenian reformed spelling
    let hy = upper_rules_for_language("hy");
    assert_eq!(upper_with_rules("և", hy), "ԵՎ");
    assert_eq!(capitalized_with_rules("ևս", hy), "Եվս");
}

#[test]
fn song_and_value_formatters() {
    let song = FormatSongName::new("track.mp3", "Bohemian Rhapsody", "Queen");
    assert_eq!(song.string(), "Queen \u{2013} Bohemian Rhapsody");
    let rich = song.text_with_entities(true);
    assert_eq!(
        rich.entities,
        vec![EntityInText::new(EntityType::Semibold, 0, 5)]
    );

    assert_eq!(format_duration_text(65), "01:05");
    assert_eq!(format_duration_text(3665), "1:01:05");
    assert_eq!(format_image_size_text(1920, 1080), "1920\u{00D7}1080");
    assert_eq!(format_size_text(512), "512 B");
    assert_eq!(format_size_text(2048), "2.0 KB");
    assert_eq!(format_size_text(5 * 1024 * 1024), "5.0 MB");

    assert_eq!(fill_amount_and_currency(123456, "USD", false), "$1,234.56");
    assert_eq!(fill_amount_and_currency(10000, "USD", true), "$100");
    assert_eq!(
        fill_amount_and_currency(-500, "EUR", false),
        "\u{2212}5,00 \u{20AC}"
    );
    assert_eq!(
        fill_amount_and_currency(1500, "JPY", false),
        "\u{00A5}1,500"
    );
    assert_eq!(
        fill_amount_and_currency(250, CREDITS_CURRENCY, false),
        "\u{2B50}250"
    );
    assert!(known_currency("USD"));
    assert!(!known_currency("ZZZ"));
}

// ============================================================================
// 7. Message Field Pure Helpers & InstantReplaces
// ============================================================================

#[test]
fn message_field_helpers_and_instant_replaces() {
    assert_eq!(prepare_mention_tag(1, 2, 3), "mention://1.2:3");
    assert!(is_valid_code_language("c++"));
    assert!(is_valid_code_language("objective-c"));
    assert!(!is_valid_code_language("rust lang"));
    assert!(is_good_factcheck_url("https://t.me/channel/1"));
    assert!(!is_good_factcheck_url("https://example.com"));

    assert!(!has_send_text("   \r\n\u{200B}\t  "));
    assert!(has_send_text("  a  "));

    // StripSupportHashtag
    let stripped = strip_support_hashtag(TextWithEntities::simple("Answer text\n#tsf123 #other"));
    assert_eq!(stripped.text, "Answer text");

    // PreventInlineBotQuery
    let prevented = prevent_inline_bot_query(TextWithEntities::simple("@gif cats"));
    assert_eq!(prevented.text, " @gif cats");

    // ParseInlineBotQuery
    let q = parse_inline_bot_query(&TextWithTags::simple("@gif cute cats"));
    assert_eq!(
        q,
        Some(ParsedInlineBotQuery {
            username: "gif".to_string(),
            query: "cute cats".to_string(),
        })
    );
    assert!(parse_inline_bot_query(&TextWithTags::simple("@ab hi")).is_none());

    // ParseMentionHashtagBotCommandQuery
    let features = AutocompleteFeatures::default();
    let ac = parse_mention_hashtag_bot_command_query("Hello @dur", 10, features);
    assert_eq!(ac.query, "@dur");
    assert!(!ac.from_start);
    let ac_cmd = parse_mention_hashtag_bot_command_query("/hel", 4, features);
    assert_eq!(ac_cmd.query, "/hel");
    assert!(ac_cmd.from_start);

    // MessageLinksParser
    let links = parse_message_links(
        &TextWithTags {
            text: "See example.com and click".to_string(),
            tags: vec![TextTag::new(20, 5, "https://t.me")],
        },
        &[],
    );
    assert_eq!(links.len(), 2);
    assert_eq!(
        links[0],
        MessageLinkRange {
            start: 4,
            length: 11,
            custom: String::new()
        }
    );
    assert_eq!(
        links[1],
        MessageLinkRange {
            start: 20,
            length: 5,
            custom: "https://t.me".to_string(),
        }
    );

    // EditFieldSelection collapses 2-unit emoji into 1 field unit
    let sel = edit_field_selection(&TextWithTags::simple("a😀b"), TextSelection::new(0, 4));
    assert_eq!(sel, TextSelection::new(0, 3));

    // InstantReplaces
    let text_only = InstantReplaces::text_only();
    assert_eq!(
        text_only.find_suffix_match("hello --"),
        Some((2, "\u{2014}"))
    );
    assert_eq!(
        text_only.find_suffix_match("ok :shrug:"),
        Some((7, "\u{00AF}\\_(\u{30C4})_/\u{00AF}"))
    );

    let def = InstantReplaces::default_replaces();
    assert!(def.find_suffix_match("hi :-)").is_some());
    assert!(def.find_suffix_match("love <3").is_some());
    assert!(def.find_suffix_match("yay :party:").is_some());
}

// ============================================================================
// 8. Emoji Database & Suggestions
// ============================================================================

#[test]
fn emoji_database_find_variants_categories_and_replaces() {
    assert!(emoji::full_count() > 3000);
    for sec in [
        Section::People,
        Section::Nature,
        Section::Food,
        Section::Activity,
        Section::Travel,
        Section::Objects,
        Section::Symbols,
    ] {
        assert!(emoji::section_count(sec) > 50);
        assert_eq!(emoji::section(sec).len(), emoji::section_count(sec));
    }

    // Simple emoji
    let (grin, len) = emoji::find_emoji("😀 hello", 0).expect("find grinning face");
    assert_eq!(len, "😀".len());
    assert_eq!(grin.id(), "😀");
    assert!(!grin.has_variants());

    // Skin-tone variants on 👍
    let (thumb, len) = emoji::find_emoji("👍", 0).expect("find thumbs up");
    assert_eq!(len, "👍".len());
    assert!(thumb.has_variants());
    assert_eq!(thumb.variants_count(), 5);
    assert!(!thumb.colored());
    let medium = thumb.variant(3);
    assert!(medium.colored());
    assert_eq!(medium.original(), thumb);
    assert_eq!(thumb.variant_index(medium), 3);
    assert_eq!(emoji::find_emoji("👍🏽", 0).map(|(e, _)| e), Some(medium));

    // ZWJ family sequence matches longest sequence, not just leading 👨
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let (_, flen) = emoji::find_emoji(family, 0).expect("find ZWJ family");
    assert_eq!(flen, family.len());

    // Postfix-required emojis: ™, ©, ® require U+FE0F
    assert!(emoji::find_emoji("\u{2122}", 0).is_none());
    assert!(emoji::find_emoji("\u{2122}\u{FE0F}", 0).is_some());
    assert!(emoji::find_emoji("\u{00A9}", 0).is_none());
    assert!(emoji::find_emoji("\u{00A9}\u{FE0F}", 0).is_some());
    assert!(emoji::find_emoji("\u{00AE}", 0).is_none());
    assert!(emoji::find_emoji("\u{00AE}\u{FE0F}", 0).is_some());

    // Heart with and without U+FE0F both match, consuming U+FE0F when present
    let (h1, h1_len) = emoji::find_emoji("\u{2764}", 0).expect("bare heart");
    let (h2, h2_len) = emoji::find_emoji("\u{2764}\u{FE0F}", 0).expect("postfixed heart");
    assert_eq!(h1, h2);
    assert_eq!(h1_len, "\u{2764}".len());
    assert_eq!(h2_len, "\u{2764}\u{FE0F}".len());
    assert!(h1.has_postfix());
    assert_eq!(h1.text().as_ref(), "\u{2764}\u{FE0F}");

    // Flag alias: CP (Clipperton Island) maps to FR
    let (fr, _) = emoji::find_emoji("🇫🇷", 0).expect("FR flag");
    let (cp, _) = emoji::find_emoji("🇨🇵", 0).expect("CP alias flag");
    assert_eq!(fr, cp);

    // URL round-trip & old keys & default recent
    let url = thumb.to_url();
    assert_eq!(emoji::from_url(&url), Some(thumb));
    assert!(!emoji::default_recent().is_empty());
    assert_eq!(emoji::color_index_from_code(0xD83C_DFFD), 3);

    // ASCII emoticon replaces
    let (smile, slen) = emoji::find_replace(":-) hi", 0).expect("find :-)");
    assert_eq!(slen, 3);
    assert_eq!(smile.id(), "😊");
    let (heart, hlen) = emoji::find_replace("<3", 0).expect("find <3");
    assert_eq!(hlen, 2);
    assert_eq!(heart, h1);
}

#[test]
fn emoji_autocomplete_suggestions() {
    assert!(emoji::suggestion_max_length() > 10);
    assert!(!emoji::all_suggestions().is_empty());

    assert_eq!(emoji::get_suggestion_emoji(":like:"), Some("👍"));
    assert_eq!(emoji::get_suggestion_emoji(":party:"), Some("🥳"));
    assert!(emoji::get_suggestion_emoji(":nonexistent_code_xyz:").is_none());

    let party = emoji::get_suggestions(":party");
    assert!(!party.is_empty());
    assert_eq!(party[0].replacement, ":party:");
    assert_eq!(party[0].emoji, "🥳");

    // Multi-word prefix query matching (like ":th_up" or ":thumbsup")
    let thumbs = emoji::get_suggestions(":thumbsup");
    assert!(thumbs.iter().any(|s| s.emoji == "👍"));

    // Empty / non-matching query
    assert!(emoji::get_suggestions(":").is_empty());
    assert!(emoji::get_suggestions("").is_empty());
}

#[test]
fn character_classifiers_and_contextual_apostrophes() {
    assert!(!is_word_separator_at("don't", 3)); // apostrophe inside word
    assert!(is_word_separator_at("'hello'", 0)); // leading apostrophe
    assert!(is_word_separator_at("'hello'", 6)); // trailing apostrophe
    assert!(is_word_separator_at("a''b", 1)); // doubled apostrophe

    assert!(is_bad('\0'));
    assert!(is_bad('\u{2028}'));
    assert!(!is_bad('\u{FE0F}'));
    assert!(is_trimmed('\u{200B}'));
    assert!(!is_trimmed(OBJECT_REPLACEMENT_CHAR));
    assert!(is_diacritic('\u{0301}')); // combining acute accent
    assert!(is_replaced_by_space('\u{0007}'));
    assert!(is_link_end('😀'));
}

#[test]
fn url_helpers_replaced_ranges_block_tag_shifts_and_edit_comparison() {
    // URL encoding / decoding / params / validation
    assert_eq!(url_encode("hello world&a=1"), "hello%20world%26a%3D1");
    assert_eq!(url_decode("hello+world%26a%3D1"), "hello world&a=1");
    let params = url_parse_params("Key=Val+1&key=dup&=skip&empty", true);
    assert_eq!(params.get("key").map(String::as_str), Some("Val 1"));
    assert_eq!(params.get("empty").map(String::as_str), Some(""));
    assert_eq!(params.len(), 2);

    assert_eq!(
        url_append_query_or_hash("https://t.me/a", "x=1"),
        "https://t.me/a?x=1"
    );
    assert_eq!(
        url_append_query_or_hash("https://t.me/a?b=2", "x=1"),
        "https://t.me/a?b=2&x=1"
    );
    assert_eq!(validate_url("example.com/a"), "http://example.com/a");
    assert_eq!(validate_url("https://t.me/test"), "https://t.me/test");
    assert_eq!(
        validate_url("http://127.0.0.1:8080/ok"),
        "http://127.0.0.1:8080/ok"
    );
    assert_eq!(validate_url("javascript://bad"), "");
    assert!(is_ipv6("2001:db8::1"));
    assert!(!is_ipv6("127.0.0.1"));

    let wrapped_url = encode_internal_wrapped_url("https://example.com/a?b=1", "src=chat");
    assert_eq!(
        external_url_from_internal_url(&wrapped_url),
        "https://example.com/a?b=1"
    );
    assert_eq!(
        encode_for_opening("wallet.ton/pay"),
        "tonsite://wallet.ton/pay"
    );
    assert_eq!(encode_for_opening("example.com"), "https://example.com");
    assert_eq!(encode_for_opening("a@b.com"), "a@b.com");
    assert_eq!(strip_url_protocol("https://t.me/durov"), "t.me/durov");

    // ReplacedTextRanges & EntityReplacesText
    let mut ranges = ReplacedTextRanges::new();
    ranges.add(2, 5);
    ranges.add(10, 14);
    ranges.prepare();
    assert!(!ranges.overlaps(0, 2));
    assert!(ranges.overlaps(4, 6));
    assert!(!ranges.overlaps(5, 10));
    assert!(ranges.overlaps(11, 12));

    assert!(entity_replaces_text(&EntityInText::with_data(
        EntityType::CustomEmoji,
        0,
        2,
        "123",
    )));
    assert!(!entity_replaces_text(&EntityInText::with_data(
        EntityType::FormattedDate,
        0,
        5,
        serialize_formatted_date_data(1_700_000_000, FormattedDateFlags::empty()),
    )));
    assert!(entity_replaces_text(&EntityInText::with_data(
        EntityType::FormattedDate,
        0,
        5,
        serialize_formatted_date_data(1_700_000_000, FormattedDateFlags::RELATIVE),
    )));

    // Block tag shifting & CheckFullTextTag
    let shifted_l = shift_left_block_tag(TextWithTags {
        text: "\nquote".to_string(),
        tags: vec![TextTag::new(0, 6, TAG_BLOCKQUOTE)],
    });
    assert_eq!(shifted_l.tags, vec![TextTag::new(1, 5, TAG_BLOCKQUOTE)]);
    let shifted_r = shift_right_block_tag(TextWithTags {
        text: "quote\n".to_string(),
        tags: vec![TextTag::new(0, 6, TAG_BLOCKQUOTE)],
    });
    assert_eq!(shifted_r.tags, vec![TextTag::new(0, 5, TAG_BLOCKQUOTE)]);
    assert!(has_full_text_tag(
        &TextWithTags {
            text: "  hi  ".to_string(),
            tags: vec![TextTag::new(2, 2, TAG_BOLD)],
        },
        TAG_BOLD
    ));

    // EditTextChanged ignores unique custom emoji counters
    let orig = TextWithTags {
        text: "😀".to_string(),
        tags: vec![TextTag::new(0, 2, "custom-emoji://999?1")],
    };
    let same = TextWithTags {
        text: "😀".to_string(),
        tags: vec![TextTag::new(0, 2, "custom-emoji://999?42")],
    };
    assert!(!edit_text_changed(&orig, &same));
    assert_eq!(fix_ampersand_in_action("A & B"), "A && B");
    assert_eq!(words("hello, world! 123"), vec!["hello", "world", "123"]);
    assert_eq!(
        default_text_options().flags,
        TextParseFlags::LINKS | TextParseFlags::MULTILINE | TextParseFlags::MARKDOWN
    );
}
