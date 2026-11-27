//! Link parsers never slice inside a multibyte character at a scheme boundary (TGB-3080).
//!
//! The case-insensitive prefix check used to slice `s[..prefix.len()]` before checking the char
//! boundary, so text pasted into the Add Proxy box such as a flag emoji followed by `:` crashed
//! the app, also in release builds.

use tg_core::local_url::{
    LocalUrlRoute, parse_local_url, parse_proxy_link, try_convert_url_to_local,
};

#[test]
fn local_url_parsers_do_not_panic_on_multibyte_scheme_boundaries() {
    for s in [
        "\u{1f1fa}\u{1f1e6}:",
        "tg:/\u{e9}x",
        "https://t.m\u{e9}",
        "http:/\u{e9}",
        "www\u{e9}",
        "https://www\u{e9}",
        "t.me\u{e9}",
        "https://t.me/prox\u{e9}",
        "https://t.me/sock\u{e9}",
        "https://t.me/webprox\u{e9}",
    ] {
        assert_eq!(try_convert_url_to_local(s), s, "{s:?}");
        assert!(parse_proxy_link(s).is_none(), "{s:?}");
        assert_eq!(parse_local_url(s), None, "{s:?}");
    }
}

#[test]
fn ascii_case_insensitive_prefixes_still_match() {
    assert_eq!(
        try_convert_url_to_local("HTTPS://WWW.T.ME/socks?server=h&port=1"),
        "tg://socks?server=h&port=1"
    );
    assert_eq!(
        parse_local_url("TG://settings/proxy"),
        Some(LocalUrlRoute::Settings("proxy".to_owned()))
    );
}
