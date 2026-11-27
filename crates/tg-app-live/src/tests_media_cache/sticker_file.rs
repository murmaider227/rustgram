//! TGB-3126: a sticker in a chat gets its file the way the chat asks for it. The bytes are only
//! on the fake server (`upload.getFile`), never handed to the painter directly, which is what the
//! earlier unit tests of the static sticker fix did.
//!
//! - A static (WebP) sticker turns `Pending` -> `Loading` -> `Ready` with the server's bytes.
//! - A sticker whose `file_reference` expired is downloaded after the message is fetched again
//!   (`FileLoader` on `FILE_REFERENCE_EXPIRED` refreshes from the `Data::FileOrigin`); before,
//!   the download failed and was retried with the same stale reference for ever, so the sticker
//!   stayed an empty square.
//!
//! Synthetic data only.

use std::sync::atomic::AtomicBool;

use tg_api::RpcError;

use super::*;

const STICKER_DOC: i64 = 31_001;
const STALE_REF: &[u8] = &[1, 1, 1];
const FRESH_REF: &[u8] = &[2, 2, 2];

/// A 32 px WebP (lossless, with alpha), as a sticker file.
fn webp_bytes() -> Vec<u8> {
    let image = image::RgbaImage::from_fn(32, 32, |x, y| {
        image::Rgba([
            0xE0,
            (x * 8) as u8,
            (y * 8) as u8,
            if x < 4 { 0 } else { 0xFF },
        ])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::WebP,
        )
        .expect("encode webp");
    out
}

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileWebp,
        mtime: 1_750_000_000,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

fn sticker_document(file_reference: &[u8], size: usize) -> DocumentData {
    DocumentData {
        id: DocumentId(STICKER_DOC as u64),
        access_hash: 77,
        file_reference: file_reference.to_vec(),
        dc_id: 4,
        mime_type: "image/webp".to_owned(),
        size: size as i64,
        kind: DocumentKind::Sticker,
        width: Some(512),
        height: Some(512),
        sticker: Some(StickerMeta {
            alt: "\u{1F600}".to_owned(),
            set_id: 88_001,
            set_access_hash: 99_002,
            is_mask: false,
        }),
        ..DocumentData::default()
    }
}

/// `upload.getFile`'s `file_reference`, if `bytes` is that request.
fn requested_reference(ctor: u32, bytes: &[u8]) -> Option<Vec<u8>> {
    if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
        return None;
    }
    let req = tl_fns::upload::GetFile::from_bytes(bytes).ok()?;
    match req.location {
        tl_enums::InputFileLocation::InputDocumentFileLocation(loc) => Some(loc.file_reference),
        _ => None,
    }
}

/// The message as `messages.getMessages` returns it: the same sticker with a new reference.
fn refetched_message(peer: PeerId, id: i32, bytes_len: usize) -> Vec<u8> {
    let user = DataPeerId(peer.0).to_user().map_or(0, |u| u.0 as i64);
    let mut msg = crate::tests_reply_targets::tl_message(
        id,
        tl_enums::Peer::User(tl_types::PeerUser { user_id: user }),
        user,
        "",
        None,
    );
    if let tl_enums::Message::Message(m) = &mut msg {
        m.media = Some(tl_enums::MessageMedia::Document(
            tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(tl_enums::Document::Document(tl_types::Document {
                    id: STICKER_DOC,
                    access_hash: 77,
                    file_reference: FRESH_REF.to_vec(),
                    date: 1_750_000_000,
                    mime_type: "image/webp".to_owned(),
                    size: bytes_len as i64,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 4,
                    attributes: vec![
                        tl_enums::DocumentAttribute::ImageSize(
                            tl_types::DocumentAttributeImageSize { w: 512, h: 512 },
                        ),
                        tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
                            mask: false,
                            alt: "\u{1F600}".to_owned(),
                            stickerset: tl_enums::InputStickerSet::ID(
                                tl_types::InputStickerSetID {
                                    id: 88_001,
                                    access_hash: 99_002,
                                },
                            ),
                            mask_coords: None,
                        }),
                    ],
                })),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            },
        ));
    }
    tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages: vec![msg],
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    })
    .to_bytes()
}

fn ready_bytes(live: &LiveSession, peer: PeerId, id: i32) -> Option<Vec<u8>> {
    match live.inline_media_descriptor(peer, MsgId(id))?.photo_load? {
        PhotoLoadState::Ready { bytes } => Some(bytes.to_vec()),
        _ => None,
    }
}

#[test]
fn a_static_sticker_downloads_its_file_and_turns_ready() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let webp = webp_bytes();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let (webp, calls) = (webp.clone(), Arc::clone(&calls));
        fake.set_dynamic_handler(Arc::new(move |dc, ctor, bytes| {
            requested_reference(ctor, bytes).map(|_| {
                assert_eq!(dc, 4, "the sticker is asked from its own DC");
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(file_answer(&webp))
            })
        }));
    }
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            401,
            MessageMedia::Document(Box::new(sticker_document(FRESH_REF, webp.len()))),
            MessageFlags::empty(),
        ),
    );

    let first = live
        .inline_media_descriptor(peer_id, MsgId(401))
        .expect("sticker descriptor");
    assert!(
        matches!(
            first.photo_load,
            Some(PhotoLoadState::Pending | PhotoLoadState::Loading { .. })
        ),
        "the first look starts the load: {:?}",
        first.photo_load
    );
    assert!(
        wait_until(|| ready_bytes(&live, peer_id, 401).is_some()),
        "the sticker file never arrived"
    );
    assert_eq!(ready_bytes(&live, peer_id, 401), Some(webp.clone()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The painter decodes the file it gets (the photo pipeline's decoder).
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: webp,
        ..Default::default()
    })
    .expect("the sticker file decodes");
    assert_eq!(decoded.image.dimensions(), (32, 32));
}

#[test]
fn a_sticker_with_an_expired_file_reference_is_fetched_again_and_loads() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let webp = webp_bytes();
    let refetched = Arc::new(AtomicBool::new(false));
    let stale_asks = Arc::new(AtomicUsize::new(0));
    {
        let (webp, refetched, stale_asks) = (
            webp.clone(),
            Arc::clone(&refetched),
            Arc::clone(&stale_asks),
        );
        let answer = refetched_message(peer_id, 402, webp.len());
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, bytes| {
            if ctor == tl_fns::messages::GetMessages::CONSTRUCTOR_ID {
                refetched.store(true, Ordering::SeqCst);
                return Some(Ok(answer.clone()));
            }
            let reference = requested_reference(ctor, bytes)?;
            if reference == STALE_REF {
                stale_asks.fetch_add(1, Ordering::SeqCst);
                return Some(Err(InvocationError::Rpc(RpcError {
                    code: 400,
                    name: "FILE_REFERENCE_EXPIRED".to_owned(),
                    value: None,
                })));
            }
            Some(Ok(file_answer(&webp)))
        }));
    }
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            402,
            MessageMedia::Document(Box::new(sticker_document(STALE_REF, webp.len()))),
            MessageFlags::empty(),
        ),
    );

    let _ = live.inline_media_descriptor(peer_id, MsgId(402));
    assert!(
        wait_until(|| ready_bytes(&live, peer_id, 402).is_some()),
        "a sticker whose file reference expired never loads (stale asks: {}, message fetched \
         again: {})",
        stale_asks.load(Ordering::SeqCst),
        refetched.load(Ordering::SeqCst)
    );
    assert!(
        refetched.load(Ordering::SeqCst),
        "the message was asked for again"
    );
    assert_eq!(ready_bytes(&live, peer_id, 402), Some(webp));
}
