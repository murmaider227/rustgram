//! TGB-3126: the sticker set box gets a picture for each sticker of a pack the user has not
//! installed. `messages.getStickerSet` gives no picture bytes, so before this every cell stayed a
//! placeholder; now each sticker's file is downloaded with the pack's file reference and the
//! cell gets it. Fake server, synthetic data.

use std::time::{Duration, Instant};

use tg_data::PeerId as DataPeerId;
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tg_ui::sticker_thumbs::StickerThumbSource;

use super::LiveStickerThumbs;
use crate::tests_reply_targets::{PEER_ID, build_live, tl_message, tl_user_peer};
use tg_app_core::session_api::AppSession;

const SET_ID: i64 = 88_101;
const SET_HASH: i64 = 99_102;
const DOC: i64 = 70_101;
const PACK_REF: &[u8] = &[9, 8, 7];

fn webp_bytes() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(24, 16, image::Rgba([0x30, 0xA0, 0xE0, 0xFF]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::WebP,
        )
        .expect("encode webp");
    out
}

fn pack(size: usize) -> Vec<u8> {
    let doc = tl_enums::Document::Document(tl_types::Document {
        id: DOC,
        access_hash: DOC * 3,
        file_reference: PACK_REF.to_vec(),
        date: 1_750_000_000,
        mime_type: "image/webp".to_owned(),
        size: size as i64,
        // As the server sends user-made stickers: a WebP thumbnail to download, no stripped one.
        thumbs: Some(vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
            r#type: "m".to_owned(),
            w: 128,
            h: 128,
            size: 900,
        })]),
        video_thumbs: None,
        dc_id: 4,
        attributes: vec![
            tl_enums::DocumentAttribute::ImageSize(tl_types::DocumentAttributeImageSize {
                w: 512,
                h: 341,
            }),
            tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                mask: false,
                alt: "\u{1F600}".to_owned(),
                stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                    id: SET_ID,
                    access_hash: SET_HASH,
                }),
                mask_coords: None,
            }),
        ],
    });
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
            short_name: "HandmadePackTwo".to_owned(),
            thumbs: None,
            thumb_dc_id: None,
            thumb_version: None,
            thumb_document_id: None,
            count: 1,
            hash: 7,
        }),
        packs: Vec::new(),
        keywords: Vec::new(),
        documents: vec![doc],
    })
    .to_bytes()
}

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileWebp,
        mtime: 1_750_000_000,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

#[test]
fn a_pack_opened_from_a_sticker_gets_a_picture_for_each_cell() {
    let peer = DataPeerId::from_user(tg_data::UserId(PEER_ID as u64));
    let (rpc, mut live) = build_live(
        peer,
        vec![tl_message(1, tl_user_peer(PEER_ID), PEER_ID, "hello", None)],
    );
    let webp = webp_bytes();
    let (set_reply, file_reply) = (pack(webp.len()), file_answer(&webp));
    rpc.set_handler(move |ctor, _| {
        if ctor == tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID {
            Some(set_reply.clone())
        } else if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            Some(file_reply.clone())
        } else {
            None
        }
    });

    let preview = live
        .resolve_sticker_set_by_id(SET_ID as u64, SET_HASH as u64)
        .expect("the pack");
    assert_eq!(preview.items.len(), 1);
    assert!(
        preview.items[0].thumb_rgba.is_none(),
        "the pack itself carries no picture bytes"
    );

    let source = LiveStickerThumbs::new(&live);
    let deadline = Instant::now() + Duration::from_secs(10);
    let thumb = loop {
        if let Some(thumb) = source.sticker_thumb(DOC as u64) {
            break Some(thumb);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let thumb = thumb.expect("the cell never got its sticker picture");
    assert_eq!((thumb.width, thumb.height), (24, 16));
    assert_eq!(&thumb.rgba[..4], &[0x30, 0xA0, 0xE0, 0xFF]);

    let gets = rpc.calls::<tl_fns::upload::GetFile>();
    let get = gets.first().expect("upload.getFile");
    match &get.location {
        tl_enums::InputFileLocation::InputDocumentFileLocation(loc) => {
            assert_eq!(loc.id, DOC);
            assert_eq!(loc.file_reference, PACK_REF, "the pack's file reference");
        }
        other => panic!("{other:?}"),
    }
}
