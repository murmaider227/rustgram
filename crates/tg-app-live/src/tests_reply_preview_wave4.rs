//! TGB-2959: what the 32x32 preview of a reply header shows for a video with a cover, a story,
//! a poll with attached media, media of a message from another chat and an invoice photo
//! (`Reply::update` / `Reply::paint`, `DocumentData::getReplyPreview`, `Story::replyPreview`).
//! `FakeRpcClient` only, synthetic data.

use tg_data::{
    DocumentData, DocumentId, DocumentKind, InvoiceData, Message, MessageContent, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, PhotoData, PhotoId, PhotoSize, PollAnswer, PollData,
    PollMedia, ReplyInfo, TextWithEntities as DataText, UserId, WebDocumentData, WebPageData,
};
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{
    PEER_ID, Reply, SELF_ID, build_live, plain_messages, tl_message, tl_user_peer, wait_until,
};
use crate::reply_preview::{
    OwnedReplyImage, ReplyImage, ReplyTargets, reply_image_to_load, reply_preview_source,
    reply_thumb,
};
use tg_app_core::history::backend::{HistoryBackend, ReplyHeader};
use tg_app_core::info::backend::SharedMediaThumb;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const STRIPPED: [u8; 4] = [0x01, 0x20, 0x20, 0xAB];
const COVER_STRIPPED: [u8; 4] = [0x01, 0x10, 0x10, 0xCD];
/// A chat this account has no access to.
const OTHER_CHAT: i64 = 999;

fn peer() -> DataPeerId {
    DataPeerId::from_user(UserId(PEER_ID as u64))
}

fn photo(id: u64, stripped: &[u8]) -> PhotoData {
    PhotoData {
        id: PhotoId(id),
        access_hash: 4242,
        dc_id: 2,
        sizes: vec![
            PhotoSize {
                kind: "m".to_owned(),
                width: 320,
                height: 240,
                size: 12_000,
            },
            PhotoSize {
                kind: "x".to_owned(),
                width: 1280,
                height: 960,
                size: 96_000,
            },
        ],
        stripped_thumb: (!stripped.is_empty()).then(|| stripped.to_vec()),
        ..PhotoData::default()
    }
}

fn video(id: u64, with_thumb: bool) -> DocumentData {
    DocumentData {
        id: DocumentId(id),
        access_hash: 77,
        dc_id: 2,
        mime_type: "video/mp4".to_owned(),
        kind: DocumentKind::Video,
        thumbs: if with_thumb {
            vec![PhotoSize {
                kind: "m".to_owned(),
                width: 320,
                height: 180,
                size: 9_000,
            }]
        } else {
            Vec::new()
        },
        stripped_thumb: Some(STRIPPED.to_vec()),
        ..DocumentData::default()
    }
}

fn message(id: i64, media: MessageMedia) -> Message {
    let mut msg = Message::new_text(
        peer(),
        DataMsgId(id),
        Some(peer()),
        1_795_340_000,
        DataText::plain(String::new()),
    );
    msg.content = MessageContent::Regular {
        text: DataText::plain(String::new()),
        media,
    };
    msg
}

/// A text message that replies to message 1 of the same chat.
fn holder(reply: ReplyInfo) -> Message {
    let mut msg = message(50, MessageMedia::Empty);
    msg.reply_to = Some(reply);
    msg
}

fn to_msg() -> ReplyInfo {
    ReplyInfo {
        reply_to_msg_id: Some(DataMsgId(1)),
        ..ReplyInfo::default()
    }
}

fn target(msg: &Message) -> ReplyTargets<'_> {
    ReplyTargets {
        message: Some(msg),
        story: None,
    }
}

