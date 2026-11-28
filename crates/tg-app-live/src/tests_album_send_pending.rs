//! TGB-3130: an album (and a scheduled file) is sent without waiting on the UI thread and
//! joins the ordered media queue of single files (TGB-3168).
//!
//! tdesktop shows a pending item for every file of the album at once (`ApiWrap::sendFiles`),
//! uploads them, and sends one `messages.sendMultiMedia` when all files still in the album are
//! uploaded (`sendAlbumIfReady`); a cancelled file leaves the album (`sendAlbumWithCancelled`),
//! one file left goes as `messages.sendMedia`, and a failed `messages.sendMultiMedia` fails
//! every item. The fake server holds `upload.saveFilePart` until the test lets it go and
//! records every send, so the tests see the order of the sends and that each goes once. Child
//! module of `tests_upload` for its fake and harness.

use std::sync::atomic::{AtomicBool, AtomicI32};
use std::time::Instant;

use tg_app_core::history::backend::{AlbumItem, HistoryLiveUpdate};

use super::*;

/// How long a held request waits before it is answered anyway.
const MAX_HOLD: Duration = Duration::from_secs(10);

/// Longest a send may take on the UI thread while the server is held.
const UI_CALL_LIMIT: Duration = Duration::from_millis(500);

const FIRST_SERVER_ID: i32 = 9_101;

/// What the fake server saw, in the order it saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sent {
    /// `messages.sendMedia` of a file (its name) or of an album photo (`photo:<id>`).
    Media(String),
    /// `messages.sendMultiMedia` with the photo ids of its files, in order.
    Album(Vec<i64>),
}

struct AlbumServer {
    /// Opens the held uploads.
    open: Arc<AtomicBool>,
    sent: Arc<Mutex<Vec<Sent>>>,
    /// File name of each `messages.uploadMedia`, in order (photo id = 7001 + index).
    uploaded: Arc<Mutex<Vec<String>>>,
    /// Random id of each file of the last album, in its order.
    album_random_ids: Arc<Mutex<Vec<i64>>>,
}

