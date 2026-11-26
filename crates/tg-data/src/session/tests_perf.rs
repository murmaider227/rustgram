//! Tests for the id indices, ordered range queries and message unloading (TGB-2893, package F).

use std::time::Instant;

use super::*;
use crate::changes::ChangeEvent;
use crate::id::{DocumentId, SERVER_MAX_MSG_ID};
use crate::message::{DocumentData, MessageMedia};
use crate::peer::LoadedStatus;
use crate::text::TextWithEntities;

const SELF_ID: UserId = UserId(1);

fn user_peer(id: u64) -> PeerId {
    PeerId::from_user(UserId(id))
}

fn text_message(peer: PeerId, id: i64) -> Message {
    let mut msg = Message::new_text(
        peer,
        MsgId(id),
        Some(peer),
        1_000 + id as i32,
        TextWithEntities::plain(format!("message {id}")),
    );
    if id % 3 == 0 {
        msg.flags.insert(MessageFlags::OUTGOING);
    }
    msg
}

fn photo_message(peer: PeerId, id: i64, photo_id: u64) -> Message {
    let mut msg = text_message(peer, id);
    msg.content = MessageContent::Regular {
        text: TextWithEntities::plain(""),
        media: MessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(photo_id),
            ..PhotoData::default()
        })),
    };
    msg
}

fn document_message(peer: PeerId, id: i64, document_id: u64) -> Message {
    let mut msg = text_message(peer, id);
    msg.content = MessageContent::Regular {
        text: TextWithEntities::plain(""),
        media: MessageMedia::Document(Box::new(DocumentData {
            id: DocumentId(document_id),
            ..DocumentData::default()
        })),
    };
    msg
}

/// Loads `ids` into `peer` as one gap-free slice ending at the bottom when `bottom`.
fn add_slice(
    session: &mut Session,
    peer: PeerId,
    ids: std::ops::RangeInclusive<i64>,
    bottom: bool,
) {
    let from = MsgId(*ids.start());
    let till = if bottom {
        SERVER_MAX_MSG_ID
    } else {
        MsgId(*ids.end())
    };
    let messages = ids.map(|id| text_message(peer, id)).collect();
    session.add_history_slice(peer, messages, MsgRange::new(from, till), None);
}

/// The ordered walk the live backend does today (`history_messages_ordered`).
fn naive_ordered(session: &Session, peer: PeerId) -> Vec<MsgId> {
    let Some(history) = session.history(peer) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for slice in history.loaded_ranges.slices() {
        for &mid in &slice.messages {
            if session.message(FullMsgId::new(peer, mid)).is_some() {
                out.push(mid);
            }
        }
    }
    out
}

fn ids(messages: impl IntoIterator<Item = impl std::ops::Deref<Target = Message>>) -> Vec<MsgId> {
    messages.into_iter().map(|m| m.id).collect()
}

/// Deterministic pseudo-random numbers for the randomized comparisons.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound.max(1)
    }
}

/// A history with gaps (several slices), holes (deleted ids) and an unread tail.
fn random_history(rng: &mut Lcg) -> (Session, PeerId) {
    let mut session = Session::new(SELF_ID);
    session.set_message_budget(None);
    let peer = user_peer(2);
    let mut start = 1 + rng.next(5) as i64;
    let slices = 1 + rng.next(4);
    for index in 0..slices {
        let len = 1 + rng.next(60) as i64;
        let bottom = index + 1 == slices && rng.next(2) == 0;
        add_slice(&mut session, peer, start..=start + len - 1, bottom);
        start += len + 2 + rng.next(20) as i64;
    }
    let all = naive_ordered(&session, peer);
    let deletions: Vec<MsgId> = all.iter().copied().filter(|_| rng.next(7) == 0).collect();
    session.delete_messages(peer, &deletions);
    if let Some(last) = naive_ordered(&session, peer).last() {
        let read_till = MsgId(last.0 - rng.next(30) as i64);
        session.mark_inbox_read(peer, MsgId(read_till.0 + 1), Some(1 + rng.next(9) as i32));
    }
    (session, peer)
}

