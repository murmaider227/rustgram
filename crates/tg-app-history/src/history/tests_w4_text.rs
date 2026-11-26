//! Wave 4 TEXT package tests: clicks on hashtags, cashtags, bot commands and name mentions
//! (TGB-2973), the inline bot on the name line (TGB-2965) and web page previews without a site
//! name or title (TGB-2960). Mock data only; fixtures in `tests_w4_text_snap`.

use egui::Vec2;
use tg_bots::commands::BotStatus;
use tg_ui::entity_link::EntityLink;

use crate::history::HistoryAction;
use crate::history::bubble::{LinkPreviewRows, link_preview_card_height, link_preview_card_rows};
use crate::history::entity_clicks::clicked_command_text;
use crate::history::inner::InnerEvent;
use crate::history::name_line::{name_font, via_text};
use crate::history::tests_w4_text_snap::{
    CASHTAG_MSG, COMMAND_MSG, HASHTAG_MSG, MENTION_MSG, MENTIONED_USER, chat_harness,
    click_message_text, group_peer, inner_harness, link_messages, message, page_preview,
    preview_message, via_message,
};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

// TGB-2973 ------------------------------------------------------------------------------------

#[test]
fn clicks_on_entities_reach_the_widget_as_entity_links() {
    let mut harness = inner_harness(link_messages(group_peer()), Vec2::new(720.0, 520.0));
    let expected = [
        (HASHTAG_MSG, EntityLink::Hashtag("#release".into())),
        (CASHTAG_MSG, EntityLink::Cashtag("$TON".into())),
        (COMMAND_MSG, EntityLink::BotCommand("/start".into())),
        (MENTION_MSG, EntityLink::MentionName(MENTIONED_USER)),
    ];
    for (id, link) in expected {
        let bubble = harness
            .state()
            .inner
            .last_painted_bubble_rect(MsgId(id))
            .expect("message painted");
        harness.state_mut().events.clear();
        crate::history::tests_w4_text_snap::click_at(
            &mut harness,
            bubble.left_top() + Vec2::new(19.0, 16.0),
        );
        let events = &harness.state().events;
        assert!(
            events.contains(&InnerEvent::EntityLink {
                msg_id: MsgId(id),
                link: link.clone(),
            }),
            "{link:?}: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, InnerEvent::OpenLink(_))),
            "{link:?} must not reach the generic link router: {events:?}"
        );
    }
}

#[test]
fn a_url_in_message_text_is_clickable_and_text_still_drag_selects() {
    // The drag-select sensor used to sit on top of the text and swallow every link click.
    let peer = group_peer();
    let mut msg = message(peer, 1, 1001, "Mira Vance", "");
    msg.outgoing = true;
    let url = "https://example.org/a";
    msg.text = tg_text::TextWithEntities {
        text: url.to_owned(),
        entities: vec![tg_text::EntityInText::new(
            tg_text::EntityType::Url,
            0,
            url.len() as i32,
        )],
    };
    let mut harness = inner_harness(vec![msg], Vec2::new(720.0, 300.0));
    let bubble = harness
        .state()
        .inner
        .last_painted_bubble_rect(MsgId(1))
        .expect("message painted");
    let pos = bubble.left_top() + Vec2::new(19.0, 16.0);
    crate::history::tests_w4_text_snap::click_at(&mut harness, pos);
    assert!(
        harness
            .state()
            .events
            .contains(&InnerEvent::OpenLink(url.to_owned())),
        "{:?}",
        harness.state().events
    );

    harness.state_mut().events.clear();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    for dx in [10.0, 30.0, 60.0] {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos + Vec2::new(dx, 0.0)));
        harness.step();
    }
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: pos + Vec2::new(60.0, 0.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    let st = harness.state();
    assert!(
        !st.events
            .iter()
            .any(|e| matches!(e, InnerEvent::OpenLink(_))),
        "a drag is not a click: {:?}",
        st.events
    );
    assert!(
        !st.inner.extract_selected_text().is_empty(),
        "dragging over the text selects it"
    );
}

