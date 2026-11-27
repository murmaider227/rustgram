//! Large synthetic demo data for the frame benchmark (`tools/bench/frame_bench.py`, TGB-2893).
//!
//! Off unless `RUSTGRAM_BENCH_DATA` is set (`1`, or `chats=<n>,messages=<n>`). Adds `chats`
//! extra dialogs (two messages each) below the stock demo chats and one supergroup, "Bench
//! History", at the top of the list with `messages` messages mixing text of varied length,
//! photos, replies, reactions and day separators.

use super::{
    EntityType, HistoryMedia, HistoryMessage, InfoPeerKind, LocalDateTime, MsgId, PeerId,
    PeerRecord, PhotoAttachment, ReactionCounter, ReplyHeader, rich_text_with_entities,
    session_peers, synthetic_stripped_thumb,
};
use std::collections::HashMap;

/// Peer id of the long-history chat.
pub(super) const BENCH_HISTORY_PEER: PeerId = PeerId::from_channel(900_001);
const BENCH_USER_BASE: u64 = 910_000;
const BASE_UNIX: i32 = 1_795_255_200;

const NAMES: [&str; 12] = [
    "Ada Rowe",
    "Ben Ortiz",
    "Cleo Hart",
    "Dev Mehta",
    "Eli Novak",
    "Fay Lund",
    "Gus Reyes",
    "Hana Ito",
    "Ivo Brandt",
    "Jo Lam",
    "Kai Dorn",
    "Liv Moss",
];

const TEXTS: [&str; 8] = [
    "ok",
    "Pushed the fix, CI is green now.",
    "Can someone look at the layout of the reply preview when the quoted message is very long and wraps onto several lines?",
    "Lunch at 13:00?",
    "The release checklist: update the changelog, bump the version, tag, build the installer, upload, announce. Ping me if any step fails and I will take over.",
    "👍 sounds good",
    "Here is the profile I took yesterday; most of the time goes into text layout and image uploads on the first frame after opening a chat.",
    "Merged.",
];

struct BenchSize {
    chats: usize,
    messages: usize,
}

fn size_from_env() -> Option<BenchSize> {
    let raw = std::env::var("RUSTGRAM_BENCH_DATA").ok()?;
    let raw = raw.trim();
    if raw.is_empty() || raw == "0" {
        return None;
    }
    let mut size = BenchSize {
        chats: 600,
        messages: 2500,
    };
    for part in raw.split(',') {
        let mut kv = part.splitn(2, '=');
        let (Some(k), Some(v)) = (kv.next(), kv.next()) else {
            continue;
        };
        let Ok(n) = v.trim().parse::<usize>() else {
            continue;
        };
        match k.trim() {
            "chats" => size.chats = n.min(20_000),
            "messages" => size.messages = n.min(100_000),
            _ => {}
        }
    }
    Some(size)
}

fn message(peer: PeerId, id: i32, date_unix: i32, n: usize) -> HistoryMessage {
    let outgoing = n % 7 == 3;
    let name = NAMES[n % NAMES.len()];
    let from = if outgoing {
        session_peers::SELF
    } else {
        PeerId::from_user(BENCH_USER_BASE + 50_000 + (n % NAMES.len()) as u64)
    };
    let text = TEXTS[n % TEXTS.len()];
    let text = if n % 11 == 5 {
        rich_text_with_entities(
            &format!("{text} See https://example.com/notes/{n} #bench"),
            &[
                (&format!("https://example.com/notes/{n}"), EntityType::Url),
                ("#bench", EntityType::Hashtag),
            ],
        )
    } else {
        rich_text_with_entities(text, &[])
    };
    let media = (n % 5 == 2).then(|| {
        let landscape = n.is_multiple_of(2);
        HistoryMedia::Photo(PhotoAttachment {
            width: if landscape { 1280 } else { 720 },
            height: if landscape { 800 } else { 1280 },
            stripped_thumb: Some(synthetic_stripped_thumb((n % 200) as u8)),
            fallback_rgb: (
                [(n * 37 % 256) as u8, 0x85, 0x52],
                [0x26, (n * 53 % 256) as u8, 0x6B],
            ),
            file_name: format!("bench_photo_{n}.jpg"),
            size_bytes: 200_000 + n as u64,
        })
    });
    let reply = (n % 3 == 1 && id > 1).then(|| ReplyHeader {
        reply_to_msg_id: MsgId(id - 1),
        author_name: NAMES[(n + 11) % NAMES.len()].to_owned(),
        author_color_id: (n % 7) as u64,
        author_accent: None,
        preview_text: TEXTS[(n + 7) % TEXTS.len()].to_owned(),
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        preview_spoilers: Vec::new(),
        quote_text: None,
    });
    let reactions = if n.is_multiple_of(4) {
        vec![
            ReactionCounter {
                emoji: "👍".to_owned(),
                count: (n % 9 + 1) as u32,
                chosen: n.is_multiple_of(8),
            },
            ReactionCounter {
                emoji: "🔥".to_owned(),
                count: (n % 4 + 1) as u32,
                chosen: false,
            },
        ]
    } else {
        Vec::new()
    };
    HistoryMessage {
        extra: Default::default(),
        id: MsgId(id),
        peer_id: peer,
        from_id: from,
        sender_name: if outgoing { "Taylor Kim" } else { name }.to_owned(),
        date_unix,
        outgoing,
        is_post: false,
        text,
        media,
        service: None,
        reply,
        forward: None,
        reactions,
        views: None,
        edited: n.is_multiple_of(13),
        is_read: true,
        is_sending: false,
        is_pinned: false,
    }
}