#[test]
fn ordered_queries_match_the_full_walk_on_random_histories() {
    let mut rng = Lcg(7);
    for _ in 0..300 {
        let (session, peer) = random_history(&mut rng);
        let all = naive_ordered(&session, peer);
        assert_eq!(ids(session.loaded_messages(peer)), all);
        assert_eq!(session.loaded_message_count(peer), all.len());
        let max = all.last().map_or(10, |m| m.0) + 5;
        for _ in 0..20 {
            let target = MsgId(rng.next(max as u64 + 1) as i64);
            let before: Vec<MsgId> = all
                .iter()
                .rev()
                .copied()
                .filter(|id| *id < target)
                .collect();
            assert_eq!(ids(session.loaded_messages_before(peer, target)), before);
            let after: Vec<MsgId> = all.iter().copied().filter(|id| *id > target).collect();
            assert_eq!(ids(session.loaded_messages_after(peer, target)), after);
            let from: Vec<MsgId> = all.iter().copied().filter(|id| *id >= target).collect();
            assert_eq!(ids(session.loaded_messages_from(peer, target)), from);

            let (limit_before, limit_after) = (rng.next(30) as usize, rng.next(30) as usize);
            let expected = if all.is_empty() {
                Vec::new()
            } else {
                let center = all.partition_point(|id| *id < target).min(all.len() - 1);
                let start = center.saturating_sub(limit_before);
                let end = (center + limit_after + 1).min(all.len());
                all[start..end].to_vec()
            };
            assert_eq!(
                ids(session.loaded_messages_around(peer, target, limit_before, limit_after)),
                expected,
                "around {target:?} -{limit_before} +{limit_after} in {all:?}"
            );
            let window = rng.next(50) as usize;
            let last = all[all.len().saturating_sub(window)..].to_vec();
            assert_eq!(ids(session.last_loaded_messages(peer, window)), last);
        }
        let history = session.history(peer).expect("history");
        let read_till = history.inbox_read_till_id();
        let expected_unread = if history.unread_count.unwrap_or(0) > 0 {
            all.iter().copied().find(|id| {
                *id > read_till
                    && !session
                        .message(FullMsgId::new(peer, *id))
                        .is_some_and(Message::is_outgoing)
            })
        } else {
            None
        };
        assert_eq!(session.first_unread_incoming(peer), expected_unread);
    }
}

#[test]
fn media_index_follows_inserts_edits_mutation_and_deletes() {
    let mut session = Session::new(SELF_ID);
    let (a, b) = (user_peer(2), user_peer(3));
    session.insert_message(photo_message(a, 10, 500));
    session.insert_message(photo_message(b, 4, 500));
    session.insert_message(document_message(a, 11, 600));
    let both = vec![FullMsgId::new(a, MsgId(10)), FullMsgId::new(b, MsgId(4))];
    assert_eq!(session.messages_with_media(500), both);
    assert_eq!(
        session.messages_with_media(600),
        vec![FullMsgId::new(a, MsgId(11))]
    );

    // An edit that swaps the photo moves the message to the new key.
    assert!(session.edit_message(photo_message(a, 10, 501)));
    assert_eq!(
        session.messages_with_media(500),
        vec![FullMsgId::new(b, MsgId(4))]
    );
    assert_eq!(
        session.messages_with_media(501),
        vec![FullMsgId::new(a, MsgId(10))]
    );

    // A change through `message_mut` is seen at once and indexed on the next write.
    if let Some(msg) = session.message_mut(FullMsgId::new(b, MsgId(4))) {
        msg.content = MessageContent::Regular {
            text: TextWithEntities::plain(""),
            media: MessageMedia::Empty,
        };
    }
    assert!(session.messages_with_media(500).is_empty());
    session.insert_message(text_message(a, 12));
    assert!(session.messages_with_media(500).is_empty());
    assert!(!session.media_messages.contains_key(&500));

    session.delete_messages(a, &[MsgId(11)]);
    assert!(session.messages_with_media(600).is_empty());
    assert!(session.message_media.len() == 1 && session.media_messages.len() == 1);
}

#[test]
fn media_index_follows_a_local_message_rekeyed_to_its_server_id() {
    let mut session = Session::new(SELF_ID);
    let peer = user_peer(2);
    let local = crate::id::START_CLIENT_MSG_ID;
    let mut msg = photo_message(peer, 1, 700);
    msg.id = local;
    msg.flags.insert(MessageFlags::BEING_SENT);
    session.insert_message(msg);
    session.register_random_id(42, FullMsgId::new(peer, local));
    session.resolve_random_id(42, MsgId(55));
    assert_eq!(
        session.messages_with_media(700),
        vec![FullMsgId::new(peer, MsgId(55))]
    );
    assert_eq!(
        session.stored_message_ids(peer).collect::<Vec<_>>(),
        vec![MsgId(55)]
    );
}

