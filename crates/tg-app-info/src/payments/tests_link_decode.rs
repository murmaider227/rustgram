//! Percent-decoding of `tg://passport` and `tg://invoice` link values.

use super::{parse_invoice_deep_link, parse_passport_deep_link};

fn passport_scope(link: &str) -> String {
    parse_passport_deep_link(link)
        .expect("passport link with bot_id and scope parses")
        .scope
}

#[test]
fn percent_before_a_multibyte_char_does_not_panic() {
    assert_eq!(
        passport_scope("tg://passport?bot_id=1&scope=%a\u{e9}x"),
        "%a\u{e9}x"
    );
    assert_eq!(
        passport_scope("tg://passport?bot_id=1&scope=%\u{e9}\u{e9}"),
        "%\u{e9}\u{e9}"
    );
    assert_eq!(
        parse_invoice_deep_link("tg://invoice?slug=%1\u{44f}").as_deref(),
        Some("%1\u{44f}")
    );
}

#[test]
fn signed_hex_is_not_a_percent_escape() {
    // `qthelp::url_decode` (QByteArray::fromPercentEncoding) needs two hex digits.
    assert_eq!(passport_scope("tg://passport?bot_id=1&scope=%+5"), "% 5");
    assert_eq!(passport_scope("tg://passport?bot_id=1&scope=%-5"), "%-5");
}

#[test]
fn escapes_and_plus_decode_like_tdesktop() {
    assert_eq!(
        passport_scope("tg://passport?bot_id=1&scope=%7B%22v%22%3A1%7D+x"),
        "{\"v\":1} x"
    );
    assert_eq!(
        passport_scope("tg://passport?bot_id=1&scope=%D1%8F%2"),
        "\u{44f}%2"
    );
}
