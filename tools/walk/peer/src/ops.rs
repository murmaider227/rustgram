//! Thin wrappers over the raw TL calls the fixture and the walk need.

use std::path::Path;

use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::side::{Side, msg_ids};

/// Options for one outgoing text or media message.
#[derive(Default, Clone)]
pub struct Out {
    pub entities: Vec<tl_enums::MessageEntity>,
    pub reply_to: Option<i32>,
    pub schedule: Option<i32>,
    pub no_webpage: bool,
    pub silent: bool,
}

fn reply(reply_to: Option<i32>) -> Option<tl_enums::InputReplyTo> {
    reply_to.map(|id| {
        tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
            reply_to_msg_id: id,
            top_msg_id: None,
            reply_to_peer_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            monoforum_peer_id: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        })
    })
}

fn ents(e: &[tl_enums::MessageEntity]) -> Option<Vec<tl_enums::MessageEntity>> {
    (!e.is_empty()).then(|| e.to_vec())
}

pub async fn send_text(
    side: &Side,
    peer: &tl_enums::InputPeer,
    text: &str,
    out: &Out,
) -> Result<i32, String> {
    let upd = side
        .call(
            "sendMessage",
            &tl_fns::messages::SendMessage {
                no_webpage: out.no_webpage,
                silent: out.silent,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: peer.clone(),
                reply_to: reply(out.reply_to),
                message: text.to_owned(),
                random_id: side.random_id(),
                reply_markup: None,
                entities: ents(&out.entities),
                schedule_date: out.schedule,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: None,
            },
        )
        .await?;
    msg_ids(&upd)
        .last()
        .copied()
        .ok_or_else(|| "sendMessage returned no id".to_owned())
}

pub async fn send_media(
    side: &Side,
    peer: &tl_enums::InputPeer,
    media: tl_enums::InputMedia,
    caption: &str,
    out: &Out,
) -> Result<i32, String> {
    let upd = side
        .call(
            "sendMedia",
            &tl_fns::messages::SendMedia {
                silent: out.silent,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: peer.clone(),
                reply_to: reply(out.reply_to),
                media,
                message: caption.to_owned(),
                random_id: side.random_id(),
                reply_markup: None,
                entities: ents(&out.entities),
                schedule_date: out.schedule,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            },
        )
        .await?;
    msg_ids(&upd)
        .last()
        .copied()
        .ok_or_else(|| "sendMedia returned no id".to_owned())
}

/// Uploads a local file in 512 KB parts and returns the `InputFile` that refers to it.
pub async fn upload(side: &Side, path: &Path) -> Result<tl_enums::InputFile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read media file: {e}"))?;
    const PART: usize = 512 * 1024;
    let file_id = side.random_id();
    let parts = bytes.len().div_ceil(PART).max(1);
    let big = bytes.len() > 10 * 1024 * 1024;
    for (i, chunk) in bytes.chunks(PART).enumerate() {
        let part = i32::try_from(i).map_err(|_| "file too big")?;
        let total = i32::try_from(parts).map_err(|_| "file too big")?;
        if big {
            side.call(
                "saveBigFilePart",
                &tl_fns::upload::SaveBigFilePart {
                    file_id,
                    file_part: part,
                    file_total_parts: total,
                    bytes: chunk.to_vec(),
                },
            )
            .await?;
        } else {
            side.call(
                "saveFilePart",
                &tl_fns::upload::SaveFilePart {
                    file_id,
                    file_part: part,
                    bytes: chunk.to_vec(),
                },
            )
            .await?;
        }
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let parts = i32::try_from(parts).map_err(|_| "file too big")?;
    Ok(if big {
        tl_enums::InputFile::Big(tl_types::InputFileBig {
            id: file_id,
            parts,
            name,
        })
    } else {
        tl_enums::InputFile::InputFile(tl_types::InputFile {
            id: file_id,
            parts,
            name,
            md5_checksum: String::new(),
        })
    })
}

pub fn uploaded_photo(file: tl_enums::InputFile) -> tl_enums::InputMedia {
    tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
        spoiler: false,
        live_photo: false,
        file,
        stickers: None,
        ttl_seconds: None,
        video: None,
    })
}

pub fn uploaded_doc(
    file: tl_enums::InputFile,
    mime: &str,
    attributes: Vec<tl_enums::DocumentAttribute>,
    force_file: bool,
    nosound: bool,
) -> tl_enums::InputMedia {
    tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
        nosound_video: nosound,
        force_file,
        spoiler: false,
        file,
        thumb: None,
        mime_type: mime.to_owned(),
        attributes,
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    })
}

pub fn attr_filename(name: &str) -> tl_enums::DocumentAttribute {
    tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
        file_name: name.to_owned(),
    })
}

pub fn attr_video(round: bool, secs: f64, w: i32, h: i32) -> tl_enums::DocumentAttribute {
    tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
        round_message: round,
        supports_streaming: true,
        nosound: false,
        duration: secs,
        w,
        h,
        preload_prefix_size: None,
        video_start_ts: None,
        video_codec: None,
    })
}

/// Turns an uploaded media into a server media reference (needed for albums).
pub async fn upload_media(
    side: &Side,
    peer: &tl_enums::InputPeer,
    media: tl_enums::InputMedia,
) -> Result<tl_enums::InputMedia, String> {
    let got = side
        .call(
            "uploadMedia",
            &tl_fns::messages::UploadMedia {
                business_connection_id: None,
                peer: peer.clone(),
                media,
            },
        )
        .await?;
    match got {
        tl_enums::MessageMedia::Photo(p) => match p.photo {
            Some(tl_enums::Photo::Photo(ph)) => {
                Ok(tl_enums::InputMedia::Photo(tl_types::InputMediaPhoto {
                    spoiler: false,
                    live_photo: false,
                    id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                        id: ph.id,
                        access_hash: ph.access_hash,
                        file_reference: ph.file_reference,
                    }),
                    ttl_seconds: None,
                    video: None,
                }))
            }
            _ => Err("uploadMedia: no photo".into()),
        },
        tl_enums::MessageMedia::Document(d) => match d.document {
            Some(tl_enums::Document::Document(doc)) => Ok(input_doc_media(&doc)),
            _ => Err("uploadMedia: no document".into()),
        },
        _ => Err("uploadMedia: unexpected media".into()),
    }
}

