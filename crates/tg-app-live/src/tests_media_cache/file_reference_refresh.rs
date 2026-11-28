//! TGB-3192: every file of a chat message is downloaded with the message as its origin, so an
//! expired `file_reference` is fetched again (`messages.getMessages`) and the download repeated
//! with the new one, as tdesktop's `FileLoader` does from `Data::FileOriginMessage`
//! (`Api::RefreshFileReference`). Before, only stickers had an origin (TGB-3126): a photo, a
//! video's thumbnail, a GIF, a round video or a voice message failed with
//! `FILE_REFERENCE_EXPIRED` and was retried with the same stale reference for ever.
//!
//! The fake server holds the files; a request with the stale reference fails, the refetched
//! message carries the fresh one. Synthetic data only.

use std::sync::atomic::AtomicBool;

use tg_api::RpcError;
use tg_app_core::core_types::dto::inline_media::InlineMediaDescriptor;

use super::*;

const STALE_REF: &[u8] = &[7, 7, 7];
const FRESH_REF: &[u8] = &[8, 8, 8];
const SELF_UID: u64 = 700_001;

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FilePartial,
        mtime: 1_750_000_000,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

/// The `file_reference` of an `upload.getFile` request for a photo or a document.
fn requested_reference(ctor: u32, bytes: &[u8]) -> Option<Vec<u8>> {
    if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
        return None;
    }
    match tl_fns::upload::GetFile::from_bytes(bytes).ok()?.location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(loc) => Some(loc.file_reference),
        tl_enums::InputFileLocation::InputDocumentFileLocation(loc) => Some(loc.file_reference),
        _ => None,
    }
}

fn photo_media(file_reference: &[u8], size: usize) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
            has_stickers: false,
            id: 61_001,
            access_hash: 11,
            file_reference: file_reference.to_vec(),
            date: 1_750_000_000,
            sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "x".into(),
                w: 800,
                h: 600,
                size: size as i32,
            })],
            video_sizes: None,
            dc_id: 2,
        })),
        ttl_seconds: None,
        video: None,
    })
}

struct DocSpec {
    id: i64,
    mime: &'static str,
    size: usize,
    attributes: Vec<tl_enums::DocumentAttribute>,
    /// Size of a `m` thumbnail, when the document has one.
    thumb: Option<usize>,
    round: bool,
    voice: bool,
}

fn video_attribute(round: bool, nosound: bool) -> tl_enums::DocumentAttribute {
    tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
        round_message: round,
        supports_streaming: true,
        nosound,
        duration: 3.0,
        w: 320,
        h: 320,
        preload_prefix_size: None,
        video_start_ts: None,
        video_codec: None,
    })
}

fn document_media(spec: &DocSpec, file_reference: &[u8]) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: spec.attributes.iter().any(|a| {
            matches!(a, tl_enums::DocumentAttribute::Video(_))
                && !spec.round
                && !spec
                    .attributes
                    .iter()
                    .any(|a| matches!(a, tl_enums::DocumentAttribute::Animated))
        }),
        round: spec.round,
        voice: spec.voice,
        document: Some(tl_enums::Document::Document(tl_types::Document {
            id: spec.id,
            access_hash: 12,
            file_reference: file_reference.to_vec(),
            date: 1_750_000_000,
            mime_type: spec.mime.to_owned(),
            size: spec.size as i64,
            thumbs: spec.thumb.map(|size| {
                vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                    r#type: "m".into(),
                    w: 320,
                    h: 180,
                    size: size as i32,
                })]
            }),
            video_thumbs: None,
            dc_id: 2,
            attributes: spec.attributes.clone(),
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    })
}

fn tl_message_with(peer: PeerId, id: i32, media: tl_enums::MessageMedia) -> tl_enums::Message {
    let user = DataPeerId(peer.0).to_user().map_or(0, |u| u.0 as i64);
    let mut msg = crate::tests_reply_targets::tl_message(
        id,
        tl_enums::Peer::User(tl_types::PeerUser { user_id: user }),
        user,
        "",
        None,
    );
    if let tl_enums::Message::Message(m) = &mut msg {
        m.media = Some(media);
    }
    msg
}

struct Served {
    refetched: Arc<AtomicBool>,
    stale_asks: Arc<AtomicUsize>,
}

/// Stores message `id` with `stale` media and serves `file` for the fresh reference only;
/// `messages.getMessages` answers with the message carrying `fresh`.
fn serve(
    fake: &MediaFakeRpcClient,
    live: &LiveSession,
    peer: PeerId,
    id: i32,
    stale: tl_enums::MessageMedia,
    fresh: tl_enums::MessageMedia,
    file: &[u8],
) -> Served {
    let served = Served {
        refetched: Arc::new(AtomicBool::new(false)),
        stale_asks: Arc::new(AtomicUsize::new(0)),
    };
    let answer = tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages: vec![tl_message_with(peer, id, fresh)],
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    })
    .to_bytes();
    {
        let (file, refetched, stale_asks) = (
            file.to_vec(),
            Arc::clone(&served.refetched),
            Arc::clone(&served.stale_asks),
        );
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
            Some(Ok(file_answer(&file)))
        }));
    }
    let message =
        tg_api::convert::message_from_tl(&tl_message_with(peer, id, stale), DataUserId(SELF_UID))
            .expect("the synthetic message converts");
    insert_message_into_live(live, message);
    served
}