/// Appends the benchmark chats when `RUSTGRAM_BENCH_DATA` is set.
pub(super) fn extend_from_env(
    peers: &mut Vec<PeerRecord>,
    histories: &mut HashMap<PeerId, Vec<HistoryMessage>>,
) {
    let Some(size) = size_from_env() else {
        return;
    };
    let Some(user_template) = peers
        .iter()
        .find(|p| p.id == session_peers::USER_MIRA)
        .cloned()
    else {
        return;
    };
    let Some(group_template) = peers
        .iter()
        .find(|p| p.id == session_peers::GROUP_CORE)
        .cloned()
    else {
        return;
    };

    let mut group = group_template;
    group.id = BENCH_HISTORY_PEER;
    group.kind = InfoPeerKind::Supergroup;
    group.title = "Bench History".to_owned();
    group.initials = "BH".to_owned();
    group.usernames.clear();
    group.invite_link = None;
    group.is_forum = false;
    group.pinned = false;
    group.unread_count = 0;
    group.unread_mentions = 0;
    group.folders.clear();
    group.last_message_time = LocalDateTime::new(2026, 11, 22, 15, 0, 0);
    peers.push(group);
    let mut long = Vec::with_capacity(size.messages);
    for n in 0..size.messages {
        let id = n as i32 + 1;
        // About 40 messages a day, so day separators appear every screen or two.
        let date = BASE_UNIX - ((size.messages - n) as i32) * 2_160;
        long.push(message(BENCH_HISTORY_PEER, id, date, n));
    }
    histories.insert(BENCH_HISTORY_PEER, long);

    for i in 0..size.chats {
        let mut peer = user_template.clone();
        peer.id = PeerId::from_user(BENCH_USER_BASE + i as u64);
        let first = NAMES[i % NAMES.len()];
        peer.title = format!("{first} {i}");
        peer.initials = first.chars().take(1).collect::<String>() + &(i % 10).to_string();
        peer.color_index = (i % 7) as u8;
        peer.avatar_rgb = [
            (i * 41 % 256) as u8,
            (i * 97 % 256) as u8,
            (i * 13 % 256) as u8,
        ];
        peer.usernames.clear();
        peer.phone = None;
        peer.pinned = false;
        peer.folders.clear();
        peer.typing = None;
        peer.is_online = i % 9 == 0;
        peer.muted = i % 6 == 0;
        peer.unread_count = if i % 4 == 0 { (i % 30) as u32 + 1 } else { 0 };
        peer.unread_mentions = 0;
        let minutes_back = i as i64 * 47;
        let day = 21 - (minutes_back / (24 * 60)).min(20) as u8;
        let minute_of_day = 23 * 60 + 59 - (minutes_back % (24 * 60)) as u32;
        peer.last_message_time = LocalDateTime::new(
            2026,
            11,
            day,
            (minute_of_day / 60) as u8,
            (minute_of_day % 60) as u8,
            0,
        );
        let date = BASE_UNIX - (i as i32) * 2_820;
        histories.insert(
            peer.id,
            vec![
                message(peer.id, 1, date - 600, i + 1),
                message(peer.id, 2, date, i + 2),
            ],
        );
        peers.push(peer);
    }
}
