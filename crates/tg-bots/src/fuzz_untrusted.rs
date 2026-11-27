//! Panic regression and fuzz tests over untrusted-input entry points (TGB-3065).

#[test]
fn http_scheme_with_multibyte_char_at_the_boundary_does_not_panic() {
    // Old code sliced `url[..8]` / `url[..7]` before checking char boundaries.
    for url in ["https:/\u{e9}x", "http:/\u{e9}x", "https://t.me/\u{e9}"] {
        let _ = crate::deep_links::try_convert_url_to_local(url);
    }
}

#[test]
fn game_callback_url_with_multibyte_char_at_byte_five_does_not_panic() {
    // Old code sliced `converted[..5]` to test for `tg://`.
    for url in ["tg:/\u{e9}", "abcd\u{e9}f", "\u{e9}\u{e9}\u{e9}"] {
        let answer = crate::callback::CallbackAnswerInput {
            url: Some(url.to_string()),
            ..Default::default()
        };
        let _ = crate::callback::handle_callback_answer(true, false, false, false, &answer);
    }
}

use tg_fuzz::Fuzz;

#[test]
fn bot_links_and_queries_do_not_panic() {
    const TOKENS: &[&str] = &[
        "https://",
        "http://",
        "tg://",
        "t.me/",
        "telegram.me/",
        "resolve?",
        "domain=",
        "start=",
        "startapp",
        "startgroup=",
        "admin=",
        "change_info+post_messages",
        "choose=",
        "users+bots",
        "game=",
        "appname=",
        "/",
        "?",
        "&",
        "=",
        "%",
        "%2",
        "%zz",
        "+",
        "@",
        "bot",
        "_",
        "a",
        "1",
        " ",
        "\u{e9}",
        "#",
    ];
    let mut fuzz = Fuzz::new(0xB075_0001, 10_000);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(TOKENS, 12);
        let first_tag = fuzz.rng().one_in(2).then(|| fuzz.rng().below(20));
        fuzz.check("bot strings", i, &s, || {
            let _ = crate::deep_links::validate_start_param(&s);
            let _ = crate::deep_links::validate_startapp_param(&s);
            let _ = crate::deep_links::percent_decode(&s, true);
            let _ = crate::deep_links::parse_query_params(&s);
            let _ = crate::deep_links::parse_requested_admin_rights(&s);
            let _ = crate::deep_links::is_valid_domain(&s);
            let _ = crate::deep_links::try_convert_url_to_local(&s);
            let _ = crate::deep_links::is_mini_app_url(&s);
            let _ = crate::deep_links::parse_bot_deep_link(&s, &["ref_", "r_"]);
            let _ = crate::markup::parse_choose_types(&s);
            let _ = crate::inline::parse_inline_bot_query(&s, first_tag);
            let _ = crate::inline::prevent_inline_bot_query(&s);
        });
    }
    fuzz.finish();
}