/// Holds every `upload.saveFilePart` whose bytes start with `hold_prefix` (all of them when
/// `None`) until `open`. Albums are answered with `updateMessageID` in reverse order, so the
/// server ids must be matched by random id; `album_error` fails `messages.sendMultiMedia`.
fn serve(
    h: &UploadTestHarness,
    hold_prefix: Option<&'static [u8]>,
    album_error: Option<&'static str>,
) -> AlbumServer {
    let server = AlbumServer {
        open: Arc::new(AtomicBool::new(false)),
        sent: Arc::new(Mutex::new(Vec::new())),
        uploaded: Arc::new(Mutex::new(Vec::new())),
        album_random_ids: Arc::new(Mutex::new(Vec::new())),
    };
    let next_id = Arc::new(AtomicI32::new(FIRST_SERVER_ID));
    let (open, sent, uploaded, album_random_ids) = (
        Arc::clone(&server.open),
        Arc::clone(&server.sent),
        Arc::clone(&server.uploaded),
        Arc::clone(&server.album_random_ids),
    );
    let (self_uid, bob_uid) = (h.self_uid, h.bob_uid);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            let part = tl_fns::upload::SaveFilePart::from_bytes(raw).ok()?;
            if hold_prefix.is_none_or(|p| part.bytes.starts_with(p)) {
                let started = Instant::now();
                while !open.load(Ordering::Acquire) && started.elapsed() < MAX_HOLD {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::UploadMedia::CONSTRUCTOR_ID {
            let req = tl_fns::messages::UploadMedia::from_bytes(raw).ok()?;
            let name = match req.media {
                tl_enums::InputMedia::UploadedPhoto(p) => match p.file {
                    tl_enums::InputFile::InputFile(f) => f.name,
                    tl_enums::InputFile::Big(f) => f.name,
                    _ => String::new(),
                },
                _ => String::new(),
            };
            let mut names = uploaded.lock().expect("uploaded");
            let id = 7_001 + names.len() as i64;
            names.push(name);
            let media = tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
                spoiler: false,
                live_photo: false,
                photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                    has_stickers: false,
                    id,
                    access_hash: id * 3,
                    file_reference: vec![1],
                    date: NOW_SECS,
                    sizes: Vec::new(),
                    video_sizes: None,
                    dc_id: 2,
                })),
                ttl_seconds: None,
                video: None,
            });
            return Some(Ok(media.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            let req = tl_fns::messages::SendMedia::from_bytes(raw).ok()?;
            let what = match req.media {
                tl_enums::InputMedia::UploadedDocument(doc) => doc
                    .attributes
                    .into_iter()
                    .find_map(|a| match a {
                        tl_enums::DocumentAttribute::Filename(f) => Some(f.file_name),
                        _ => None,
                    })
                    .unwrap_or_default(),
                tl_enums::InputMedia::Photo(p) => match p.id {
                    tl_enums::InputPhoto::InputPhoto(photo) => format!("photo:{}", photo.id),
                    tl_enums::InputPhoto::Empty => "photo:?".to_owned(),
                },
                tl_enums::InputMedia::UploadedPhoto(_) => "uploaded photo".to_owned(),
                _ => "other".to_owned(),
            };
            sent.lock().expect("sent").push(Sent::Media(what));
            let id = next_id.fetch_add(1, Ordering::AcqRel);
            let upd = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                        id,
                        random_id: req.random_id,
                    }),
                    tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                        message: synthetic_tl_message(id, bob_uid, self_uid, true, ""),
                        pts: 300 + id,
                        pts_count: 1,
                    }),
                ],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 0,
            });
            return Some(Ok(upd.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID {
            let req = tl_fns::messages::SendMultiMedia::from_bytes(raw).ok()?;
            let mut photo_ids = Vec::new();
            let mut random_ids = Vec::new();
            for tl_enums::InputSingleMedia::InputSingleMedia(item) in &req.multi_media {
                random_ids.push(item.random_id);
                if let tl_enums::InputMedia::Photo(p) = &item.media
                    && let tl_enums::InputPhoto::InputPhoto(photo) = &p.id
                {
                    photo_ids.push(photo.id);
                }
            }
            sent.lock().expect("sent").push(Sent::Album(photo_ids));
            *album_random_ids.lock().expect("random ids") = random_ids.clone();
            if let Some(name) = album_error {
                return Some(Err(InvocationError::Rpc(RpcError {
                    code: 400,
                    name: name.to_owned(),
                    value: None,
                })));
            }
            let ids: Vec<i32> = random_ids
                .iter()
                .map(|_| next_id.fetch_add(1, Ordering::AcqRel))
                .collect();
            let mut updates = Vec::new();
            // Reverse order: the server ids are matched by random id, not by position.
            for (&id, &random_id) in ids.iter().zip(&random_ids).rev() {
                updates.push(tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                    id,
                    random_id,
                }));
                updates.push(tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message: synthetic_tl_message(id, bob_uid, self_uid, true, ""),
                    pts: 300 + id,
                    pts_count: 1,
                }));
            }
            let upd = tl_enums::Updates::Updates(tl_types::Updates {
                updates,
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 0,
            });
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));
    server
}

impl AlbumServer {
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().expect("sent").clone()
    }

    fn uploaded(&self) -> Vec<String> {
        self.uploaded.lock().expect("uploaded").clone()
    }

    fn open(&self) {
        self.open.store(true, Ordering::Release);
    }
}

/// An album of `names.len()` photos whose bytes are staged under those names.
fn album_request(live: &LiveSession, names: &[&str]) -> SendMessageExtended {
    let items = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            live.stage_media_bytes(*name, encode_test_png(4 + i as u32, 4));
            AlbumItem {
                msg_id: MsgId(9_800 + i as i32),
                width: 320,
                height: 240,
                stripped_thumb: None,
                fallback_rgb: ([0x40, 0x80, 0xC0], [0x20, 0x40, 0x60]),
                video_duration_secs: None,
                label: (*name).to_owned(),
            }
        })
        .collect();
    SendMessageExtended {
        text: TextWithEntities::simple("Album caption"),
        media: Some(HistoryMedia::Album(items)),
        ..SendMessageExtended::default()
    }
}

fn document_request(dir: &std::path::Path, name: &str, head: &[u8]) -> SendMessageExtended {
    let path = dir.join(name);
    let mut bytes = head.to_vec();
    bytes.extend(synth_bytes(2_000, 0x41));
    std::fs::write(&path, &bytes).expect("write file");
    SendMessageExtended {
        media: Some(HistoryMedia::Document(DocumentAttachment {
            file_name: path.to_string_lossy().into_owned(),
            size_bytes: bytes.len() as u64,
            mime_type: "application/pdf".to_owned(),
            thumb_rgb: None,
        })),
        ..SendMessageExtended::default()
    }
}

