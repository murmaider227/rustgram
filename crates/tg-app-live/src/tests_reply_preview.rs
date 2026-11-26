//! Reply header of a message that replies to a photo (`FakeRpcClient` only, synthetic data):
//! the text is the media description ("Photo"), the header carries the inline thumbnail, and
//! the real thumbnail is downloaded through `reply_preview_thumb`.

use tg_data::PeerId as DataPeerId;
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{
    PEER_ID, Reply, build_live, tl_message, tl_user_peer, wait_until,
};
use tg_app_core::history::backend::{HistoryBackend, HistoryMessage, ReplyHeader};
use tg_app_core::info::backend::SharedMediaThumb;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const STRIPPED: [u8; 4] = [0x01, 0x20, 0x20, 0xAB];

fn user_peer() -> PeerId {
    PeerId(DataPeerId::from_user(tg_data::UserId(PEER_ID as u64)).0)
}

fn photo(id: i64) -> tl_enums::Photo {
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
            tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "x".into(),
                w: 1280,
                h: 960,
                size: 96_000,
            }),
        ],
        video_sizes: None,
        dc_id: 2,
    })
}

fn photo_message(id: i32, caption: &str, spoiler: bool) -> tl_enums::Message {
    let mut msg = tl_message(id, tl_user_peer(PEER_ID), PEER_ID, caption, None);
    if let tl_enums::Message::Message(m) = &mut msg {
        m.media = Some(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler,
            live_photo: false,
            photo: Some(photo(i64::from(id) + 9000)),
            ttl_seconds: None,
            video: None,
        }));
    }
    msg
}

fn reply_to(id: i32, to: i32) -> tl_enums::Message {
    tl_message(
        id,
        tl_user_peer(PEER_ID),
        PEER_ID,
        "a reply",
        Some(Reply {
            to,
            ..Reply::default()
        }),
    )
}

fn reply_header(messages: &[HistoryMessage], id: i32) -> ReplyHeader {
    messages
        .iter()
        .find(|m| m.id.0 == id)
        .and_then(|m| m.reply.clone())
        .unwrap_or_else(|| panic!("message {id} has no reply header"))
}

#[test]
fn a_reply_to_a_photo_shows_the_media_text_and_the_inline_thumbnail() {
    let peer = user_peer();
    let (_rpc, live) = build_live(
        DataPeerId(peer.0),
        vec![
            photo_message(10, "", false),
            photo_message(11, "look at this", false),
            tl_message(12, tl_user_peer(PEER_ID), PEER_ID, "plain text", None),
            reply_to(20, 10),
            reply_to(21, 11),
            reply_to(22, 12),
        ],
    );
    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);

    let photo = reply_header(&slice.messages, 20);
    assert_eq!(photo.preview_text, tg_lang::tr::lng_in_dlg_photo());
    assert_eq!(photo.preview_text, "Photo");
    assert_eq!(photo.preview_colored_len, "Photo".len());
    let thumb = photo.preview_thumb.expect("a photo reply has a preview");
    assert_eq!(thumb.stripped.as_deref(), Some(&STRIPPED[..]));
    assert!(!thumb.round && !thumb.spoiler);
    assert!(
        photo.preview_thumb_rgb.is_some(),
        "the layout reserves the preview"
    );

    // No mini images in a reply header: "{media}, {caption}" with the media part coloured.
    let captioned = reply_header(&slice.messages, 21);
    assert_eq!(captioned.preview_text, "Photo, look at this");
    assert_eq!(captioned.preview_colored_len, "Photo,".len());
    assert!(captioned.preview_thumb.is_some());

    let text = reply_header(&slice.messages, 22);
    assert_eq!(text.preview_text, "plain text");
    assert_eq!(text.preview_colored_len, 0);
    assert!(text.preview_thumb.is_none());
    assert!(text.preview_thumb_rgb.is_none());
}

#[test]
fn the_thumbnail_of_the_replied_to_photo_is_downloaded() {
    let peer = user_peer();
    let (rpc, live) = build_live(
        DataPeerId(peer.0),
        vec![
            photo_message(10, "", false),
            tl_message(12, tl_user_peer(PEER_ID), PEER_ID, "plain text", None),
            photo_message(13, "", true),
            reply_to(20, 10),
            reply_to(22, 12),
            reply_to(23, 13),
        ],
    );
    let payload = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
    let answer = payload.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID).then(|| {
            tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FileJpeg,
                mtime: 0,
                bytes: answer.clone(),
            })
            .to_bytes()
        })
    });

    // Nothing to load for a text message, and a spoilered photo keeps its blurred thumbnail.
    assert_eq!(
        live.reply_preview_thumb(peer, MsgId(22)),
        SharedMediaThumb::Unavailable
    );
    assert_eq!(
        live.reply_preview_thumb(peer, MsgId(23)),
        SharedMediaThumb::Unavailable
    );
    assert!(rpc.calls::<tl_fns::upload::GetFile>().is_empty());

    let mut last = live.reply_preview_thumb(peer, MsgId(20));
    assert!(
        wait_until(|| {
            last = live.reply_preview_thumb(peer, MsgId(20));
            matches!(last, SharedMediaThumb::Ready { .. })
        }),
        "thumbnail never arrived: {last:?}"
    );
    assert_eq!(last, SharedMediaThumb::Ready { bytes: payload });

    let requests = rpc.calls::<tl_fns::upload::GetFile>();
    assert!(
        !requests.is_empty(),
        "the thumbnail is fetched from the cloud"
    );
    for request in &requests {
        let tl_enums::InputFileLocation::InputPhotoFileLocation(location) = &request.location
        else {
            panic!("expected a photo location, got {:?}", request.location);
        };
        assert_eq!(location.id, 9010);
        assert_eq!(
            location.thumb_size, "m",
            "the thumbnail size, not the full photo"
        );
    }
}
