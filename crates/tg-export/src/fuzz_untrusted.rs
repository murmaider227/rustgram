//! Panic fuzz over server strings, entities, numbers and dates as written into exports
//! (TGB-3065).

use tg_fuzz::Fuzz;

use crate::data::{TextEntityInput, TextPartType};

const ITERATIONS: usize = 3_000;

const TOKENS: &[&str] = &[
    "a",
    " ",
    "\n",
    "<",
    ">",
    "&",
    "\"",
    "'",
    "\\",
    "/",
    "..",
    ":",
    "javascript:",
    "https://",
    "tg://",
    "mailto:",
    "#",
    "?",
    "%",
    "\u{0}",
    "\u{7f}",
    "\u{e9}",
    "\u{1f600}",
    "\u{1f1fa}\u{1f1e6}",
    "\u{202e}",
    "\u{feff}",
    "\u{d7ff}",
    "+",
    "380",
    "1",
    "image/png",
    "video/mp4",
    "application/x-",
    "con",
    "aux",
    ".exe",
    "*",
    "|",
    "?",
    "\r",
];

const KINDS: &[TextPartType] = &[
    TextPartType::Text,
    TextPartType::Mention,
    TextPartType::Hashtag,
    TextPartType::Url,
    TextPartType::Bold,
    TextPartType::Pre,
    TextPartType::TextUrl,
    TextPartType::MentionName,
    TextPartType::Blockquote,
    TextPartType::CustomEmoji,
];

#[test]
fn export_text_numbers_and_dates_do_not_panic() {
    let mut fuzz = Fuzz::new(0xE490_0001, ITERATIONS);
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 16);
        let len = text.encode_utf16().count();
        let entities: Vec<TextEntityInput> = (0..fuzz.rng().below(5))
            .map(|_| TextEntityInput {
                kind: *fuzz.rng().pick(KINDS),
                offset: fuzz.rng().int(len) as i32,
                length: fuzz.rng().int(len) as i32,
                additional: fuzz.text(TOKENS, 3),
            })
            .collect();
        let n = fuzz.rng().int(1 << 40);
        let date = fuzz.rng().int(2_000_000_000) as i32;
        let utc_offset = fuzz.rng().int(100_000) as i32;
        let f = n as f64 / (1 + fuzz.rng().below(9)) as f64;
        let bytes = fuzz.mutate(text.as_bytes());
        fuzz.check(
            "export",
            i,
            &(&text, &entities, n, date, utc_offset),
            || {
                let parts = crate::data::parse_text_entities(&text, &entities);
                let _ = crate::data::compute_message_text(&parts);
                let _ = crate::output::html::serialize_html_string(&text);
                let _ = crate::output::html::safe_message_href(&text);
                let _ = crate::output::html::safe_http_href(&text);
                let _ = crate::output::json::serialize_json_string(&text);
                let _ = crate::output::json::serialize_json_bytes(&bytes);
                let _ = crate::output::file::OutputFile::prepare_relative_path(&text, &text);
                let _ = crate::stand_in::file_name_from_user_string(&text);
                let _ = crate::stand_in::percent_encode_url(&bytes);
                let _ = crate::stand_in::format_phone(&text);
                let _ = crate::stand_in::fill_amount_and_currency(n, &text);
                let _ = crate::stand_in::format_duration_text(n);
                let _ = crate::stand_in::format_image_size_text(date, utc_offset);
                let _ = crate::stand_in::unix_to_civil(i64::from(date), utc_offset);
                let _ = crate::stand_in::format_utc_offset(utc_offset);
                let _ = crate::data::string_bare_peer_id(&bytes);
                let _ = crate::data::fill_left(&text, date.unsigned_abs() as usize % 64, '0');
                let _ = crate::data::float_to_string(f);
                let _ = crate::data::extension_for_mime(&text);
                let _ = crate::data::format_phone_number(&text);
                let _ = crate::data::prepare_file_name_date_part(date, utc_offset);
                let _ = crate::data::prepare_photo_file_name(date, date, utc_offset);
                let _ = crate::data::format_date_time_with_offset(
                    date, utc_offset, true, '.', ':', ' ',
                );
                let _ = crate::data::format_iso_date_with_offset(date, utc_offset);
                let _ = crate::data::format_money_amount(n, &text);
                let _ = crate::data::format_grams_amount(n);
                let _ = crate::data::format_file_size(n);
                let _ = crate::data::format_duration(n);
                let _ = crate::settings::Settings::deserialize_qt(&bytes);
            },
        );
    }
    fuzz.finish();
}

#[test]
fn hostile_entity_bounds_and_sizes_do_not_overflow() {
    // Old code computed `start + length` in i32 and `size * 10` in i64 unchecked.
    let entities = [TextEntityInput {
        kind: TextPartType::Bold,
        offset: 1,
        length: i32::MAX,
        additional: String::new(),
    }];
    let parts = crate::data::parse_text_entities("abc", &entities);
    assert_eq!(crate::data::compute_message_text(&parts), "abc");
    assert!(crate::data::format_file_size(i64::MAX).ends_with(" MB"));
}