#[test]
fn a_video_with_a_cover_shows_the_cover() {
    let cover = photo(901, &COVER_STRIPPED);
    let mut doc = video(900, true);
    doc.video_cover = Some(Box::new(cover.clone()));
    let orig = message(1, MessageMedia::Document(Box::new(doc.clone())));
    let h = holder(to_msg());
    let source = reply_preview_source(&h, target(&orig))
        .flatten()
        .expect("preview");
    assert_eq!(source.image, ReplyImage::Photo(&cover));
    let thumb = reply_thumb(&h, target(&orig)).flatten().expect("thumb");
    assert_eq!(thumb.stripped.as_deref(), Some(&COVER_STRIPPED[..]));

    // Without a cover: the video's own thumbnail.
    doc.video_cover = None;
    let plain = message(1, MessageMedia::Document(Box::new(doc.clone())));
    let source = reply_preview_source(&h, target(&plain))
        .flatten()
        .expect("preview");
    assert_eq!(source.image, ReplyImage::Document(&doc));
    assert_ne!(
        reply_thumb(&h, target(&plain))
            .flatten()
            .expect("thumb")
            .key,
        thumb.key,
        "another image, another key"
    );

    // A cover does not give a video without a thumbnail a preview (`MediaFile::hasReplyPreview`).
    let mut bare = video(902, false);
    bare.video_cover = Some(Box::new(cover));
    let bare = message(1, MessageMedia::Document(Box::new(bare)));
    assert!(reply_preview_source(&h, target(&bare)).is_none());
}

#[test]
fn a_web_page_video_shows_its_cover_photo() {
    let cover = photo(911, &COVER_STRIPPED);
    let doc = video(910, true);
    let mut page = WebPageData {
        photo: Some(cover.clone()),
        document: Some(doc.clone()),
        photo_is_video_cover: true,
        ..WebPageData::default()
    };
    let h = holder(to_msg());
    let with_cover = message(1, MessageMedia::WebPage(Box::new(page.clone())));
    assert_eq!(
        reply_preview_source(&h, target(&with_cover))
            .flatten()
            .expect("preview")
            .image,
        ReplyImage::Photo(&cover)
    );
    page.photo_is_video_cover = false;
    let without = message(1, MessageMedia::WebPage(Box::new(page)));
    assert_eq!(
        reply_preview_source(&h, target(&without))
            .flatten()
            .expect("preview")
            .image,
        ReplyImage::Document(&doc)
    );
}

#[test]
fn spoiler_and_self_destructing_media_are_blurred_with_particles() {
    let h = holder(to_msg());
    let mut spoilered = photo(920, &STRIPPED);
    spoilered.spoiler = true;
    let orig = message(1, MessageMedia::Photo(Box::new(spoilered)));
    let thumb = reply_thumb(&h, target(&orig)).flatten().expect("thumb");
    assert!(thumb.spoiler && thumb.spoiler_animation);
    assert!(
        reply_image_to_load(&h, target(&orig)).is_none(),
        "the blurred inline thumbnail is kept, the file is not loaded"
    );

    // `hasSpoilerForPreview`: a self-destructing photo counts too.
    let mut timed = photo(921, &STRIPPED);
    timed.ttl_seconds = Some(10);
    let orig = message(1, MessageMedia::Photo(Box::new(timed)));
    assert!(
        reply_thumb(&h, target(&orig))
            .flatten()
            .expect("thumb")
            .spoiler
    );

    // No inline thumbnail: the file is loaded and blurred.
    let mut no_inline = photo(922, &[]);
    no_inline.spoiler = true;
    let orig = message(1, MessageMedia::Photo(Box::new(no_inline.clone())));
    assert_eq!(
        reply_image_to_load(&h, target(&orig)),
        Some(OwnedReplyImage::Photo(Box::new(no_inline)))
    );
}

#[test]
fn a_story_reply_shows_the_story_media_without_a_spoiler() {
    let story_photo = photo(930, &STRIPPED);
    let story = MessageMedia::Photo(Box::new(story_photo.clone()));
    let h = holder(ReplyInfo {
        reply_to_story: Some(tg_data::FullStoryId::new(peer(), 5)),
        ..ReplyInfo::default()
    });
    let targets = ReplyTargets {
        message: None,
        story: Some(&story),
    };
    let source = reply_preview_source(&h, targets)
        .flatten()
        .expect("preview");
    assert_eq!(source.image, ReplyImage::Photo(&story_photo));
    assert!(!source.spoiler && !source.spoiler_animation);

    // A video story: its thumbnail, never the cover (`Story::replyPreview` has no message).
    let mut doc = video(931, true);
    doc.video_cover = Some(Box::new(photo(932, &COVER_STRIPPED)));
    let story = MessageMedia::Document(Box::new(doc.clone()));
    let targets = ReplyTargets {
        message: None,
        story: Some(&story),
    };
    assert_eq!(
        reply_preview_source(&h, targets)
            .flatten()
            .expect("preview")
            .image,
        ReplyImage::Document(&doc)
    );
}

