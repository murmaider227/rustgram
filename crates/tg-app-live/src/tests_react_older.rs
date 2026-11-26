//! TGB-3013 / TGB-3017: reactions of messages paged in by scrolling up, built from the MTProto
//! shapes a megagroup's `messages.getHistory` returns (layer 233 `messageReactions` with `min`).

use tg_data::{ChannelId, PeerId as DataPeerId};
use tg_tl::types as tl_types;
use tg_tl::{Identifiable, Serializable, enums as tl_enums, functions as tl_fns};

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, wait_until};
use crate::LiveSession;
use tg_app_core::history::backend::{HistoryBackend, Loadable};
use tg_app_core::stand_in::types::{MsgId, PeerId};

const GROUP_ID: i64 = 700;
const CUSTOM_DOC: i64 = 5_199_576_980_080_601_299;

fn group() -> PeerId {
    PeerId(DataPeerId::from_channel(ChannelId(GROUP_ID as u64)).0)
}

fn tl_group_peer() -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: GROUP_ID,
    })
}

fn count(reaction: tl_enums::Reaction, count: i32) -> tl_enums::ReactionCount {
    tl_enums::ReactionCount::ReactionCount(tl_types::ReactionCount {
        chosen_order: None,
        reaction,
        count,
    })
}

/// `messageReactions` as a channel's history page carries it: `min`, no recent reactors.
fn reactions() -> tl_enums::MessageReactions {
    tl_enums::MessageReactions::MessageReactions(tl_types::MessageReactions {
        min: true,
        can_see_list: false,
        reactions_as_tags: false,
        results: vec![
            count(
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "👍".to_owned(),
                }),
                3,
            ),
            count(
                tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                    document_id: CUSTOM_DOC,
                }),
                7,
            ),
            count(tl_enums::Reaction::Paid, 1),
        ],
        recent_reactions: None,
        top_reactors: None,
    })
}

fn reacted(id: i32, text: &str) -> tl_enums::Message {
    let mut msg = tl_message(id, tl_group_peer(), PEER_ID, text, None);
    if let tl_enums::Message::Message(m) = &mut msg {
        m.reactions = Some(reactions());
    }
    msg
}

/// Megagroup with 50..=52 loaded at the bottom; the server holds 40..=49 before them, every
/// message with reactions.
fn group_with_older_reacted_history() -> (super::tests_reply_targets::FakeRpcClient, LiveSession) {
    let (rpc, live) = build_live(
        DataPeerId(group().0),
        (50..=52).map(|id| reacted(id, "recent")).collect(),
    );
    rpc.set_handler(|ctor, _| {
        (ctor == tl_fns::messages::GetHistory::CONSTRUCTOR_ID).then(|| {
            tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
                inexact: false,
                pts: 1,
                count: 13,
                offset_id_offset: None,
                messages: (40..50).rev().map(|id| reacted(id, "older")).collect(),
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            })
            .to_bytes()
        })
    });
    (rpc, live)
}

fn expected_keys() -> Vec<String> {
    vec![
        "⭐".to_owned(),
        format!("custom:{CUSTOM_DOC}"),
        "👍".to_owned(),
    ]
}

#[test]
fn older_pages_keep_their_reactions_in_tdesktop_order() {
    let (_rpc, live) = group_with_older_reacted_history();
    let mut slice = None;
    assert!(wait_until(|| {
        match live.try_load_slice_before(group(), MsgId(50), 40) {
            Loadable::Ready(s) if !s.messages.is_empty() => {
                slice = Some(s);
                true
            }
            _ => false,
        }
    }));
    let slice = slice.expect("older slice");
    assert_eq!(slice.messages.len(), 10);
    for msg in &slice.messages {
        let keys: Vec<String> = msg.reactions.iter().map(|r| r.emoji.clone()).collect();
        assert_eq!(keys, expected_keys(), "message {}", msg.id.0);
    }
    let bottom = live.load_slice_around(group(), MsgId::SHOW_AT_END, 10, 0);
    for msg in &bottom.messages {
        let keys: Vec<String> = msg.reactions.iter().map(|r| r.emoji.clone()).collect();
        assert_eq!(keys, expected_keys(), "message {}", msg.id.0);
    }
}

#[test]
fn custom_emoji_reactions_are_requested_for_loading() {
    let (rpc, live) = group_with_older_reacted_history();
    let _ = live.load_slice_around(group(), MsgId::SHOW_AT_END, 10, 0);
    assert!(
        wait_until(|| !rpc
            .calls::<tl_fns::messages::GetCustomEmojiDocuments>()
            .is_empty()),
        "the loader asks for the reaction's custom emoji document"
    );
    let calls = rpc.calls::<tl_fns::messages::GetCustomEmojiDocuments>();
    assert!(
        calls.iter().any(|c| c.document_id.contains(&CUSTOM_DOC)),
        "{calls:?}"
    );
}

/// A megagroup post that carried an empty `messageReactions` when it loaded (nobody had reacted
/// yet); the server does not push other members' reactions in big chats.
fn group_with_unreacted_post() -> (super::tests_reply_targets::FakeRpcClient, LiveSession) {
    let mut post = tl_message(60, tl_group_peer(), PEER_ID, "posted earlier", None);
    if let tl_enums::Message::Message(m) = &mut post {
        m.reactions = Some(tl_enums::MessageReactions::MessageReactions(
            tl_types::MessageReactions {
                min: true,
                can_see_list: false,
                reactions_as_tags: false,
                results: Vec::new(),
                recent_reactions: None,
                top_reactors: None,
            },
        ));
    }
    let (rpc, live) = build_live(DataPeerId(group().0), vec![post]);
    rpc.set_handler(|ctor, _| {
        (ctor == tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID).then(|| {
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::MessageReactions(
                    tl_types::UpdateMessageReactions {
                        peer: tl_group_peer(),
                        msg_id: 60,
                        top_msg_id: None,
                        saved_peer_id: None,
                        reactions: reactions(),
                    },
                )],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_795_340_100,
                seq: 0,
            })
            .to_bytes()
        })
    });
    (rpc, live)
}

/// TGB-3013: tdesktop refreshes the reactions of every painted message every 20 s
/// (`HistoryInner::paintEvent` -> `Reactions::poll`). Before, nothing polled painted messages
/// and the slice poll skipped messages whose reactions list was empty, so a message loaded
/// before anyone reacted never showed its reactions.
#[test]
fn painted_messages_get_their_reactions_polled_once_per_interval() {
    let (rpc, mut live) = group_with_unreacted_post();

    // The history passes its painted messages every frame.
    for _ in 0..30 {
        live.poll_painted_reactions(group(), &[MsgId(60)]);
    }
    let mut edited = None;
    assert!(wait_until(|| {
        for update in live.drain_history_updates(group()) {
            if let tg_app_core::history::backend::HistoryLiveUpdate::MessageEdited(m) = update
                && m.id == MsgId(60)
            {
                edited = Some(m);
            }
        }
        edited.is_some()
    }));
    let keys: Vec<String> = edited
        .expect("edited post")
        .reactions
        .iter()
        .map(|r| r.emoji.clone())
        .collect();
    assert_eq!(keys, expected_keys());
    for _ in 0..30 {
        live.poll_painted_reactions(group(), &[MsgId(60)]);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        rpc.calls::<tl_fns::messages::GetMessagesReactions>().len(),
        1,
        "one request per kPollEach however often the post is painted"
    );
}