/// Waits until `loaded` holds for the message's descriptor; the failure names what happened.
fn assert_loads(
    live: &LiveSession,
    peer: PeerId,
    id: i32,
    served: &Served,
    what: &str,
    loaded: impl Fn(&InlineMediaDescriptor) -> bool,
) {
    let ok = wait_until(|| {
        live.inline_media_descriptor(peer, MsgId(id))
            .is_some_and(|d| loaded(&d))
    });
    assert!(
        ok,
        "{what} whose file reference expired never loads (stale asks: {}, message fetched \
         again: {})",
        served.stale_asks.load(Ordering::SeqCst),
        served.refetched.load(Ordering::SeqCst)
    );
    assert!(
        served.refetched.load(Ordering::SeqCst),
        "{what}: the message was asked for again"
    );
    assert!(
        served.stale_asks.load(Ordering::SeqCst) >= 1,
        "{what}: the stale reference was tried first"
    );
}

fn photo_ready(d: &InlineMediaDescriptor, file: &[u8]) -> bool {
    matches!(&d.photo_load, Some(PhotoLoadState::Ready { bytes }) if bytes.as_ref() == file)
}

fn video_ready(d: &InlineMediaDescriptor, file: &[u8]) -> bool {
    matches!(&d.video_data, Some(InlineDocumentData::Bytes(bytes)) if bytes == file)
}

#[test]
fn a_photo_with_an_expired_file_reference_is_fetched_again_and_loads() {
    let (_dir, fake, live, peer) = start_live_session();
    let file = synthetic_file(b"photo", 3_000);
    let served = serve(
        &fake,
        &live,
        peer,
        501,
        photo_media(STALE_REF, file.len()),
        photo_media(FRESH_REF, file.len()),
        &file,
    );
    let _ = live.inline_media_descriptor(peer, MsgId(501));
    assert_loads(&live, peer, 501, &served, "a photo", |d| {
        photo_ready(d, &file)
    });
}

#[test]
fn a_video_thumbnail_with_an_expired_file_reference_is_fetched_again_and_loads() {
    let (_dir, fake, live, peer) = start_live_session();
    let thumb = synthetic_file(b"thumb", 1_500);
    let spec = DocSpec {
        id: 62_001,
        mime: "video/mp4",
        size: 4_000_000,
        attributes: vec![video_attribute(false, false)],
        thumb: Some(thumb.len()),
        round: false,
        voice: false,
    };
    let served = serve(
        &fake,
        &live,
        peer,
        502,
        document_media(&spec, STALE_REF),
        document_media(&spec, FRESH_REF),
        &thumb,
    );
    let _ = live.inline_media_descriptor(peer, MsgId(502));
    assert_loads(&live, peer, 502, &served, "a video thumbnail", |d| {
        photo_ready(d, &thumb)
    });
}

#[test]
fn a_gif_with_an_expired_file_reference_is_fetched_again_and_plays() {
    let (_dir, fake, live, peer) = start_live_session();
    let file = synthetic_file(b"gif-mp4", 2_000);
    let spec = DocSpec {
        id: 62_002,
        mime: "video/mp4",
        size: file.len(),
        attributes: vec![
            tl_enums::DocumentAttribute::Animated,
            video_attribute(false, true),
        ],
        thumb: None,
        round: false,
        voice: false,
    };
    let served = serve(
        &fake,
        &live,
        peer,
        503,
        document_media(&spec, STALE_REF),
        document_media(&spec, FRESH_REF),
        &file,
    );
    let _ = live.inline_media_descriptor(peer, MsgId(503));
    assert_loads(&live, peer, 503, &served, "a GIF", |d| {
        video_ready(d, &file)
    });
}

#[test]
fn a_round_video_with_an_expired_file_reference_is_fetched_again_and_plays() {
    let (_dir, fake, live, peer) = start_live_session();
    if let Ok(mut cache) = live.cache.lock() {
        cache.auto_download.autoplay_round_videos = true;
    }
    let file = synthetic_file(b"round-mp4", 2_000);
    let spec = DocSpec {
        id: 62_003,
        mime: "video/mp4",
        size: file.len(),
        attributes: vec![video_attribute(true, false)],
        thumb: None,
        round: true,
        voice: false,
    };
    let served = serve(
        &fake,
        &live,
        peer,
        504,
        document_media(&spec, STALE_REF),
        document_media(&spec, FRESH_REF),
        &file,
    );
    let _ = live.inline_media_descriptor(peer, MsgId(504));
    assert_loads(&live, peer, 504, &served, "a round video", |d| {
        video_ready(d, &file)
    });
}

/// A voice message is downloaded by its first play (the click), not by the list.
#[test]
fn a_voice_message_with_an_expired_file_reference_is_fetched_again_on_play() {
    let (_dir, fake, live, peer) = start_live_session();
    let file = synthetic_file(b"OggS", 2_500);
    let spec = DocSpec {
        id: 62_004,
        mime: "audio/ogg",
        size: file.len(),
        attributes: vec![tl_enums::DocumentAttribute::Audio(
            tl_types::DocumentAttributeAudio {
                voice: true,
                duration: 3,
                title: None,
                performer: None,
                waveform: Some(vec![1, 2, 3]),
            },
        )],
        thumb: None,
        round: false,
        voice: true,
    };
    let served = serve(
        &fake,
        &live,
        peer,
        505,
        document_media(&spec, STALE_REF),
        document_media(&spec, FRESH_REF),
        &file,
    );
    live.start_media_download(peer, MsgId(505));
    assert_loads(&live, peer, 505, &served, "a voice message", |d| {
        d.audio_bytes.as_deref() == Some(file.as_slice())
    });
}

fn synthetic_file(head: &[u8], len: usize) -> Vec<u8> {
    let mut out = head.to_vec();
    out.extend((0..len).map(|i| (i % 251) as u8));
    out
}
