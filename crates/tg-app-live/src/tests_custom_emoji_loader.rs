//! TGB-2964: the custom emoji a history shows (emoji statuses on name lines, custom emoji in
//! text) are loaded the way `Data::CustomEmojiManager` loads them: one
//! `messages.getCustomEmojiDocuments` per 100 ids, the file downloaded once, the result kept.

use std::io::Cursor;

use tg_data::{ChannelId, DocumentId, PeerId as DataPeerId, UserFlags, UserId as DataUserId};
use tg_tl::{
    Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::custom_emoji::has_registered_custom_emoji;

use super::tests_reply_targets::{FakeRpcClient, PEER_ID, build_live, tl_message, wait_until};
use crate::custom_emoji_loader::{MAX_PER_REQUEST, asset_from_blob, request, wanted_ids};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_ui::custom_emoji::CustomEmojiAsset;

const GROUP_ID: i64 = 700;

fn group_data_peer() -> DataPeerId {
    DataPeerId::from_channel(ChannelId(GROUP_ID as u64))
}

fn tl_group_peer() -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: GROUP_ID,
    })
}

fn png_bytes() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([0xF0, 0xA0, 0x20, 0xFF]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

fn emoji_doc(id: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 13 + 5,
        file_reference: vec![9, 8, 7],
        date: 1_750_000_000,
        mime_type: "image/png".to_owned(),
        size: 0,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl_enums::DocumentAttribute::CustomEmoji(
            tl_types::DocumentAttributeCustomEmoji {
                free: false,
                text_color: false,
                alt: "⭐".to_owned(),
                stickerset: tl_enums::InputStickerSet::Empty,
            },
        )],
    })
}

/// Answers `getCustomEmojiDocuments` with a still-image document per id and `upload.getFile`
/// with a small PNG.
fn serve_emoji(rpc: &FakeRpcClient) {
    let png = png_bytes();
    rpc.set_handler(move |ctor, bytes| {
        if ctor == tl_fns::messages::GetCustomEmojiDocuments::CONSTRUCTOR_ID {
            let req = tl_fns::messages::GetCustomEmojiDocuments::from_bytes(bytes).ok()?;
            let docs: Vec<tl_enums::Document> =
                req.document_id.iter().map(|&id| emoji_doc(id)).collect();
            return Some(docs.to_bytes());
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return Some(
                tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FilePng,
                    mtime: 0,
                    bytes: png.clone(),
                })
                .to_bytes(),
            );
        }
        None
    });
}

#[test]
fn ids_load_in_batches_of_100_once_and_register_for_painting() {
    let (rpc, live) = build_live(
        group_data_peer(),
        vec![tl_message(1000, tl_group_peer(), PEER_ID, "hi", None)],
    );
    serve_emoji(&rpc);
    let ids: Vec<u64> = (9_100_000..9_100_150).collect();
    request(&live, &ids);
    assert!(
        wait_until(|| ids
            .iter()
            .all(|id| has_registered_custom_emoji(&id.to_string()))),
        "every requested emoji is registered"
    );
    let batches = rpc.calls::<tl_fns::messages::GetCustomEmojiDocuments>();
    assert_eq!(batches.len(), 2, "150 ids go as 100 + 50");
    assert_eq!(batches[0].document_id.len(), MAX_PER_REQUEST);
    assert_eq!(batches[1].document_id.len(), 50);
    assert_eq!(rpc.calls::<tl_fns::upload::GetFile>().len(), 150);

    // Asking again costs nothing: they are registered.
    request(&live, &ids);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        rpc.calls::<tl_fns::messages::GetCustomEmojiDocuments>()
            .len(),
        2
    );
}

#[test]
fn opening_a_group_loads_the_emoji_status_of_a_premium_sender() {
    const STATUS: u64 = 9_200_001;
    let (rpc, live) = build_live(
        group_data_peer(),
        (1000..1010)
            .map(|id| tl_message(id, tl_group_peer(), PEER_ID, "hi", None))
            .collect(),
    );
    live.write_session(|s| {
        let mut user = s.user(DataUserId(PEER_ID as u64)).cloned().expect("sender");
        user.flags.insert(UserFlags::PREMIUM);
        user.common.emoji_status.document_id = DocumentId(STATUS);
        s.upsert_user(user, tg_data::PeerUpdateFlags::EMOJI_STATUS);
    });
    serve_emoji(&rpc);
    assert!(!has_registered_custom_emoji(&STATUS.to_string()));

    let slice = live.load_slice_around(PeerId(group_data_peer().0), MsgId::SHOW_AT_END, 50, 0);
    assert!(
        slice
            .messages
            .iter()
            .all(|m| m.extra.sender_status.and_then(|s| s.emoji_status_id) == Some(STATUS))
    );
    assert_eq!(wanted_ids(&slice.messages), vec![STATUS; 10]);
    assert!(wait_until(|| has_registered_custom_emoji(
        &STATUS.to_string()
    )));
    let batches = rpc.calls::<tl_fns::messages::GetCustomEmojiDocuments>();
    assert_eq!(
        batches.len(),
        1,
        "ten messages of one sender make one request"
    );
    assert_eq!(batches[0].document_id, vec![STATUS as i64]);
}

#[test]
fn cached_blobs_decode_by_their_header() {
    let png = png_bytes();
    let mut still = vec![2u8];
    still.extend_from_slice(&png);
    assert!(matches!(
        asset_from_blob(&still),
        Some(CustomEmojiAsset::StaticRgba {
            width: 8,
            height: 8,
            text_color_tinted: false,
            ..
        })
    ));
    assert!(matches!(
        asset_from_blob(&[0b100, 1, 2, 3]),
        Some(CustomEmojiAsset::TgsBytes {
            text_color_tinted: true,
            ..
        })
    ));
    assert!(matches!(
        asset_from_blob(&[1, 1, 2]),
        Some(CustomEmojiAsset::WebmBytes { .. })
    ));
    assert!(asset_from_blob(&[]).is_none());
}