#[test]
fn per_peer_ids_give_top_fallback_and_clear_history() {
    let mut session = Session::new(SELF_ID);
    let peer = PeerId::from_channel(ChannelId(9));
    add_slice(&mut session, peer, 1..=20, true);
    session.delete_messages(peer, &[MsgId(20)]);
    assert_eq!(
        session.history(peer).and_then(|h| h.top_message_id),
        Some(MsgId(19))
    );
    session.clear_history_up_till(peer, MsgId(10));
    let left: Vec<MsgId> = session.stored_message_ids(peer).collect();
    assert_eq!(left, (10..=19).map(MsgId).collect::<Vec<_>>());
}

fn loaded_user(id: u64, username: &str) -> User {
    let mut user = User::new(UserId(id), false);
    user.common.loaded_status = LoadedStatus::Normal;
    user.usernames.set_username(username);
    user
}

#[test]
fn username_lookup_sees_upserts_and_direct_edits() {
    let mut session = Session::new(SELF_ID);
    session.upsert_user(loaded_user(5, "PeerAlpha"), PeerUpdateFlags::empty());
    assert_eq!(session.peer_by_username("@peeralpha"), Some(user_peer(5)));
    assert_eq!(session.peer_by_username(" PEERALPHA "), Some(user_peer(5)));
    assert_eq!(session.peer_by_username("peerbeta"), None);

    if let Some(user) = session.user_mut(UserId(5)) {
        user.usernames.set_username("peerbeta");
    }
    assert_eq!(session.peer_by_username("peerbeta"), Some(user_peer(5)));
    assert_eq!(session.peer_by_username("peeralpha"), None);

    // A `min` (not loaded) peer is not found, as before.
    let mut min_user = User::new(UserId(6), false);
    min_user.usernames.set_username("peergamma");
    session.upsert_user(min_user, PeerUpdateFlags::empty());
    assert_eq!(session.peer_by_username("peergamma"), None);
}

#[test]
fn over_budget_unloads_the_least_recent_chat_down_to_its_newest_messages() {
    let mut session = Session::new(SELF_ID);
    session.set_message_budget(Some(1_000));
    let mut events = session.subscribe();
    let (old, recent) = (user_peer(2), user_peer(3));
    add_slice(&mut session, old, 1..=500, false);
    add_slice(&mut session, old, 600..=1_100, true);
    let old_top = session.history(old).and_then(|h| h.top_message_id);
    add_slice(&mut session, recent, 1..=400, true);
    assert!(
        session.stored_message_count() <= 1_000,
        "{}",
        session.stored_message_count()
    );

    // The least recently loaded chat keeps its newest messages as one bottom slice.
    let kept = naive_ordered(&session, old);
    let expected: Vec<MsgId> = (1_100 - UNLOAD_KEEP_TAIL as i64 + 1..=1_100)
        .map(MsgId)
        .collect();
    assert_eq!(kept, expected);
    let history = session.history(old).expect("history");
    assert!(history.loaded_ranges.loaded_at_bottom());
    assert!(!history.loaded_ranges.loaded_at_top());
    assert_eq!(history.top_message_id, old_top);
    assert_eq!(naive_ordered(&session, recent).len(), 400);

    let mut unloaded_event = false;
    while let Ok(event) = events.try_recv() {
        if let ChangeEvent::HistoryUpdated { peer_id, flags } = event
            && peer_id == old
            && flags.contains(HistoryUpdateFlags::MESSAGES_UNLOADED)
        {
            unloaded_event = true;
        }
        assert!(
            !matches!(event, ChangeEvent::MessageUpdated { flags, .. }
                if flags.contains(MessageUpdateFlags::DESTROYED)),
            "unloading must not look like a deletion"
        );
    }
    assert!(unloaded_event);

    // Scrolling up reloads the older part exactly like a first load.
    add_slice(&mut session, old, 700..=900, false);
    let reloaded = naive_ordered(&session, old);
    assert_eq!(reloaded.first(), Some(&MsgId(700)));
    assert_eq!(reloaded.len(), 201 + UNLOAD_KEEP_TAIL);
}