/// Sends `request` and checks the call returned at once.
fn send_at_once(
    live: &mut LiveSession,
    peer: PeerId,
    request: SendMessageExtended,
) -> tg_app_core::history::backend::HistoryMessage {
    let started = Instant::now();
    let pending =
        HistoryBackend::send_message_extended(live, peer, request).expect("send is accepted");
    let took = started.elapsed();
    assert!(
        took < UI_CALL_LIMIT,
        "the send waited {took:?} for the upload on the UI thread"
    );
    assert!(pending.is_sending, "the bubble shows the clock");
    pending
}

/// The local (pending or failed) messages in the history, oldest first.
fn local_bubbles(
    live: &LiveSession,
    peer: PeerId,
) -> Vec<tg_app_core::history::backend::HistoryMessage> {
    HistoryBackend::load_slice_around(live, peer, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .into_iter()
        .filter(|m| crate::convert::to_data_msg(m.id).is_client())
        .collect()
}

fn drain_until(
    live: &mut LiveSession,
    peer: PeerId,
    done: impl Fn(&[HistoryLiveUpdate]) -> bool,
) -> Vec<HistoryLiveUpdate> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(HistoryBackend::drain_history_updates(live, peer));
        if done(&all) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    all
}

fn sent_pairs(updates: &[HistoryLiveUpdate]) -> Vec<(MsgId, MsgId)> {
    updates
        .iter()
        .filter_map(|u| match u {
            HistoryLiveUpdate::LocalMessageSent { local_id, message } => {
                Some((*local_id, message.id))
            }
            _ => None,
        })
        .collect()
}

fn count_sent(updates: &[HistoryLiveUpdate]) -> usize {
    sent_pairs(updates).len()
}

