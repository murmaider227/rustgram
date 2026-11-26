//! TGB-2966: link entities that arrive from the server reach the message body as link spans,
//! through the whole live path (TL message → `tg_data` → history message → painted spans).

use tg_api::convert::{entities_to_tl, message_from_tl};
use tg_app::history::text_bridge::entities_to_rich_text;
use tg_app::live::convert::message_to_history_message;
use tg_data::{
    ChannelId, EntityKind, PeerId as DataPeerId, Session, TextEntity, UserId as DataUserId,
};
use tg_tl::{enums as tl_enums, types as tl_types};
use tg_ui::text::TextEntityKind;

fn utf16(s: &str) -> i32 {
    s.encode_utf16().count() as i32
}

#[test]
fn server_link_entities_become_link_spans_in_the_message_body() {
    let parts: [(&str, Option<EntityKind>); 15] = [
        ("https://t.me/c/1234567890/261238", Some(EntityKind::Url)),
        (" ", None),
        (
            "notes",
            Some(EntityKind::TextUrl {
                url: "https://example.org/notes".to_owned(),
            }),
        ),
        (" ", None),
        ("@crew_bot", Some(EntityKind::Mention)),
        (" ", None),
        ("#release", Some(EntityKind::Hashtag)),
        (" ", None),
        (
            "Lia Moss",
            Some(EntityKind::MentionName {
                user_id: DataUserId(4242),
            }),
        ),
        (" ", None),
        ("$TON", Some(EntityKind::Cashtag)),
        (" ", None),
        ("/start", Some(EntityKind::BotCommand)),
        (" ", None),
        ("crew@example.org", Some(EntityKind::Email)),
    ];
    let mut text = String::new();
    let mut entities = Vec::new();
    for (piece, kind) in &parts {
        let offset = utf16(&text);
        text.push_str(piece);
        if let Some(kind) = kind {
            entities.push(TextEntity::new(offset, utf16(piece), kind.clone()));
        }
    }
    let mut session = Session::new(DataUserId(100));
    let mut mentioned = tg_data::User::new(DataUserId(4242), false);
    mentioned.access_hash = 4_242_007;
    mentioned.first_name = "Lia".to_owned();
    session.upsert_user(mentioned, tg_data::PeerUpdateFlags::NAME);
    let tl_entities = entities_to_tl(&entities, &session);
    assert_eq!(tl_entities.len(), 8, "every kind maps to a TL entity");

    let peer = DataPeerId::from_channel(ChannelId(700));
    let tl_msg = tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id: 10,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 700 }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_795_340_000,
        message: text.clone(),
        media: None,
        reply_markup: None,
        entities: Some(tl_entities),
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    });
    let msg = message_from_tl(&tl_msg, DataUserId(100)).expect("message");
    assert_eq!(msg.peer_id, peer);
    let history = message_to_history_message(&session, &msg);
    let rich = entities_to_rich_text(&history.text);
    let links: Vec<&str> = rich
        .spans
        .iter()
        .filter(|s| matches!(s.kind, TextEntityKind::Link(_)))
        .map(|s| &rich.text[s.start..s.end])
        .collect();
    assert_eq!(
        links,
        vec![
            "https://t.me/c/1234567890/261238",
            "notes",
            "@crew_bot",
            "#release",
            "Lia Moss",
            "$TON",
            "/start",
            "crew@example.org",
        ]
    );
}
