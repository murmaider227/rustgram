//! TGB-3126: a sticker message carries its set, and a click on it looks the pack up by id
//! (`messages.getStickerSet` with `inputStickerSetID`, as `StickerSetBox` does). Fake server,
//! synthetic data.

use tg_data::PeerId as DataPeerId;
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{PEER_ID, build_live, tl_message, tl_user_peer};
use tg_app_core::history::backend::{HistoryBackend, HistoryMedia, StickerSetId};
use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const SET_ID: i64 = 88_001;
const SET_HASH: i64 = 99_002;

fn sticker_attr() -> tl_enums::DocumentAttribute {
    tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
        mask: false,
        alt: "\u{1F600}".to_owned(),
        stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
            id: SET_ID,
            access_hash: SET_HASH,
        }),
        mask_coords: None,
    })
}

fn document(id: i64, mime: &str, extra: Vec<tl_enums::DocumentAttribute>) -> tl_enums::Document {
    let mut attributes = vec![
        tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
            w: 512,
            h: 512,
        }),
        sticker_attr(),
    ];
    attributes.extend(extra);
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 11,
        file_reference: vec![1, 2, 3],
        date: 1_750_000_000,
        mime_type: mime.to_owned(),
        size: 9_000,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes,
    })
}

fn sticker_message(id: i32, doc: tl_enums::Document) -> tl_enums::Message {
    let mut msg = tl_message(id, tl_user_peer(PEER_ID), PEER_ID, "", None);
    if let tl_enums::Message::Message(m) = &mut msg {
        m.media = Some(tl_enums::MessageMedia::Document(
            tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(doc),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            },
        ));
    }
    msg
}

fn sticker_set() -> tl_enums::messages::StickerSet {
    tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
        set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
            archived: false,
            official: false,
            masks: false,
            emojis: false,
            text_color: false,
            channel_emoji_status: false,
            creator: false,
            installed_date: None,
            id: SET_ID,
            access_hash: SET_HASH,
            title: "Handmade Pack".to_owned(),
            short_name: "HandmadePack".to_owned(),
            thumbs: None,
            thumb_dc_id: None,
            thumb_version: None,
            thumb_document_id: None,
            count: 1,
            hash: 7,
        }),
        packs: vec![tl_enums::StickerPack::StickerPack(tl_types::StickerPack {
            emoticon: "\u{1F600}".to_owned(),
            documents: vec![701],
        })],
        keywords: Vec::new(),
        documents: vec![document(701, "image/webp", Vec::new())],
    })
}

/// `build_live` needs a history page; one text message.
fn plain() -> tl_enums::Message {
    tl_message(1, tl_user_peer(PEER_ID), PEER_ID, "hello", None)
}

fn user_peer() -> PeerId {
    PeerId(DataPeerId::from_user(tg_data::UserId(PEER_ID as u64)).0)
}

#[test]
fn every_sticker_kind_carries_its_set() {
    let video_attr = tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
        round_message: false,
        supports_streaming: false,
        nosound: true,
        duration: 2.0,
        w: 512,
        h: 512,
        preload_prefix_size: None,
        video_start_ts: None,
        video_codec: None,
    });
    let peer = user_peer();
    let (_rpc, live) = build_live(
        DataPeerId(peer.0),
        vec![
            sticker_message(10, document(501, "image/webp", Vec::new())),
            sticker_message(11, document(502, "application/x-tgsticker", Vec::new())),
            sticker_message(12, document(503, "video/webm", vec![video_attr])),
        ],
    );
    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    let expected = Some(StickerSetId {
        id: SET_ID as u64,
        access_hash: SET_HASH as u64,
    });
    for id in [10, 11, 12] {
        let msg = slice
            .messages
            .iter()
            .find(|m| m.id.0 == id)
            .unwrap_or_else(|| panic!("message {id}"));
        let Some(HistoryMedia::Sticker(sticker)) = &msg.media else {
            panic!("message {id} is a sticker: {:?}", msg.media);
        };
        assert_eq!(sticker.set, expected, "message {id}");
    }
}

#[test]
fn a_clicked_sticker_opens_its_pack_by_set_id() {
    let peer = user_peer();
    let (rpc, mut live) = build_live(DataPeerId(peer.0), vec![plain()]);
    let reply = sticker_set().to_bytes();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID).then(|| reply.clone())
    });
    let preview = live
        .resolve_sticker_set_by_id(SET_ID as u64, SET_HASH as u64)
        .expect("the pack");
    assert_eq!(preview.title, "Handmade Pack");
    assert_eq!(preview.short_name, "HandmadePack");
    assert_eq!(preview.set_id, SET_ID as u64);
    let calls = rpc.calls::<tl_fns::messages::GetStickerSet>();
    let last = calls.last().expect("messages.getStickerSet");
    assert!(
        matches!(
            &last.stickerset,
            tl_enums::InputStickerSet::ID(set) if set.id == SET_ID && set.access_hash == SET_HASH
        ),
        "{:?}",
        last.stickerset
    );
}

#[test]
fn an_unknown_pack_shows_sticker_set_not_found() {
    let peer = user_peer();
    let (rpc, mut live) = build_live(DataPeerId(peer.0), vec![plain()]);
    rpc.set_handler(|_, _| None);
    let err = live
        .resolve_sticker_set_by_id(1, 2)
        .expect_err("no such pack");
    let shown = tg_app_core::user_error::sticker_set(&err);
    assert!(
        format!("{shown:?}").contains(&tg_lang::tr::lng_stickers_not_found()),
        "{shown:?}"
    );
}