#[test]
fn media_from_another_chat_is_shown_when_the_message_is_not_available() {
    let external = photo(940, &STRIPPED);
    let reply = ReplyInfo {
        reply_to_msg_id: Some(DataMsgId(1)),
        reply_to_peer_id: Some(DataPeerId::from_user(UserId(999))),
        reply_media: Some(Box::new(MessageMedia::Photo(Box::new(external.clone())))),
        ..ReplyInfo::default()
    };
    let h = holder(reply.clone());
    let source = reply_preview_source(&h, ReplyTargets::default())
        .flatten()
        .expect("preview");
    assert_eq!(source.image, ReplyImage::Photo(&external));
    assert!(!source.spoiler && !source.spoiler_animation);

    // The spoiler flag is the one of the holder's own media (the media's parent is the
    // holder in tdesktop), and there are never particles.
    let mut own = photo(941, &STRIPPED);
    own.spoiler = true;
    let mut spoilered_holder = message(50, MessageMedia::Photo(Box::new(own)));
    spoilered_holder.reply_to = Some(reply);
    let source = reply_preview_source(&spoilered_holder, ReplyTargets::default())
        .flatten()
        .expect("preview");
    assert!(source.spoiler && !source.spoiler_animation);

    // A resolved message with media wins over the external media.
    let own_photo = photo(942, &STRIPPED);
    let orig = message(1, MessageMedia::Photo(Box::new(own_photo.clone())));
    assert_eq!(
        reply_preview_source(&h, target(&orig))
            .flatten()
            .expect("preview")
            .image,
        ReplyImage::Photo(&own_photo)
    );
}

#[test]
fn a_poll_reply_shows_the_poll_or_answer_media() {
    let attached = photo(950, &STRIPPED);
    let answer_doc = video(951, true);
    let poll = PollData {
        answers: vec![
            PollAnswer {
                text: DataText::plain("one"),
                option: vec![0],
            },
            PollAnswer {
                text: DataText::plain("two"),
                option: vec![1],
            },
        ],
        attached_media: Some(Box::new(PollMedia {
            photo: Some(attached.clone()),
            document: None,
        })),
        answer_media: vec![(
            vec![1],
            PollMedia {
                photo: None,
                document: Some(answer_doc.clone()),
            },
        )],
        ..PollData::default()
    };
    let orig = message(1, MessageMedia::Poll(Box::new(poll)));

    let h = holder(to_msg());
    assert_eq!(
        reply_preview_source(&h, target(&orig))
            .flatten()
            .expect("the poll's attached media")
            .image,
        ReplyImage::Photo(&attached)
    );

    let to_answer = |option: u8| {
        holder(ReplyInfo {
            poll_option: Some(vec![option]),
            ..to_msg()
        })
    };
    let h = to_answer(1);
    assert_eq!(
        reply_preview_source(&h, target(&orig))
            .flatten()
            .expect("the answer's media")
            .image,
        ReplyImage::Document(&answer_doc)
    );
    // An answer without media has no preview: the poll's own media is not used for it.
    let h = to_answer(0);
    assert!(reply_preview_source(&h, target(&orig)).is_none());
}

#[test]
fn an_invoice_reply_shows_the_product_photo() {
    let web = WebDocumentData {
        url: "https://shop.example/item.jpg".to_owned(),
        access_hash: Some(55),
        size: 20_000,
        mime_type: "image/jpeg".to_owned(),
        width: 640,
        height: 480,
    };
    let invoice = |photo: Option<WebDocumentData>| {
        message(
            1,
            MessageMedia::Invoice(Box::new(InvoiceData {
                title: "Pro Plan".to_owned(),
                photo,
                ..InvoiceData::default()
            })),
        )
    };
    let h = holder(to_msg());
    let orig = invoice(Some(web.clone()));
    assert_eq!(
        reply_image_to_load(&h, target(&orig)),
        Some(OwnedReplyImage::Web(web.clone()))
    );
    assert!(reply_preview_source(&h, target(&invoice(None))).is_none());
    // `webDocumentNoProxy` would be fetched straight from its URL: not done, no preview.
    let no_proxy = WebDocumentData {
        access_hash: None,
        ..web
    };
    assert!(reply_preview_source(&h, target(&invoice(Some(no_proxy)))).is_none());
}

// --- through the live session ------------------------------------------------------------

fn user_peer() -> PeerId {
    PeerId(peer().0)
}

