//! One-time, idempotent fill of the two test accounts. Every finished step is recorded in the
//! state file; a second run skips it, so running the fixture again adds nothing.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::ops::{self, Out};
use crate::side::{now, twe, u16len, updates_chats};
use crate::world::{World, seen};

/// Builds a text with entities, counting offsets in UTF-16 units.
#[derive(Default)]
struct Rich {
    text: String,
    ents: Vec<tl_enums::MessageEntity>,
}

impl Rich {
    fn plain(mut self, s: &str) -> Self {
        self.text.push_str(s);
        self
    }
    fn ent(mut self, s: &str, make: impl FnOnce(i32, i32) -> tl_enums::MessageEntity) -> Self {
        let o = u16len(&self.text);
        self.text.push_str(s);
        self.ents.push(make(o, u16len(s)));
        self
    }
}

const HISTORY_LINES: usize = 320;
const HISTORY_PREFIX: &str = "History line ";

const HISTORY_WORDS: &[&str] = &[
    "ok",
    "sounds good",
    "let me check the schedule and come back to you later today",
    "did you see the new photos from the trip? the mountains looked amazing in the morning light",
    "yes",
    "what time works for you tomorrow?",
    "I think we should test scrolling through a really long conversation with messages of very different lengths, because that is where layout bugs usually hide",
    "lunch?",
    "👍",
    "running late, ten minutes",
];

pub async fn run(w: &mut World, media: &Path) -> Result<(), String> {
    w.ensure_user_hashes().await?;
    contacts(w).await?;
    chats(w).await?;
    channel_posts(w, media).await?;
    supergroup(w, media).await?;
    basic_group(w).await?;
    stickers(w, media).await?;
    private_chat(w).await?;
    saved_and_scheduled(w).await?;
    list_state(w).await?;
    story(w, media).await?;
    custom_emoji(w).await?;
    reactions_retry(w).await?;
    w.state["finished_at"] = json!(now());
    w.save()?;
    eprintln!("[fixture] complete");
    Ok(())
}

async fn contacts(w: &mut World) -> Result<(), String> {
    if w.todo("contacts") {
        for (who, other, name) in [("a", "b", "Walk Peer B"), ("b", "a", "Walk Peer A")] {
            let side = w.side(who)?;
            side.call(
                "addContact",
                &tl_fns::contacts::AddContact {
                    add_phone_privacy_exception: false,
                    id: w.input_user(who, other)?,
                    first_name: name.to_owned(),
                    last_name: String::new(),
                    phone: String::new(),
                    note: None,
                },
            )
            .await?;
        }
        w.done("contacts")?;
    }
    Ok(())
}

/// Finds a chat A already has with this title, so a lost state file does not duplicate chats.
async fn find_titled_by(
    w: &World,
    who: &str,
    title: &str,
) -> Result<Option<tl_enums::Chat>, String> {
    let side = w.side(who)?;
    let mut all = seen(side, None).await?.chats;
    all.extend(seen(side, Some(1)).await?.chats);
    Ok(all.into_iter().find(|c| match c {
        tl_enums::Chat::Channel(c) => c.title == title && !c.left,
        _ => false,
    }))
}

async fn find_titled(w: &World, title: &str) -> Result<Option<tl_enums::Chat>, String> {
    let s = seen(&w.a, None).await?;
    let mut all = s.chats;
    all.extend(seen(&w.a, Some(1)).await?.chats);
    Ok(all.into_iter().find(|c| match c {
        tl_enums::Chat::Chat(c) => c.title == title && !c.left && !c.deactivated,
        tl_enums::Chat::Channel(c) => c.title == title && !c.left,
        _ => false,
    }))
}