#[test]
fn nothing_unloads_until_the_ui_reports_open_chats() {
    let mut session = Session::new(SELF_ID);
    for chat in 0..80u64 {
        add_slice(&mut session, user_peer(100 + chat), 1..=400, true);
    }
    assert_eq!(session.stored_message_count(), 80 * 400);
    session.set_history_open(user_peer(179), true);
    session.insert_message(text_message(user_peer(100), 401));
    assert!(session.stored_message_count() <= DEFAULT_MESSAGE_BUDGET);
    assert_eq!(naive_ordered(&session, user_peer(179)).len(), 400);
}

#[test]
fn open_chats_unread_tails_and_sending_chats_stay_loaded() {
    let mut session = Session::new(SELF_ID);
    session.set_message_budget(None);
    let (open, unread, sending, filler) = (user_peer(2), user_peer(3), user_peer(4), user_peer(5));
    add_slice(&mut session, open, 1..=600, true);
    session.set_history_open(open, true);
    add_slice(&mut session, unread, 1..=600, true);
    session.mark_inbox_read(unread, MsgId(100), Some(500));
    add_slice(&mut session, sending, 1..=600, true);
    let mut pending = text_message(sending, 0);
    pending.id = crate::id::START_CLIENT_MSG_ID;
    session.insert_message(pending);
    add_slice(&mut session, filler, 1..=600, true);

    session.set_message_budget(Some(1_000));
    assert_eq!(
        naive_ordered(&session, open).len(),
        600,
        "open chat is never unloaded"
    );
    let unread_kept = naive_ordered(&session, unread);
    assert_eq!(
        unread_kept.first(),
        Some(&MsgId(101)),
        "every unread message stays"
    );
    assert_eq!(session.first_unread_incoming(unread), Some(MsgId(101)));
    let sending_kept = naive_ordered(&session, sending);
    assert_eq!(sending_kept.len(), 600, "a chat with a pending send stays");

    session.set_history_open(open, false);
    session.insert_message(text_message(filler, 601));
    assert!(naive_ordered(&session, open).len() <= 600);
}

#[test]
fn a_long_session_stays_within_budget() {
    let mut session = Session::new(SELF_ID);
    session.set_message_budget(Some(5_000));
    let mut peak = 0;
    for chat in 0..60u64 {
        let peer = user_peer(100 + chat);
        add_slice(&mut session, peer, 1..=400, true);
        for page in 1..4i64 {
            let to = 1_000 * page;
            add_slice(&mut session, peer, to - 399..=to, false);
        }
        peak = peak.max(session.stored_message_count());
    }
    // The chat being loaded can go over by its own size; everything else is bounded.
    assert!(peak <= 5_000 + 1_600, "peak {peak}");
    assert!(
        session.stored_message_count() <= 5_000 + 1_600,
        "{}",
        session.stored_message_count()
    );
    // Every chat still shows its last message in the chat list.
    for chat in 0..60u64 {
        let peer = user_peer(100 + chat);
        let top = session.history(peer).and_then(|h| h.top_message_id);
        assert!(top.is_some_and(|id| session.message(FullMsgId::new(peer, id)).is_some()));
    }
}

/// Before/after timing of a 40-message page in a 10,000-message history:
/// `cargo test --release -p tg-data -- --ignored --nocapture page_query_timing`.
#[test]
#[ignore = "timing; run explicitly with --ignored --nocapture"]
fn page_query_timing() {
    let mut session = Session::new(SELF_ID);
    session.set_message_budget(None);
    let peer = user_peer(2);
    add_slice(&mut session, peer, 1..=10_000, true);
    let target = MsgId(5_000);
    const ROUNDS: u32 = 200;

    let start = Instant::now();
    let mut acc = 0usize;
    for _ in 0..ROUNDS {
        let all = naive_ordered(&session, peer);
        let page: Vec<MsgId> = all
            .iter()
            .rev()
            .copied()
            .filter(|id| *id < target)
            .take(40)
            .collect();
        acc += page.len() + all.len();
    }
    let collect = start.elapsed() / ROUNDS;

    let start = Instant::now();
    for _ in 0..ROUNDS {
        let page: Vec<&Message> = session
            .loaded_messages_before(peer, target)
            .take(40)
            .collect();
        acc += page.len();
    }
    let ranged = start.elapsed() / ROUNDS;
    println!(
        "page of 40 before id 5000 in 10k loaded: collect-all {collect:?}, range query {ranged:?} (acc {acc})"
    );
}