#[test]
fn a_hashtag_or_cashtag_click_searches_it_in_the_chat() {
    for (id, tag) in [(HASHTAG_MSG, "#release"), (CASHTAG_MSG, "$TON")] {
        let mut harness = chat_harness(Vec2::new(720.0, 560.0));
        click_message_text(&mut harness, id);
        let st = harness.state();
        assert!(st.widget.is_chat_search_open(), "{tag}");
        assert_eq!(st.widget.chat_search().query, tag);
        assert!(
            !st.actions
                .iter()
                .any(|a| matches!(a, HistoryAction::OpenLink(_))),
            "{:?}",
            st.actions
        );
    }
}

#[test]
fn a_bot_command_click_sends_the_command() {
    let mut harness = chat_harness(Vec2::new(720.0, 560.0));
    let peer = harness.state().peer;
    click_message_text(&mut harness, COMMAND_MSG);
    let st = harness.state();
    assert!(
        st.actions.contains(&HistoryAction::SendMessage {
            peer_id: peer,
            text: "/start".to_owned(),
        }),
        "{:?}",
        st.actions
    );
    assert!(
        st.widget
            .inner()
            .messages()
            .iter()
            .any(|m| m.outgoing && m.text.text == "/start" && m.id.0 != COMMAND_MSG),
        "the sent command is appended to the chat"
    );
}

#[test]
fn a_name_mention_click_opens_the_profile() {
    let mut harness = chat_harness(Vec2::new(720.0, 560.0));
    click_message_text(&mut harness, MENTION_MSG);
    assert!(
        harness
            .state()
            .actions
            .contains(&HistoryAction::OpenPeerProfile(PeerId::from_user(
                MENTIONED_USER
            ))),
        "{:?}",
        harness.state().actions
    );
}

#[test]
fn a_command_from_a_bot_gets_the_bot_name_only_in_groups() {
    // `Bot::WrapCommandInChat`.
    let wrap = |kind, cmd, bot, status| clicked_command_text(kind, cmd, bot, status);
    assert_eq!(
        wrap(
            PeerKind::Group,
            "/start",
            Some("crew_bot"),
            BotStatus::HasBots
        ),
        "/start@crew_bot"
    );
    assert_eq!(
        wrap(
            PeerKind::Group,
            "/start",
            Some("crew_bot"),
            BotStatus::Unknown
        ),
        "/start@crew_bot"
    );
    assert_eq!(
        wrap(
            PeerKind::Group,
            "/start@other_bot",
            Some("crew_bot"),
            BotStatus::HasBots
        ),
        "/start@other_bot"
    );
    assert_eq!(
        wrap(
            PeerKind::Group,
            "/start",
            Some("crew_bot"),
            BotStatus::NoBots
        ),
        "/start"
    );
    assert_eq!(
        wrap(PeerKind::Group, "/start", None, BotStatus::HasBots),
        "/start"
    );
    assert_eq!(
        wrap(
            PeerKind::User,
            "/start",
            Some("crew_bot"),
            BotStatus::HasBots
        ),
        "/start"
    );
    assert_eq!(
        wrap(
            PeerKind::Channel,
            "/start",
            Some("crew_bot"),
            BotStatus::HasBots
        ),
        "/start"
    );
}

// TGB-2965 ------------------------------------------------------------------------------------