fn tl_photo(id: i64) -> tl_enums::Photo {
    tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: false,
        id,
        access_hash: 4242,
        file_reference: vec![1, 2, 3],
        date: 1_795_000_000,
        sizes: vec![
            tl_enums::PhotoSize::PhotoStrippedSize(tl_types::PhotoStrippedSize {
                r#type: "i".into(),
                bytes: STRIPPED.to_vec(),
            }),
            tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "m".into(),
                w: 320,
                h: 240,
                size: 12_000,
            }),
        ],
        video_sizes: None,
        dc_id: 2,
    })
}

fn tl_photo_media(id: i64) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl_photo(id)),
        ttl_seconds: None,
        video: None,
    })
}

fn header(live: &crate::LiveSession, id: i32) -> ReplyHeader {
    live.load_slice_around(user_peer(), MsgId::SHOW_AT_END, 50, 0)
        .messages
        .iter()
        .find(|m| m.id.0 == id)
        .and_then(|m| m.reply.clone())
        .unwrap_or_else(|| panic!("message {id} has no reply header"))
}

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileJpeg,
        mtime: 0,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

fn wait_ready(live: &crate::LiveSession, holder: i32) -> SharedMediaThumb {
    let mut last = live.reply_preview_thumb(user_peer(), MsgId(holder));
    wait_until(|| {
        last = live.reply_preview_thumb(user_peer(), MsgId(holder));
        matches!(last, SharedMediaThumb::Ready { .. })
    });
    last
}

#[test]
fn a_story_reply_gets_the_story_photo_as_its_thumbnail() {
    let mut reply = tl_message(601, tl_user_peer(PEER_ID), SELF_ID, "re story", None);
    if let tl_enums::Message::Message(inner) = &mut reply {
        inner.reply_to = Some(tl_enums::MessageReplyHeader::MessageReplyStoryHeader(
            tl_types::MessageReplyStoryHeader {
                peer: tl_user_peer(PEER_ID),
                story_id: 5,
            },
        ));
    }
    let (rpc, live) = build_live(peer(), vec![reply]);
    let payload = vec![0xFF, 0xD8, 0xFF, 0xE0, 9, 9, 9];
    let answer = payload.clone();
    rpc.set_handler(move |ctor, _| {
        if ctor == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID {
            return Some(
                tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                    count: 1,
                    stories: vec![tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                        pinned: false,
                        public: true,
                        close_friends: false,
                        min: false,
                        noforwards: false,
                        edited: false,
                        contacts: false,
                        selected_contacts: false,
                        out: false,
                        id: 5,
                        date: 1_795_300_000,
                        from_id: None,
                        fwd_from: None,
                        expire_date: 1_795_386_400,
                        caption: Some("sunset".to_owned()),
                        entities: None,
                        media: Box::new(tl_photo_media(9305)),
                        media_areas: None,
                        privacy: None,
                        views: None,
                        sent_reaction: None,
                        albums: None,
                        music: None,
                    })],
                    pinned_to_top: None,
                    chats: Vec::new(),
                    users: Vec::new(),
                })
                .to_bytes(),
            );
        }
        (ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID).then(|| file_answer(&answer))
    });

    // While the story loads there is no preview; then its photo.
    assert!(header(&live, 601).preview_thumb.is_none());
    assert!(
        wait_until(|| header(&live, 601).preview_thumb.is_some()),
        "the story reply never got its thumbnail: {:?}",
        header(&live, 601)
    );
    let resolved = header(&live, 601);
    assert_eq!(resolved.preview_text, "Story, sunset");
    assert!(resolved.preview_thumb_rgb.is_some());
    let thumb = resolved.preview_thumb.expect("thumb");
    assert_eq!(thumb.stripped.as_deref(), Some(&STRIPPED[..]));
    assert!(!thumb.spoiler);

    assert_eq!(
        wait_ready(&live, 601),
        SharedMediaThumb::Ready { bytes: payload }
    );
    let requests = rpc.calls::<tl_fns::upload::GetFile>();
    let tl_enums::InputFileLocation::InputPhotoFileLocation(location) = &requests[0].location
    else {
        panic!("expected a photo location, got {:?}", requests[0].location);
    };
    assert_eq!(location.id, 9305);
}

