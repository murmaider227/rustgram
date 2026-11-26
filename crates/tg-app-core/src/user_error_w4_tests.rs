//! TGB-2977 / TGB-2978: the boxes and screens tdesktop opens for a failed request.

use super::*;
use tg_ui::text::TextEntityKind;

fn rpc(code: i32, name: &str) -> String {
    tg_api::ApiError::Invocation(tg_api::RpcError::new(code, name).into()).to_string()
}

fn links(text: &tg_ui::text::RichText) -> Vec<(String, String)> {
    text.spans
        .iter()
        .filter_map(|span| match &span.kind {
            TextEntityKind::Link(url) => {
                Some((text.text[span.start..span.end].to_owned(), url.clone()))
            }
            _ => None,
        })
        .collect()
}

fn bold(text: &tg_ui::text::RichText) -> Vec<String> {
    text.spans
        .iter()
        .filter(|span| span.kind == TextEntityKind::Bold)
        .map(|span| text.text[span.start..span.end].to_owned())
        .collect()
}

#[test]
fn peer_flood_and_banned_link_learn_more_to_spambot() {
    let more = tr::lng_cant_more_info();
    for (error, phrase) in [
        ("PEER_FLOOD", tr::lng_cant_send_to_not_contact(&more)),
        (
            "USER_BANNED_IN_CHANNEL",
            tr::lng_error_public_groups_denied(&more),
        ),
    ] {
        let shown = send_message(&rpc(400, error), SendTarget::Group);
        let UserFacingError::InformRich { title, text } = &shown else {
            panic!("{error}: {shown:?}");
        };
        assert_eq!(title, &None);
        assert_eq!(text.text, phrase);
        assert_eq!(
            links(text),
            vec![(more.clone(), "https://t.me/spambot".to_owned())],
            "{error}"
        );
    }
}

#[test]
fn premium_required_opens_the_premium_screen_and_balance_too_low_is_the_type() {
    assert_eq!(
        send_message(&rpc(400, "PREMIUM_ACCOUNT_REQUIRED"), SendTarget::User),
        UserFacingError::PremiumScreen {
            referral: "premium_stickers".to_owned()
        }
    );
    // No dice stake in Rustgram: `show->showToast(error)` (`apiwrap.cpp:651-653`).
    assert_eq!(
        send_message(&rpc(400, "BALANCE_TOO_LOW"), SendTarget::User),
        UserFacingError::Toast("BALANCE_TOO_LOW".to_owned())
    );
    assert!(
        UserFacingError::PremiumScreen {
            referral: String::new()
        }
        .needs_window()
    );
    assert!(!UserFacingError::Toast(String::new()).needs_window());
}

#[test]
fn limit_errors_open_their_limit_boxes() {
    assert_eq!(
        join_chatlist(&rpc(400, "FILTER_INCLUDE_TOO_MUCH")),
        UserFacingError::LimitBox(LimitBoxKind::FilterChats)
    );
    assert_eq!(
        join_chatlist(&rpc(400, "CHATLISTS_TOO_MUCH")),
        UserFacingError::LimitBox(LimitBoxKind::ShareableFilters)
    );
    assert_eq!(
        join_chatlist(&rpc(400, "CHANNELS_TOO_MUCH")),
        UserFacingError::LimitBox(LimitBoxKind::Channels)
    );
    assert_eq!(
        export_chatlist_invite(&rpc(400, "INVITES_TOO_MUCH")),
        UserFacingError::LimitBox(LimitBoxKind::FilterLinks)
    );
    assert_eq!(
        export_chatlist_invite(&rpc(400, "CHATLISTS_TOO_MUCH")),
        UserFacingError::LimitBox(LimitBoxKind::ShareableFilters)
    );
}

#[test]
fn limit_box_content_follows_simple_limit_box() {
    let free = limit_box_content(LimitBoxKind::FilterChats, false);
    assert_eq!(free.title, tr::lng_filter_chats_limit_title());
    assert_eq!(
        free.increase_ref.as_deref(),
        Some("double_limits__dialog_filters_chats")
    );
    assert!(!free.text.text.contains("**"), "{}", free.text.text);
    let free_bold = bold(&free.text);
    assert_eq!(free_bold[..2], ["100".to_owned(), "200".to_owned()]);

    let premium = limit_box_content(LimitBoxKind::FilterChats, true);
    assert_eq!(premium.increase_ref, None, "Premium gets a single OK");
    assert_eq!(bold(&premium.text), vec!["200".to_owned()]);

    let channels = limit_box_content(LimitBoxKind::Channels, true);
    assert_eq!(
        channels.text.text,
        format!(
            "{} {}",
            tr::lng_channels_limit1(1000),
            tr::lng_channels_limit2_final()
        )
        .replace("**", "")
    );
    assert_eq!(
        limit_box_content(LimitBoxKind::FilterLinks, false)
            .increase_ref
            .as_deref(),
        Some("double_limits__chatlist_invites")
    );
    assert_eq!(
        limit_box_content(LimitBoxKind::ShareableFilters, false).title,
        tr::lng_filter_shared_limit_title()
    );
}

#[test]
fn transcription_failure_is_bubble_text_not_a_toast() {
    assert_eq!(
        transcribe_audio(&rpc(400, "TRANSCRIPTION_FAILED")),
        UserFacingError::Silent
    );
    assert_eq!(
        transcribe_bubble_text(true),
        tr::lng_audio_transcribe_long()
    );
    assert_eq!(transcribe_bubble_text(false), tr::lng_attach_failed());
}

#[test]
fn rich_from_marked_turns_markers_into_bold() {
    let rich = rich_from_marked("Sorry, **3** links and **20** more.");
    assert_eq!(rich.text, "Sorry, 3 links and 20 more.");
    assert_eq!(bold(&rich), vec!["3".to_owned(), "20".to_owned()]);
}

#[test]
fn already_premium_gift_code_toast_has_the_date() {
    // 2026-12-24 15:30 UTC, shown in local time as `langDateTime` does.
    let shown = apply_gift_code(&rpc(400, "PREMIUM_SUB_ACTIVE_UNTIL_1798126200"));
    let UserFacingError::Toast(text) = shown else {
        panic!("{shown:?}");
    };
    let at = tg_history::datetime::LocalDateTime::from_unix_local(1_798_126_200);
    let expected = format!(
        "{:02}.{:02}.{:02} at {}",
        at.date.day,
        at.date.month,
        at.date.year.rem_euclid(100),
        tg_history::datetime::format_time_short(at)
    );
    assert!(
        text.starts_with(&tr::lng_gift_link_already_title()),
        "{text}"
    );
    assert!(text.contains(&expected), "{text} / {expected}");
    assert!(text.contains(&tr::lng_gift_link_already_link()), "{text}");
    assert!(!text.contains("**"), "{text}");
}