/// Nothing reaches the server for a while (the held upload is not over).
fn assert_nothing_sent_for_a_while(server: &AlbumServer) {
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        assert!(server.sent().is_empty(), "sent early: {:?}", server.sent());
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_album_shows_every_pending_bubble_at_once_and_becomes_the_server_messages() {
    let h = UploadTestHarness::start();
    let server = serve(&h, None, None);
    let mut live = h.live.clone();

    let first = send_at_once(
        &mut live,
        h.bob_peer,
        album_request(&h.live, &["a.jpg", "b.jpg", "c.jpg"]),
    );
    assert!(matches!(first.media, Some(HistoryMedia::Photo(_))));

    // The open chat gets every bubble from its next drain (the view does not reload the
    // slice), each with its photo; the caller already shows the first one.
    let drained = drain_until(&mut live, h.bob_peer, |all| {
        all.iter()
            .filter(|u| matches!(u, HistoryLiveUpdate::MessageAdded(_)))
            .count()
            >= 3
    });
    let mut drained_bubbles: Vec<(MsgId, String)> = drained
        .iter()
        .filter_map(|u| match u {
            HistoryLiveUpdate::MessageAdded(m) => match &m.media {
                Some(HistoryMedia::Photo(p)) if m.is_sending => Some((m.id, p.file_name.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    drained_bubbles.sort();

    // While the uploads are held: a bubble with its photo for every file, a row per file in
    // the uploads bar, nothing sent.
    let started = Instant::now();
    let pending = local_bubbles(&live, h.bob_peer);
    let uploads = HistoryBackend::pending_media_uploads(&live, h.bob_peer);
    assert!(started.elapsed() < UI_CALL_LIMIT);
    assert_eq!(pending.len(), 3, "a bubble per file: {pending:?}");
    assert_eq!(pending[0].id, first.id);
    assert!(pending.iter().all(|m| m.is_sending));
    let names: Vec<String> = pending
        .iter()
        .map(|m| match &m.media {
            Some(HistoryMedia::Photo(p)) => p.file_name.clone(),
            other => panic!("the bubble shows its photo: {other:?}"),
        })
        .collect();
    assert_eq!(names, ["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(pending[0].text.text, "Album caption");
    assert_eq!(uploads.len(), 3, "each file shows its upload");
    assert_eq!(
        drained_bubbles,
        [
            (pending[0].id, "a.jpg".to_owned()),
            (pending[1].id, "b.jpg".to_owned()),
            (pending[2].id, "c.jpg".to_owned()),
        ],
        "the drain brings every bubble: {drained:?}"
    );
    assert_nothing_sent_for_a_while(&server);

    server.open();
    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| count_sent(all) == 3);

    // One `messages.sendMultiMedia`, its files in the album's order.
    assert_eq!(server.uploaded(), ["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(server.sent(), [Sent::Album(vec![7_001, 7_002, 7_003])]);
    // Each bubble becomes the message of its own random id, once.
    let mut pairs = sent_pairs(&updates);
    pairs.sort();
    assert_eq!(
        pairs,
        [
            (pending[0].id, MsgId(FIRST_SERVER_ID)),
            (pending[1].id, MsgId(FIRST_SERVER_ID + 1)),
            (pending[2].id, MsgId(FIRST_SERVER_ID + 2)),
        ]
    );
    assert!(local_bubbles(&live, h.bob_peer).is_empty());
    assert!(HistoryBackend::pending_media_uploads(&live, h.bob_peer).is_empty());

    h.shutdown();
}

/// A file sent before the album and one sent after it reach the server in that order, each
/// exactly once, also when the first one uploads longest (`Uploader` queue order and
/// `afterRequest`). Before, the album went at once on the UI thread and overtook the file.
#[test]
fn media_sent_before_and_after_an_album_reach_the_server_in_order() {
    let h = UploadTestHarness::start();
    let server = serve(&h, Some(b"%FIRST"), None);
    let mut live = h.live.clone();
    let tmp = tempfile::tempdir().expect("tempdir");

    send_at_once(
        &mut live,
        h.bob_peer,
        document_request(tmp.path(), "first.pdf", b"%FIRST"),
    );
    send_at_once(
        &mut live,
        h.bob_peer,
        album_request(&h.live, &["a.jpg", "b.jpg"]),
    );
    send_at_once(
        &mut live,
        h.bob_peer,
        document_request(tmp.path(), "last.pdf", b"%LAST"),
    );
    assert_eq!(local_bubbles(&live, h.bob_peer).len(), 4);
    assert_nothing_sent_for_a_while(&server);

    server.open();
    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| count_sent(all) == 4);
    assert_eq!(
        server.sent(),
        [
            Sent::Media("first.pdf".to_owned()),
            Sent::Album(vec![7_001, 7_002]),
            Sent::Media("last.pdf".to_owned()),
        ],
        "server order, each once"
    );
    assert_eq!(count_sent(&updates), 4);
    assert!(local_bubbles(&live, h.bob_peer).is_empty());

    h.shutdown();
}

/// A file cancelled while the album uploads leaves the album without an error
/// (`sendAlbumWithCancelled`); the others are sent together.
#[test]
fn a_file_cancelled_during_the_upload_leaves_the_album_and_the_rest_is_sent() {
    let h = UploadTestHarness::start();
    let server = serve(&h, None, None);
    let mut live = h.live.clone();

    send_at_once(
        &mut live,
        h.bob_peer,
        album_request(&h.live, &["a.jpg", "b.jpg", "c.jpg"]),
    );
    let pending = local_bubbles(&live, h.bob_peer);
    let uploads = HistoryBackend::pending_media_uploads(&live, h.bob_peer);
    assert_eq!(uploads.len(), 3);
    assert!(HistoryBackend::cancel_media_upload(
        &mut live,
        uploads[1].upload_id
    ));
    server.open();

    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| {
        count_sent(all) == 2
            && all
                .iter()
                .any(|u| matches!(u, HistoryLiveUpdate::MessagesDeleted(_)))
    });
    assert_eq!(server.uploaded(), ["a.jpg", "c.jpg"]);
    assert_eq!(server.sent(), [Sent::Album(vec![7_001, 7_002])]);
    assert!(
        updates.iter().any(|u| matches!(
            u,
            HistoryLiveUpdate::MessagesDeleted(ids) if ids.contains(&pending[1].id)
        )),
        "the cancelled bubble is removed: {updates:?}"
    );
    let sent: Vec<MsgId> = sent_pairs(&updates).iter().map(|p| p.0).collect();
    assert!(sent.contains(&pending[0].id) && sent.contains(&pending[2].id));
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, HistoryLiveUpdate::LocalMessageFailed { .. })),
        "a cancel is not a failure: {updates:?}"
    );

    h.shutdown();
}

/// With one file left the album goes as `messages.sendMedia` (`apiwrap.cpp:5660-5668`).
#[test]
fn an_album_with_one_file_left_is_sent_as_a_single_media() {
    let h = UploadTestHarness::start();
    let server = serve(&h, None, None);
    let mut live = h.live.clone();

    send_at_once(
        &mut live,
        h.bob_peer,
        album_request(&h.live, &["a.jpg", "b.jpg", "c.jpg"]),
    );
    let pending = local_bubbles(&live, h.bob_peer);
    let uploads = HistoryBackend::pending_media_uploads(&live, h.bob_peer);
    assert!(HistoryBackend::cancel_media_upload(
        &mut live,
        uploads[0].upload_id
    ));
    assert!(HistoryBackend::cancel_media_upload(
        &mut live,
        uploads[2].upload_id
    ));
    server.open();

    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| count_sent(all) == 1);
    assert_eq!(server.sent(), [Sent::Media("photo:7001".to_owned())]);
    assert_eq!(
        sent_pairs(&updates),
        [(pending[1].id, MsgId(FIRST_SERVER_ID))]
    );

    h.shutdown();
}

/// A failed `messages.sendMultiMedia` marks every bubble failed (`sendMessageFail` per item);
/// the media queue goes on with the next file.
#[test]
fn a_failed_album_marks_every_bubble_failed_and_the_queue_goes_on() {
    let h = UploadTestHarness::start();
    let server = serve(&h, Some(b"%NOTHING"), Some("MEDIA_INVALID"));
    let mut live = h.live.clone();
    let tmp = tempfile::tempdir().expect("tempdir");

    send_at_once(
        &mut live,
        h.bob_peer,
        album_request(&h.live, &["a.jpg", "b.jpg"]),
    );
    let pending = local_bubbles(&live, h.bob_peer);
    send_at_once(
        &mut live,
        h.bob_peer,
        document_request(tmp.path(), "next.pdf", b"%NEXT"),
    );

    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| {
        count_sent(all) == 1
            && all
                .iter()
                .filter(|u| matches!(u, HistoryLiveUpdate::LocalMessageFailed { .. }))
                .count()
                == 2
    });
    let failed: Vec<MsgId> = updates
        .iter()
        .filter_map(|u| match u {
            HistoryLiveUpdate::LocalMessageFailed { local_id, .. } => Some(*local_id),
            _ => None,
        })
        .collect();
    assert_eq!(failed, [pending[0].id, pending[1].id]);
    assert!(
        updates.iter().any(|u| matches!(
            u,
            HistoryLiveUpdate::LocalMessageFailed { error: Some(e), .. }
                if e.contains("MEDIA_INVALID")
        )),
        "the error is shown: {updates:?}"
    );
    assert_eq!(
        server.sent(),
        [
            Sent::Album(vec![7_001, 7_002]),
            Sent::Media("next.pdf".to_owned()),
        ]
    );
    let left = local_bubbles(&live, h.bob_peer);
    assert_eq!(left.len(), 2, "the failed bubbles stay");
    assert!(left.iter().all(|m| m.extra.send_failed));

    h.shutdown();
}

/// A scheduled album and a scheduled photo return at once, keep the queue order, and put no
/// bubble in the history (their place is the scheduled section).
#[test]
fn scheduled_media_return_at_once_and_stay_out_of_the_history() {
    let h = UploadTestHarness::start();
    let server = serve(&h, None, None);
    let mut live = h.live.clone();
    let when = NOW_SECS + 3_600;

    let mut album = album_request(&h.live, &["a.jpg", "b.jpg"]);
    album.schedule_date = Some(when);
    send_at_once(&mut live, h.bob_peer, album);
    live.stage_media_bytes("single.jpg", encode_test_png(6, 6));
    send_at_once(
        &mut live,
        h.bob_peer,
        SendMessageExtended {
            media: Some(HistoryMedia::Photo(PhotoAttachment {
                width: 6,
                height: 6,
                stripped_thumb: None,
                fallback_rgb: ([0, 0, 0], [0, 0, 0]),
                file_name: "single.jpg".to_owned(),
                size_bytes: 100,
            })),
            schedule_date: Some(when),
            ..SendMessageExtended::default()
        },
    );
    assert!(local_bubbles(&live, h.bob_peer).is_empty());
    assert_nothing_sent_for_a_while(&server);

    server.open();
    assert!(live.wait_media_sends(TEST_TIMEOUT));
    assert_eq!(
        server.sent(),
        [
            Sent::Album(vec![7_001, 7_002]),
            Sent::Media("uploaded photo".to_owned()),
        ]
    );
    let multi = h
        .fake
        .recorded_calls_of(tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID);
    let multi = tl_fns::messages::SendMultiMedia::from_bytes(&multi[0]).expect("SendMultiMedia");
    assert_eq!(multi.schedule_date, Some(when));
    let single = h
        .fake
        .recorded_calls_of(tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    let single = tl_fns::messages::SendMedia::from_bytes(&single[0]).expect("SendMedia");
    assert_eq!(single.schedule_date, Some(when));
    assert!(local_bubbles(&live, h.bob_peer).is_empty());

    h.shutdown();
}