async fn channel_hash_for(w: &World, who: &str, id: i64) -> Result<i64, String> {
    for _ in 0..10 {
        let side = w.side(who)?;
        let mut chats = seen(side, None).await?.chats;
        chats.extend(seen(side, Some(1)).await?.chats);
        for c in chats {
            if let tl_enums::Chat::Channel(c) = c
                && c.id == id
                && let Some(h) = c.access_hash
            {
                return Ok(h);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(format!("{who} does not see the channel it was invited to"))
}

async fn chats(w: &mut World) -> Result<(), String> {
    for (key, title) in [
        ("basic", "Walk Basic Group"),
        ("archived", "Walk Archived Group"),
    ] {
        if w.state["chats"][key]["id"].is_i64() {
            continue;
        }
        let id = match find_titled(w, title).await? {
            Some(tl_enums::Chat::Chat(c)) => c.id,
            _ => {
                let got =
                    w.a.call(
                        "createChat",
                        &tl_fns::messages::CreateChat {
                            users: vec![w.input_user("a", "b")?],
                            title: title.to_owned(),
                            ttl_period: None,
                        },
                    )
                    .await?;
                let tl_enums::messages::InvitedUsers::InvitedUsers(inv) = got;
                updates_chats(&inv.updates)
                    .into_iter()
                    .find_map(|c| match c {
                        tl_enums::Chat::Chat(c) => Some(c.id),
                        _ => None,
                    })
                    .ok_or("createChat returned no chat")?
            }
        };
        w.state["chats"][key] = json!({ "kind": "chat", "id": id, "title": title });
        w.save()?;
    }
    // Supergroup and channel are created by B: account A hit FLOOD_WAIT_61 on
    // channels.createChannel twice on 2026-11-27 (owner rule: stop on waits over 60 s).
    for (key, title, broadcast) in [
        ("super", "Walk Supergroup", false),
        ("channel", "Walk Channel", true),
    ] {
        if w.state["chats"][key]["hash_a"].is_i64() && w.state["chats"][key]["hash_b"].is_i64() {
            continue;
        }
        let (owner, other) = ("b", "a");
        let (id, hash) = match find_titled_by(w, owner, title).await? {
            Some(tl_enums::Chat::Channel(c)) => (c.id, c.access_hash.unwrap_or(0)),
            _ => {
                let upd = w
                    .side(owner)?
                    .call(
                        "createChannel",
                        &tl_fns::channels::CreateChannel {
                            broadcast,
                            megagroup: !broadcast,
                            for_import: false,
                            forum: false,
                            title: title.to_owned(),
                            about: "Created by the Rustgram scenario walk fixture.".to_owned(),
                            geo_point: None,
                            address: None,
                            ttl_period: None,
                        },
                    )
                    .await?;
                updates_chats(&upd)
                    .into_iter()
                    .find_map(|c| match c {
                        tl_enums::Chat::Channel(c) => Some((c.id, c.access_hash.unwrap_or(0))),
                        _ => None,
                    })
                    .ok_or("createChannel returned no channel")?
            }
        };
        w.state["chats"][key] =
            json!({ "kind": "channel", "id": id, "title": title, "owner": owner });
        w.state["chats"][key][format!("hash_{owner}")] = json!(hash);
        w.save()?;
        let req = tl_fns::channels::InviteToChannel {
            channel: w.input_channel(owner, key)?,
            users: vec![w.input_user(owner, other)?],
        };
        if let Err(e) = w.side(owner)?.call("inviteToChannel", &req).await
            && !e.contains("USER_ALREADY_PARTICIPANT")
        {
            return Err(e);
        }
        let h = channel_hash_for(w, other, id).await?;
        w.state["chats"][key][format!("hash_{other}")] = json!(h);
        w.save()?;
    }
    Ok(())
}

/// The account that owns a channel the fixture made (posts and pins come from it).
fn owner_of(w: &World, key: &str) -> String {
    w.state["chats"][key]["owner"]
        .as_str()
        .unwrap_or("a")
        .to_owned()
}

fn media_path(media: &Path, name: &str) -> std::path::PathBuf {
    media.join(name)
}

async fn photo(
    w: &World,
    who: &str,
    media: &Path,
    name: &str,
) -> Result<tl_enums::InputMedia, String> {
    let f = ops::upload(w.side(who)?, &media_path(media, name)).await?;
    Ok(ops::uploaded_photo(f))
}

async fn channel_posts(w: &mut World, media: &Path) -> Result<(), String> {
    let owner = owner_of(w, "channel");
    let reader = if owner == "a" { "b" } else { "a" };
    let peer = w.peer(&owner, "channel")?;
    if w.todo("channel.posts") {
        let p1 = ops::send_text(
            w.side(&owner)?,
            &peer,
            "Welcome to the Walk Channel. Posts here test views, reactions and forwards.",
            &Out::default(),
        )
        .await?;
        w.set_msg("channel.post_text", p1)?;
        let m = photo(w, &owner, media, "channel.jpg").await?;
        let p2 = ops::send_media(
            w.side(&owner)?,
            &peer,
            m,
            "A photo post with a caption.",
            &Out::default(),
        )
        .await?;
        w.set_msg("channel.post_photo", p2)?;
        let p3 = ops::send_text(
            w.side(&owner)?,
            &peer,
            "Read more: https://telegram.org/blog",
            &Out::default(),
        )
        .await?;
        w.set_msg("channel.post_link", p3)?;
        w.done("channel.posts")?;
    }
    if w.todo("channel.reactions") {
        let pb = w.peer(reader, "channel")?;
        let rs = w.side(reader)?;
        let mut notes = Vec::new();
        for (key, emoji) in [("channel.post_text", "👍"), ("channel.post_photo", "🔥")] {
            if let Some(id) = w.msg(key)
                && let Err(e) = ops::react(rs, &pb, id, emoji).await
            {
                notes.push(e);
            }
        }
        if let Some(id) = w.msg("channel.post_photo")
            && let Err(e) = ops::react(w.side(&owner)?, &peer, id, "❤").await
        {
            notes.push(e);
        }
        if !notes.is_empty() {
            w.note("channel_reactions", &notes.join("; "))?;
        }
        w.done("channel.reactions")?;
    }
    Ok(())
}

fn mention(
    w: &World,
    who_sees: &str,
    target: &str,
    o: i32,
    l: i32,
) -> Result<tl_enums::MessageEntity, String> {
    Ok(tl_enums::MessageEntity::InputMessageEntityMentionName(
        tl_types::InputMessageEntityMentionName {
            offset: o,
            length: l,
            user_id: w.input_user(who_sees, target)?,
        },
    ))
}

async fn supergroup(w: &mut World, media: &Path) -> Result<(), String> {
    let pa = w.peer("a", "super")?;
    let pb = w.peer("b", "super")?;
    if w.todo("super.text") {
        let welcome = ops::send_text(
            &w.b,
            &pb,
            "Hello everyone! This is the walk supergroup, we test everyday things here.",
            &Out::default(),
        )
        .await?;
        w.set_msg("super.welcome", welcome)?;
        for (i, t) in [
            "Hi! Glad to be here.",
            "I will post a few things to test grouping of my own messages.",
            "And a third one, so the userpic should sit next to the last of them.",
        ]
        .iter()
        .enumerate()
        {
            let id = ops::send_text(&w.a, &pa, t, &Out::default()).await?;
            w.set_msg(&format!("super.a_run{i}"), id)?;
        }
        let r = ops::send_text(
            &w.a,
            &pa,
            "Replying to the welcome message.",
            &Out {
                reply_to: Some(welcome),
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("super.a_reply", r)?;
        let rich = Rich::default()
            .plain("Styles: ")
            .ent("bold", ops::ent_bold)
            .plain(", ")
            .ent("italic", |o, l| {
                tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
                    offset: o,
                    length: l,
                })
            })
            .plain(", ")
            .ent("underline", |o, l| {
                tl_enums::MessageEntity::Underline(tl_types::MessageEntityUnderline {
                    offset: o,
                    length: l,
                })
            })
            .plain(", ")
            .ent("strikethrough", |o, l| {
                tl_enums::MessageEntity::Strike(tl_types::MessageEntityStrike {
                    offset: o,
                    length: l,
                })
            })
            .plain(", ")
            .ent("monospace", |o, l| {
                tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
                    offset: o,
                    length: l,
                })
            })
            .plain(", ")
            .ent("a text link", |o, l| {
                tl_enums::MessageEntity::TextUrl(tl_types::MessageEntityTextUrl {
                    offset: o,
                    length: l,
                    url: "https://telegram.org/".to_owned(),
                })
            })
            .plain(" and a hidden ")
            .ent("spoiler text here", |o, l| {
                tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler {
                    offset: o,
                    length: l,
                })
            })
            .plain(".\n")
            .ent("A quoted line inside the message.", |o, l| {
                tl_enums::MessageEntity::Blockquote(tl_types::MessageEntityBlockquote {
                    collapsed: false,
                    offset: o,
                    length: l,
                })
            })
            .plain("\n")
            .ent("fn main() {\n    println!(\"walk\");\n}", |o, l| {
                tl_enums::MessageEntity::Pre(tl_types::MessageEntityPre {
                    offset: o,
                    length: l,
                    language: "rust".to_owned(),
                })
            });
        let id = ops::send_text(
            &w.b,
            &pb,
            &rich.text,
            &Out {
                entities: rich.ents,
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("super.styles", id)?;
        let id = ops::send_text(
            &w.a,
            &pa,
            "Link with a preview: https://telegram.org/blog",
            &Out::default(),
        )
        .await?;
        w.set_msg("super.link", id)?;
        w.done("super.text")?;
    }
    if w.todo("super.mentions") {
        let t = "Walker, could you look at the album later?";
        let id = ops::send_text(
            &w.b,
            &pb,
            t,
            &Out {
                entities: vec![mention(w, "b", "a", 0, 6)?],
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("super.mention_of_a", id)?;
        let t = "Peer, the poll is waiting for your vote.";
        let id = ops::send_text(
            &w.a,
            &pa,
            t,
            &Out {
                entities: vec![mention(w, "a", "b", 0, 4)?],
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("super.mention_of_b", id)?;
        w.done("super.mentions")?;
    }
    if w.todo("super.album") {
        let mut items = Vec::new();
        for n in ["album1.jpg", "album2.jpg", "album3.jpg"] {
            let m = photo(w, "b", media, n).await?;
            items.push(ops::upload_media(&w.b, &pb, m).await?);
        }
        let ids = ops::send_album(&w.b, &pb, items, "Three photos from the trip").await?;
        if let Some(id) = ids.first() {
            w.set_msg("super.album", *id)?;
        }
        w.done("super.album")?;
    }
    if w.todo("super.tall") {
        let m = photo(w, "a", media, "tall.jpg").await?;
        let id = ops::send_media(&w.a, &pa, m, "", &Out::default()).await?;
        w.set_msg("super.tall_photo", id)?;
        w.done("super.tall")?;
    }
    if w.todo("super.video") {
        let f = ops::upload(&w.b, &media_path(media, "video.mp4")).await?;
        let m = ops::uploaded_doc(
            f,
            "video/mp4",
            vec![
                ops::attr_video(false, 6.0, 640, 360),
                ops::attr_filename("video.mp4"),
            ],
            false,
            false,
        );
        let id = ops::send_media(&w.b, &pb, m, "A short video", &Out::default()).await?;
        w.set_msg("super.video", id)?;
        w.done("super.video")?;
    }
    if w.todo("super.gif") {
        let f = ops::upload(&w.a, &media_path(media, "gif.mp4")).await?;
        let m = ops::uploaded_doc(
            f,
            "video/mp4",
            vec![
                ops::attr_video(false, 3.0, 480, 270),
                tl_enums::DocumentAttribute::Animated,
                ops::attr_filename("gif.mp4"),
            ],
            false,
            true,
        );
        let id = ops::send_media(&w.a, &pa, m, "", &Out::default()).await?;
        w.set_msg("super.gif", id)?;
        w.done("super.gif")?;
    }
    if w.todo("super.voice") {
        let f = ops::upload(&w.b, &media_path(media, "voice.ogg")).await?;
        let waveform: Vec<u8> = (0..63u32).map(|i| ((i * 37) % 251) as u8).collect();
        let m = ops::uploaded_doc(
            f,
            "audio/ogg",
            vec![tl_enums::DocumentAttribute::Audio(
                tl_types::DocumentAttributeAudio {
                    voice: true,
                    duration: 4,
                    title: None,
                    performer: None,
                    waveform: Some(waveform),
                },
            )],
            false,
            false,
        );
        let id = ops::send_media(&w.b, &pb, m, "", &Out::default()).await?;
        w.set_msg("super.voice", id)?;
        w.done("super.voice")?;
    }
    if w.todo("super.round") {
        let f = ops::upload(&w.a, &media_path(media, "round.mp4")).await?;
        let m = ops::uploaded_doc(
            f,
            "video/mp4",
            vec![ops::attr_video(true, 4.0, 240, 240)],
            false,
            false,
        );
        let id = ops::send_media(&w.a, &pa, m, "", &Out::default()).await?;
        w.set_msg("super.round", id)?;
        w.done("super.round")?;
    }
    if w.todo("super.file") {
        let f = ops::upload(&w.b, &media_path(media, "walk-notes.txt")).await?;
        let m = ops::uploaded_doc(
            f,
            "text/plain",
            vec![ops::attr_filename("walk-notes.txt")],
            true,
            false,
        );
        let id = ops::send_media(&w.b, &pb, m, "Notes as a file", &Out::default()).await?;
        w.set_msg("super.file", id)?;
        w.done("super.file")?;
    }
    if w.todo("super.poll") {
        let answers = ["Search", "Media viewer", "Stickers"]
            .iter()
            .map(|t| {
                tl_enums::PollAnswer::InputPollAnswer(tl_types::InputPollAnswer {
                    text: twe(t),
                    media: None,
                })
            })
            .collect();
        let poll = tl_enums::InputMedia::Poll(tl_types::InputMediaPoll {
            poll: tl_enums::Poll::Poll(tl_types::Poll {
                id: 0,
                closed: false,
                public_voters: false,
                multiple_choice: false,
                quiz: false,
                open_answers: false,
                revoting_disabled: false,
                shuffle_answers: false,
                hide_results_until_close: false,
                creator: false,
                subscribers_only: false,
                question: twe("What should we test next?"),
                answers,
                close_period: None,
                close_date: None,
                countries_iso2: None,
                hash: 0,
            }),
            correct_answers: None,
            attached_media: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        });
        let id = ops::send_media(&w.b, &pb, poll, "", &Out::default()).await?;
        w.set_msg("super.poll", id)?;
        w.done("super.poll")?;
    }
    if w.todo("super.forward")
        && let Some(post) = w.msg("channel.post_text")
    {
        let upd =
            w.b.call(
                "forwardMessages",
                &tl_fns::messages::ForwardMessages {
                    silent: false,
                    background: false,
                    with_my_score: false,
                    drop_author: false,
                    drop_media_captions: false,
                    noforwards: false,
                    allow_paid_floodskip: false,
                    from_ephemeral: false,
                    from_peer: w.peer("b", "channel")?,
                    id: vec![post],
                    random_id: vec![w.b.random_id()],
                    to_peer: pb.clone(),
                    top_msg_id: None,
                    reply_to: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    video_timestamp: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                },
            )
            .await?;
        if let Some(id) = crate::side::msg_ids(&upd).last() {
            w.set_msg("super.forwarded", *id)?;
        }
        w.done("super.forward")?;
    }
    if w.todo("super.pin")
        && let Some(id) = w.msg("super.welcome")
    {
        let owner = owner_of(w, "super");
        w.side(&owner)?
            .call(
                "updatePinnedMessage",
                &tl_fns::messages::UpdatePinnedMessage {
                    silent: true,
                    unpin: false,
                    pm_oneside: false,
                    peer: w.peer(&owner, "super")?,
                    id,
                },
            )
            .await?;
        w.done("super.pin")?;
    }
    if w.todo("super.reactions") {
        let mut notes = Vec::new();
        let plan = [
            ("a", "super.welcome", "👍"),
            ("b", "super.a_run0", "❤"),
            ("b", "super.a_run2", "😁"),
            ("a", "super.album", "🔥"),
            ("b", "super.tall_photo", "👍"),
        ];
        for (who, key, emoji) in plan {
            if let Some(id) = w.msg(key) {
                let peer = w.peer(who, "super")?;
                if let Err(e) = ops::react(w.side(who)?, &peer, id, emoji).await {
                    notes.push(e);
                }
            }
        }
        if !notes.is_empty() {
            w.note("super_reactions", &notes.join("; "))?;
        }
        w.done("super.reactions")?;
    }
    if w.todo("super.closing") {
        let id =
            ops::send_text(&w.b, &pb, "That is all for now, see you!", &Out::default()).await?;
        w.set_msg("super.last_b", id)?;
        w.done("super.closing")?;
    }
    Ok(())
}

async fn basic_group(w: &mut World) -> Result<(), String> {
    let pa = w.peer("a", "basic")?;
    let pb = w.peer("b", "basic")?;
    if w.todo("basic.text") {
        let mut last_a = 0;
        for (i, t) in ["Morning!", "Who is coming today?", "I will bring snacks."]
            .iter()
            .enumerate()
        {
            last_a = ops::send_text(&w.a, &pa, t, &Out::default()).await?;
            w.set_msg(&format!("basic.a_run{i}"), last_a)?;
        }
        let id = ops::send_text(
            &w.b,
            &pb,
            "Count me in.",
            &Out {
                reply_to: Some(last_a),
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("basic.b_reply", id)?;
        let id = ops::send_text(
            &w.b,
            &pb,
            "Walker, see the plan above.",
            &Out {
                entities: vec![mention(w, "b", "a", 0, 6)?],
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("basic.mention_of_a", id)?;
        let id = ops::send_text(
            &w.a,
            &pa,
            "Peer, thanks!",
            &Out {
                entities: vec![mention(w, "a", "b", 0, 4)?],
                ..Out::default()
            },
        )
        .await?;
        w.set_msg("basic.mention_of_b", id)?;
        w.done("basic.text")?;
    }
    let ar = w.peer("a", "archived")?;
    if w.todo("archived.text") {
        ops::send_text(
            &w.b,
            &w.peer("b", "archived")?,
            "An old chat that lives in the archive.",
            &Out::default(),
        )
        .await?;
        ops::send_text(&w.a, &ar, "Archiving this one.", &Out::default()).await?;
        w.done("archived.text")?;
    }
    Ok(())
}

async fn stickers(w: &mut World, media: &Path) -> Result<(), String> {
    let pa = w.peer("a", "super")?;
    // A custom (user-made) sticker set, like the ones the owner's sticker bug is about.
    if w.todo("stickers.custom_set") {
        let mut items = Vec::new();
        let mut err = None;
        for (n, emoji) in [("sticker1.png", "😀"), ("sticker2.png", "🎉")] {
            let f = ops::upload(&w.a, &media_path(media, n)).await?;
            let m = ops::uploaded_doc(f, "image/png", vec![ops::attr_filename(n)], true, false);
            match ops::upload_media(&w.a, &tl_enums::InputPeer::Self_, m).await {
                Ok(tl_enums::InputMedia::Document(d)) => {
                    items.push(tl_enums::InputStickerSetItem::InputStickerSetItem(
                        tl_types::InputStickerSetItem {
                            document: d.id,
                            emoji: emoji.to_owned(),
                            mask_coords: None,
                            keywords: None,
                        },
                    ));
                }
                Ok(_) => err = Some("uploadMedia did not return a document".to_owned()),
                Err(e) => err = Some(e),
            }
        }
        if err.is_none() {
            let short = format!("walk_set_{}", (w.a.random_id() as u64) % 1_000_000_000);
            match w
                .a
                .call(
                    "createStickerSet",
                    &tl_fns::stickers::CreateStickerSet {
                        masks: false,
                        emojis: false,
                        text_color: false,
                        user_id: tl_enums::InputUser::Self_,
                        title: "Walk Stickers".to_owned(),
                        short_name: short.clone(),
                        thumb: None,
                        stickers: items,
                        software: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    w.state["stickers"]["custom_set"] = json!(short);
                    w.save()?;
                }
                Err(e) => err = Some(e),
            }
        }
        if let Some(e) = err {
            w.note("custom_sticker_set", &format!("not created: {e}"))?;
        }
        w.done("stickers.custom_set")?;
    }
    if w.todo("stickers.send_custom")
        && let Some(short) = w.state["stickers"]["custom_set"]
            .as_str()
            .map(str::to_owned)
    {
        let set = get_set(
            w,
            tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: short,
            }),
        )
        .await?;
        if let Some(doc) = set.first() {
            let id = ops::send_media(
                &w.b,
                &w.peer("b", "super")?,
                ops::input_doc_media(doc),
                "",
                &Out::default(),
            )
            .await?;
            w.set_msg("super.custom_sticker", id)?;
        }
        w.done("stickers.send_custom")?;
    }
    // A sticker from a regular set the test server offers.
    if w.todo("stickers.regular") {
        let mut docs = Vec::new();
        if let Ok(tl_enums::messages::FeaturedStickers::FeaturedStickers(f)) =
            w.a.call(
                "getFeaturedStickers",
                &tl_fns::messages::GetFeaturedStickers { hash: 0 },
            )
            .await
        {
            for covered in f.sets {
                let set = match covered {
                    tl_enums::StickerSetCovered::StickerSetCovered(c) => c.set,
                    tl_enums::StickerSetCovered::StickerSetMultiCovered(c) => c.set,
                    tl_enums::StickerSetCovered::StickerSetFullCovered(c) => c.set,
                    tl_enums::StickerSetCovered::StickerSetNoCovered(c) => c.set,
                };
                let tl_enums::StickerSet::StickerSet(s) = set;
                if s.emojis {
                    continue;
                }
                docs = get_set(
                    w,
                    tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                        id: s.id,
                        access_hash: s.access_hash,
                    }),
                )
                .await
                .unwrap_or_default();
                if !docs.is_empty() {
                    w.state["stickers"]["regular_set"] = json!(s.title);
                    break;
                }
            }
        }
        if docs.is_empty() {
            docs = get_set(w, tl_enums::InputStickerSet::AnimatedEmoji)
                .await
                .unwrap_or_default();
            if !docs.is_empty() {
                w.note(
                    "regular_stickers",
                    "no featured sets on the test server; used the animated emoji set",
                )?;
            }
        }
        if let Some(doc) = docs.first() {
            let id =
                ops::send_media(&w.a, &pa, ops::input_doc_media(doc), "", &Out::default()).await?;
            w.set_msg("super.sticker", id)?;
        } else {
            w.note(
                "regular_stickers",
                "no sticker set available on the test server",
            )?;
        }
        w.done("stickers.regular")?;
    }
    Ok(())
}

async fn get_set(
    w: &World,
    set: tl_enums::InputStickerSet,
) -> Result<Vec<tl_types::Document>, String> {
    match w
        .a
        .call(
            "getStickerSet",
            &tl_fns::messages::GetStickerSet {
                stickerset: set,
                hash: 0,
            },
        )
        .await?
    {
        tl_enums::messages::StickerSet::StickerSet(s) => Ok(s
            .documents
            .into_iter()
            .filter_map(|d| match d {
                tl_enums::Document::Document(d) => Some(d),
                tl_enums::Document::Empty(_) => None,
            })
            .collect()),
        tl_enums::messages::StickerSet::NotModified => Ok(Vec::new()),
    }
}

async fn private_chat(w: &mut World) -> Result<(), String> {
    let pa = w.peer("a", "pm")?;
    let pb = w.peer("b", "pm")?;
    if w.todo("pm.history") {
        let existing = ops::history_all(&w.a, &pa, 2000).await?;
        let have: std::collections::HashSet<usize> = existing
            .iter()
            .filter_map(|m| m.message.strip_prefix(HISTORY_PREFIX))
            .filter_map(|rest| rest.split(':').next()?.trim().parse().ok())
            .collect();
        for i in 1..=HISTORY_LINES {
            if have.contains(&i) {
                continue;
            }
            let (side, peer) = if i % 3 == 0 { (&w.b, &pb) } else { (&w.a, &pa) };
            let text = format!(
                "{HISTORY_PREFIX}{i:03}: {}",
                HISTORY_WORDS[(i * 7) % HISTORY_WORDS.len()]
            );
            ops::send_text(
                side,
                peer,
                &text,
                &Out {
                    silent: true,
                    ..Out::default()
                },
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(350)).await;
            if i % 50 == 0 {
                eprintln!("[fixture] history {i}/{HISTORY_LINES}");
            }
        }
        w.done("pm.history")?;
    }
    if w.todo("pm.old_reactions") {
        let all = ops::history_all(&w.a, &pa, 2000).await?;
        let mut notes = Vec::new();
        for (n, who, emoji) in [
            (2usize, "b", "👍"),
            (3, "a", "❤"),
            (10, "b", "🔥"),
            (40, "a", "😁"),
        ] {
            let key = format!("{HISTORY_PREFIX}{n:03}:");
            if let Some(m) = all.iter().find(|m| m.message.starts_with(&key)) {
                let peer = w.peer(who, "pm")?;
                if let Err(e) = ops::react(w.side(who)?, &peer, m.id, emoji).await {
                    notes.push(e);
                }
                w.set_msg(&format!("pm.history_{n:03}"), m.id)?;
            }
        }
        if !notes.is_empty() {
            w.note("pm_reactions", &notes.join("; "))?;
        }
        w.done("pm.old_reactions")?;
    }
    if w.todo("pm.tail") {
        let id = ops::send_text(
            &w.b,
            &pb,
            "Latest message from Peer B in our private chat.",
            &Out::default(),
        )
        .await?;
        w.set_msg("pm.last_b", id)?;
        w.done("pm.tail")?;
    }
    Ok(())
}

async fn saved_and_scheduled(w: &mut World) -> Result<(), String> {
    if w.todo("saved") {
        let id = ops::send_text(
            &w.a,
            &tl_enums::InputPeer::Self_,
            "Walk saved note: shopping list, milk, bread, coffee.",
            &Out::default(),
        )
        .await?;
        w.set_msg("saved.note", id)?;
        w.done("saved")?;
    }
    if w.todo("scheduled") {
        let pa = w.peer("a", "pm")?;
        let existing =
            w.a.call(
                "getScheduledHistory",
                &tl_fns::messages::GetScheduledHistory {
                    peer: pa.clone(),
                    hash: 0,
                },
            )
            .await?;
        let has = match existing {
            tl_enums::messages::Messages::Messages(m) => !m.messages.is_empty(),
            tl_enums::messages::Messages::Slice(m) => !m.messages.is_empty(),
            tl_enums::messages::Messages::ChannelMessages(m) => !m.messages.is_empty(),
            tl_enums::messages::Messages::NotModified(_) => false,
        };
        if !has {
            ops::send_text(
                &w.a,
                &pa,
                "Walk scheduled message: happy new year!",
                &Out {
                    schedule: Some(now() + 200 * 24 * 3600),
                    ..Out::default()
                },
            )
            .await?;
        }
        w.done("scheduled")?;
    }
    Ok(())
}

/// Folder, archive, mute, pin and draft: chat-list state of account A.
async fn list_state(w: &mut World) -> Result<(), String> {
    if w.todo("folder") {
        let tl_enums::messages::DialogFilters::DialogFilters(f) =
            w.a.call("getDialogFilters", &tl_fns::messages::GetDialogFilters)
                .await?;
        let mut used = std::collections::HashSet::new();
        let mut existing = None;
        for d in &f.filters {
            if let tl_enums::DialogFilter::DialogFilter(d) = d {
                used.insert(d.id);
                let tl_enums::TextWithEntities::TextWithEntities(t) = &d.title;
                if t.text == "Walk" {
                    existing = Some(d.id);
                }
            }
        }
        let id = existing.unwrap_or_else(|| (2..255).find(|i| !used.contains(i)).unwrap_or(2));
        w.a.call(
            "updateDialogFilter",
            &tl_fns::messages::UpdateDialogFilter {
                id,
                filter: Some(tl_enums::DialogFilter::DialogFilter(
                    tl_types::DialogFilter {
                        contacts: false,
                        non_contacts: false,
                        groups: false,
                        broadcasts: false,
                        bots: false,
                        exclude_muted: false,
                        exclude_read: false,
                        exclude_archived: false,
                        title_noanimate: false,
                        id,
                        title: twe("Walk"),
                        emoticon: None,
                        color: None,
                        pinned_peers: Vec::new(),
                        include_peers: vec![
                            w.peer("a", "super")?,
                            w.peer("a", "basic")?,
                            w.peer("a", "channel")?,
                        ],
                        exclude_peers: Vec::new(),
                    },
                )),
            },
        )
        .await?;
        w.state["folder_id"] = json!(id);
        w.done("folder")?;
    }
    if w.todo("archive") {
        w.a.call(
            "editPeerFolders",
            &tl_fns::folders::EditPeerFolders {
                folder_peers: vec![tl_enums::InputFolderPeer::InputFolderPeer(
                    tl_types::InputFolderPeer {
                        peer: w.peer("a", "archived")?,
                        folder_id: 1,
                    },
                )],
            },
        )
        .await?;
        w.done("archive")?;
    }
    if w.todo("mute") {
        w.a.call(
            "updateNotifySettings",
            &tl_fns::account::UpdateNotifySettings {
                peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: w.peer("a", "channel")?,
                }),
                settings: tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
                    tl_types::InputPeerNotifySettings {
                        show_previews: None,
                        silent: None,
                        mute_until: Some(i32::MAX),
                        sound: None,
                        stories_muted: None,
                        stories_hide_sender: None,
                        stories_sound: None,
                    },
                ),
            },
        )
        .await?;
        w.done("mute")?;
    }
    if w.todo("pin_dialog") {
        w.a.call(
            "toggleDialogPin",
            &tl_fns::messages::ToggleDialogPin {
                pinned: true,
                peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                    peer: w.peer("a", "super")?,
                }),
            },
        )
        .await?;
        w.done("pin_dialog")?;
    }
    save_draft(w).await?;
    Ok(())
}

/// The draft is restored on every run (the walk may send or clear it).
pub async fn save_draft(w: &mut World) -> Result<(), String> {
    w.a.call(
        "saveDraft",
        &tl_fns::messages::SaveDraft {
            no_webpage: false,
            invert_media: false,
            reply_to: None,
            peer: w.peer("a", "basic")?,
            message: "Walk draft: remember to bring the charger".to_owned(),
            entities: None,
            media: None,
            effect: None,
            suggested_post: None,
            rich_message: None,
        },
    )
    .await?;
    Ok(())
}

/// B posts a story when none is active (stories expire after 24 h, so this runs every time).
pub async fn story(w: &mut World, media: &Path) -> Result<(), String> {
    let active =
        w.a.call(
            "getPeerStories",
            &tl_fns::stories::GetPeerStories {
                peer: w.peer("a", "pm")?,
            },
        )
        .await;
    if let Ok(tl_enums::stories::PeerStories::PeerStories(p)) = &active {
        let tl_enums::PeerStories::PeerStories(ps) = &p.stories;
        if !ps.stories.is_empty() {
            return Ok(());
        }
    }
    match w
        .b
        .call(
            "canSendStory",
            &tl_fns::stories::CanSendStory {
                peer: tl_enums::InputPeer::Self_,
            },
        )
        .await
    {
        Ok(_) => {}
        Err(e) => return w.note("story", &format!("test server refused stories: {e}")),
    }
    let f = ops::upload(&w.b, &media_path(media, "story.jpg")).await?;
    let res =
        w.b.call(
            "sendStory",
            &tl_fns::stories::SendStory {
                pinned: false,
                noforwards: false,
                fwd_modified: false,
                peer: tl_enums::InputPeer::Self_,
                media: ops::uploaded_photo(f),
                media_areas: None,
                caption: Some("Walk story".to_owned()),
                entities: None,
                privacy_rules: vec![tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll],
                random_id: w.b.random_id(),
                period: None,
                fwd_from_id: None,
                fwd_from_story: None,
                albums: None,
                music: None,
            },
        )
        .await;
    match res {
        Ok(_) => {
            w.state["story_posted_at"] = json!(now());
            w.save()
        }
        Err(e) => w.note("story", &format!("sendStory failed: {e}")),
    }
}

async fn custom_emoji(w: &mut World) -> Result<(), String> {
    if !w.todo("custom_emoji") {
        return Ok(());
    }
    if w.a.premium || w.b.premium {
        let sender = if w.b.premium { "b" } else { "a" };
        let docs = get_set(w, tl_enums::InputStickerSet::EmojiDefaultStatuses).await?;
        if let Some(doc) = docs.first() {
            let peer = w.peer(sender, "super")?;
            let t = "Custom emoji: 🙂";
            let o = u16len("Custom emoji: ");
            let ent = tl_enums::MessageEntity::CustomEmoji(tl_types::MessageEntityCustomEmoji {
                offset: o,
                length: u16len("🙂"),
                document_id: doc.id,
            });
            let id = ops::send_text(
                w.side(sender)?,
                &peer,
                t,
                &Out {
                    entities: vec![ent],
                    ..Out::default()
                },
            )
            .await?;
            w.set_msg("super.custom_emoji", id)?;
        }
    } else {
        w.note(
            "custom_emoji",
            "neither test account has Premium, so custom emoji in text cannot be sent (the server strips them)",
        )?;
    }
    w.done("custom_emoji")
}

/// The test servers refused some emoji with REACTION_INVALID; these use the two that worked.
async fn reactions_retry(w: &mut World) -> Result<(), String> {
    if !w.todo("reactions.retry") {
        return Ok(());
    }
    let plan = [
        ("a", "pm", "pm.history_003", "🔥"),
        ("a", "pm", "pm.history_040", "👍"),
        ("b", "super", "super.a_run0", "🔥"),
        ("b", "super", "super.a_run2", "👍"),
    ];
    let mut notes = Vec::new();
    for (who, chat, key, emoji) in plan {
        if let Some(id) = w.msg(key) {
            let peer = w.peer(who, chat)?;
            if let Err(e) = ops::react(w.side(who)?, &peer, id, emoji).await {
                notes.push(e);
            }
        }
    }
    if notes.is_empty() {
        w.note("pm_reactions", "test servers refused heart and grin reactions (REACTION_INVALID); used fire and thumbs up")?;
        w.note("super_reactions", "test servers refused heart and grin reactions (REACTION_INVALID); used fire and thumbs up")?;
    } else {
        w.note("reactions_retry", &notes.join("; "))?;
    }
    w.done("reactions.retry")
}