#[test]
fn a_reply_to_media_of_another_chat_shows_that_media() {
    let mut reply = tl_message(
        701,
        tl_user_peer(PEER_ID),
        PEER_ID,
        "look",
        Some(Reply {
            to: 4000,
            peer: Some(tl_user_peer(OTHER_CHAT)),
            from_name: Some("Old Author"),
            ..Reply::default()
        }),
    );
    if let tl_enums::Message::Message(inner) = &mut reply
        && let Some(tl_enums::MessageReplyHeader::MessageReplyHeader(h)) = &mut inner.reply_to
    {
        h.reply_media = Some(tl_photo_media(9401));
    }
    let (rpc, live) = build_live(peer(), vec![reply]);
    let payload = vec![0xFF, 0xD8, 0xFF, 0xE0, 4, 4, 4];
    let answer = payload.clone();
    rpc.set_handler(move |ctor, _| {
        if ctor == tl_fns::messages::GetMessages::CONSTRUCTOR_ID {
            // The message of the other chat is not available to this account.
            return Some(plain_messages(
                vec![tl_enums::Message::Empty(tl_types::MessageEmpty {
                    id: 4000,
                    peer_id: None,
                })],
                Vec::new(),
            ));
        }
        (ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID).then(|| file_answer(&answer))
    });

    // The target cannot be shown: the header shows the author, the media's text and its
    // thumbnail (`externalMedia`).
    assert!(
        wait_until(|| header(&live, 701).preview_thumb.is_some()),
        "no thumbnail for the external media: {:?}",
        header(&live, 701)
    );
    let resolved = header(&live, 701);
    assert_eq!(resolved.author_name, "Old Author");
    assert_eq!(resolved.preview_text, "Photo");
    assert_eq!(resolved.preview_colored_len, "Photo".len());
    assert_eq!(
        wait_ready(&live, 701),
        SharedMediaThumb::Ready { bytes: payload }
    );
}

#[test]
fn an_invoice_reply_loads_the_product_photo_with_get_web_file() {
    let mut invoice = tl_message(800, tl_user_peer(PEER_ID), PEER_ID, "", None);
    if let tl_enums::Message::Message(inner) = &mut invoice {
        inner.media = Some(tl_enums::MessageMedia::Invoice(
            tl_types::MessageMediaInvoice {
                shipping_address_requested: false,
                test: true,
                title: "Pro Plan".to_owned(),
                description: "Monthly".to_owned(),
                photo: Some(tl_enums::WebDocument::WebDocument(tl_types::WebDocument {
                    url: "https://shop.example/item.jpg".to_owned(),
                    access_hash: 55,
                    size: 7,
                    mime_type: "image/jpeg".to_owned(),
                    attributes: vec![tl_enums::DocumentAttribute::ImageSize(
                        tl_types::DocumentAttributeImageSize { w: 640, h: 480 },
                    )],
                })),
                receipt_msg_id: None,
                currency: "USD".to_owned(),
                total_amount: 999,
                start_param: "pro".to_owned(),
                extended_media: None,
            },
        ));
    }
    let reply = tl_message(
        801,
        tl_user_peer(PEER_ID),
        PEER_ID,
        "how much?",
        Some(Reply {
            to: 800,
            ..Reply::default()
        }),
    );
    let (rpc, live) = build_live(peer(), vec![invoice, reply]);
    let payload = vec![0xFF, 0xD8, 0xFF, 0xE0, 7, 7, 7];
    let answer = payload.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::upload::GetWebFile::CONSTRUCTOR_ID).then(|| {
            tl_enums::upload::WebFile::WebFile(tl_types::upload::WebFile {
                size: answer.len() as i32,
                mime_type: "image/jpeg".to_owned(),
                file_type: tl_enums::storage::FileType::FileJpeg,
                mtime: 0,
                bytes: answer.clone(),
            })
            .to_bytes()
        })
    });

    let resolved = header(&live, 801);
    assert!(resolved.preview_thumb_rgb.is_some());
    let thumb = resolved.preview_thumb.expect("the invoice photo");
    assert!(thumb.stripped.is_none() && !thumb.spoiler);

    assert_eq!(
        wait_ready(&live, 801),
        SharedMediaThumb::Ready { bytes: payload }
    );
    let requests = rpc.calls::<tl_fns::upload::GetWebFile>();
    assert_eq!(requests.len(), 1, "fetched once, then kept");
    let tl_enums::InputWebFileLocation::InputWebFileLocation(location) = &requests[0].location
    else {
        panic!(
            "expected a web file location, got {:?}",
            requests[0].location
        );
    };
    assert_eq!(location.url, "https://shop.example/item.jpg");
    assert_eq!(location.access_hash, 55);
}