#[test]
fn the_inline_bot_widens_the_name_line_not_the_bottom_info() {
    let peer = group_peer();
    let plain = message(peer, 1, 1005, "Tavi Roan", "ok");
    let mut via = message(peer, 2, 1002, "Ivo Marten", "ok");
    via.date_unix = plain.date_unix;
    via.extra.via_bot = Some("long_inline_pictures_bot".to_owned());
    let measure = |harness: &egui_kittest::Harness<'_, _>, text: String| {
        harness.ctx.fonts_mut(|f| {
            f.layout_no_wrap(text, name_font(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    let mut harness = inner_harness(vec![plain, via.clone()], Vec2::new(560.0, 300.0));
    harness.step();
    let text = via_text(&via).expect("via text");
    assert_eq!(text, "via @long_inline_pictures_bot");
    let via_w = measure(&harness, text);
    let name_w = measure(&harness, via.sender_name.clone());
    let inner = &harness.state().inner;
    let plain_layout = inner.cached_message_layout(MsgId(1)).expect("layout");
    let via_layout = inner.cached_message_layout(MsgId(2)).expect("layout");
    assert_eq!(
        plain_layout.bottom_info.optimal_size, via_layout.bottom_info.optimal_size,
        "the bottom info shows only the time"
    );
    // `msgPadding.left + name + spacew + via + msgPadding.right`.
    let name_line = 11.0 + name_w + via_w + 11.0;
    assert!(
        via_layout.geometry.width as f32 >= name_line.floor(),
        "the name line holds the name and `via @bot`: {} < {name_line}",
        via_layout.geometry.width
    );
    assert!(via_layout.header_heights.from_name_or_via > 0);
}

#[test]
fn without_a_name_the_inline_bot_gets_its_own_header_line() {
    let peer = group_peer();
    let mut plain = message(peer, 1, 1002, "Ivo Marten", "sunset over the bay");
    plain.outgoing = true;
    let via = via_message(peer, 2, true);
    let mut harness = inner_harness(vec![plain, via], Vec2::new(560.0, 300.0));
    harness.step();
    let inner = &harness.state().inner;
    let plain_layout = inner.cached_message_layout(MsgId(1)).expect("layout");
    let via_layout = inner.cached_message_layout(MsgId(2)).expect("layout");
    assert_eq!(plain_layout.header_heights.from_name_or_via, 0);
    assert!(via_layout.header_heights.from_name_or_via > 0);
}

// TGB-2960 ------------------------------------------------------------------------------------

#[test]
fn a_preview_block_has_only_the_lines_the_page_has() {
    let full = page_preview(Some("Example Notes"), Some("Release plan"), false);
    let no_site = page_preview(None, Some("Release plan"), false);
    let no_title = page_preview(Some("Example Notes"), None, false);
    assert_eq!(
        link_preview_card_rows(&full),
        LinkPreviewRows {
            site_name: Some(5),
            title: Some(20),
            description: Some(37),
            height: 58,
        }
    );
    assert_eq!(
        link_preview_card_rows(&no_site),
        LinkPreviewRows {
            site_name: None,
            title: Some(5),
            description: Some(22),
            height: 43,
        }
    );
    assert_eq!(
        link_preview_card_rows(&no_title),
        LinkPreviewRows {
            site_name: Some(5),
            title: None,
            description: Some(20),
            height: 41,
        }
    );
}

#[test]
fn a_bubble_with_a_shorter_preview_block_is_shorter() {
    let peer = group_peer();
    let full = preview_message(
        peer,
        1,
        page_preview(Some("Example Notes"), Some("Plan"), false),
    );
    // Another sender, so the second bubble also has a name line.
    let mut no_site = preview_message(peer, 2, page_preview(None, Some("Plan"), false));
    no_site.from_id = PeerId::from_user(1009);
    no_site.date_unix = full.date_unix;
    let messages = vec![full, no_site];
    let mut harness = inner_harness(messages, Vec2::new(560.0, 400.0));
    harness.step();
    let inner = &harness.state().inner;
    let full = inner.cached_message_layout(MsgId(1)).expect("layout");
    let no_site = inner.cached_message_layout(MsgId(2)).expect("layout");
    let full_h =
        link_preview_card_height(&page_preview(Some("Example Notes"), Some("Plan"), false));
    let no_site_h = link_preview_card_height(&page_preview(None, Some("Plan"), false));
    assert_eq!(
        full.geometry.height - no_site.geometry.height,
        full_h - no_site_h
    );
}