pub fn input_document(doc: &tl_types::Document) -> tl_enums::InputDocument {
    tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
        id: doc.id,
        access_hash: doc.access_hash,
        file_reference: doc.file_reference.clone(),
    })
}

pub fn input_doc_media(doc: &tl_types::Document) -> tl_enums::InputMedia {
    tl_enums::InputMedia::Document(tl_types::InputMediaDocument {
        spoiler: false,
        id: input_document(doc),
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
        query: None,
    })
}

pub async fn send_album(
    side: &Side,
    peer: &tl_enums::InputPeer,
    items: Vec<tl_enums::InputMedia>,
    caption: &str,
) -> Result<Vec<i32>, String> {
    let n = items.len();
    let multi = items
        .into_iter()
        .enumerate()
        .map(|(i, media)| {
            tl_enums::InputSingleMedia::InputSingleMedia(tl_types::InputSingleMedia {
                media,
                random_id: side.random_id(),
                message: if i + 1 == n {
                    caption.to_owned()
                } else {
                    String::new()
                },
                entities: None,
            })
        })
        .collect();
    let upd = side
        .call(
            "sendMultiMedia",
            &tl_fns::messages::SendMultiMedia {
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: peer.clone(),
                reply_to: None,
                multi_media: multi,
                schedule_date: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
            },
        )
        .await?;
    Ok(msg_ids(&upd))
}

pub async fn react(
    side: &Side,
    peer: &tl_enums::InputPeer,
    msg_id: i32,
    emoji: &str,
) -> Result<(), String> {
    side.call(
        "sendReaction",
        &tl_fns::messages::SendReaction {
            big: false,
            add_to_recent: true,
            peer: peer.clone(),
            msg_id,
            reaction: Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                emoticon: emoji.to_owned(),
            })]),
        },
    )
    .await
    .map(|_| ())
}

pub async fn typing(side: &Side, peer: &tl_enums::InputPeer, on: bool) -> Result<(), String> {
    side.call(
        "setTyping",
        &tl_fns::messages::SetTyping {
            peer: peer.clone(),
            top_msg_id: None,
            action: if on {
                tl_enums::SendMessageAction::SendMessageTypingAction
            } else {
                tl_enums::SendMessageAction::SendMessageCancelAction
            },
        },
    )
    .await
    .map(|_| ())
}

pub async fn delete(side: &Side, peer: &tl_enums::InputPeer, ids: Vec<i32>) -> Result<(), String> {
    if let tl_enums::InputPeer::Channel(c) = peer {
        side.call(
            "channels.deleteMessages",
            &tl_fns::channels::DeleteMessages {
                channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                    channel_id: c.channel_id,
                    access_hash: c.access_hash,
                }),
                id: ids,
            },
        )
        .await
        .map(|_| ())
    } else {
        side.call(
            "deleteMessages",
            &tl_fns::messages::DeleteMessages {
                revoke: true,
                id: ids,
            },
        )
        .await
        .map(|_| ())
    }
}

pub async fn edit(
    side: &Side,
    peer: &tl_enums::InputPeer,
    id: i32,
    text: &str,
) -> Result<(), String> {
    side.call(
        "editMessage",
        &tl_fns::messages::EditMessage {
            no_webpage: true,
            invert_media: false,
            peer: peer.clone(),
            id,
            message: Some(text.to_owned()),
            media: None,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        },
    )
    .await
    .map(|_| ())
}

/// Messages of a chat, newest first (`limit` up to 100 per page).
pub async fn history(
    side: &Side,
    peer: &tl_enums::InputPeer,
    offset_id: i32,
    limit: i32,
) -> Result<Vec<tl_types::Message>, String> {
    let got = side
        .call(
            "getHistory",
            &tl_fns::messages::GetHistory {
                peer: peer.clone(),
                offset_id,
                offset_date: 0,
                add_offset: 0,
                limit,
                max_id: 0,
                min_id: 0,
                hash: 0,
            },
        )
        .await?;
    let msgs = match got {
        tl_enums::messages::Messages::Messages(m) => m.messages,
        tl_enums::messages::Messages::Slice(m) => m.messages,
        tl_enums::messages::Messages::ChannelMessages(m) => m.messages,
        tl_enums::messages::Messages::NotModified(_) => Vec::new(),
    };
    Ok(msgs
        .into_iter()
        .filter_map(|m| match m {
            tl_enums::Message::Message(m) => Some(m),
            _ => None,
        })
        .collect())
}

/// Up to `max` newest messages of a chat.
pub async fn history_all(
    side: &Side,
    peer: &tl_enums::InputPeer,
    max: usize,
) -> Result<Vec<tl_types::Message>, String> {
    let mut out: Vec<tl_types::Message> = Vec::new();
    let mut offset = 0;
    while out.len() < max {
        let page = history(side, peer, offset, 100).await?;
        let Some(last) = page.last() else { break };
        offset = last.id;
        let n = page.len();
        out.extend(page);
        if n < 100 {
            break;
        }
    }
    Ok(out)
}

pub fn ent_bold(o: i32, l: i32) -> tl_enums::MessageEntity {
    tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
        offset: o,
        length: l,
    })
}
