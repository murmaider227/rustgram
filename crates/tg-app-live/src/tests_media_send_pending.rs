//! TGB-3033: a media send shows its pending bubble at once and uploads in the background.
//! The fake server holds `upload.saveFilePart` until the test lets it go: the send returns
//! before that, the bubble (clock, the media being sent) is in the history meanwhile, and the
//! next history drain turns it into the server message, marks it failed, or removes it when
//! the upload was cancelled. Child module of `tests_upload` for its fake and harness.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use tg_app_core::history::backend::{HistoryLiveUpdate, VoiceAttachment};

use super::*;

/// How long a held request waits before it is answered anyway.
const MAX_HOLD: Duration = Duration::from_secs(10);

/// Longest a send may take on the UI thread while the server is held. The old code waited for
/// the whole upload (at least `MAX_HOLD`).
const UI_CALL_LIMIT: Duration = Duration::from_millis(500);

const SERVER_MSG_ID: i32 = 9_001;

fn hold_until(open: &AtomicBool) {
    let started = Instant::now();
    while !open.load(Ordering::Acquire) && started.elapsed() < MAX_HOLD {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Holds every `upload.saveFilePart` until `open`; `messages.sendMedia` then answers with
/// the server message, or with `send_error` when set.
fn serve_held_upload(
    h: &UploadTestHarness,
    open: &Arc<AtomicBool>,
    send_error: Option<&'static str>,
) {
    let open = Arc::clone(open);
    let (self_uid, bob_uid) = (h.self_uid, h.bob_uid);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            hold_until(&open);
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            if let Some(name) = send_error {
                return Some(Err(InvocationError::Rpc(RpcError {
                    code: 400,
                    name: name.to_owned(),
                    value: None,
                })));
            }
            let upd = make_updates_with_messages(vec![synthetic_tl_message(
                SERVER_MSG_ID,
                bob_uid,
                self_uid,
                true,
                "",
            )]);
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));
}

fn voice_request() -> SendMessageExtended {
    let mut ogg = b"OggS".to_vec();
    ogg.extend(synth_bytes(3_000, 0x31));
    SendMessageExtended {
        media: Some(HistoryMedia::Voice(VoiceAttachment {
            duration_secs: 3,
            waveform_5bit: vec![0x01, 0x02],
            size_bytes: ogg.len() as u64,
            is_unread: false,
            transcription: None,
        })),
        voice_ogg: Some(ogg),
        ..SendMessageExtended::default()
    }
}

/// Sends `request` and checks the call returned at once with a pending local message.
fn send_without_waiting(
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
    assert!(
        crate::convert::to_data_msg(pending.id).is_client(),
        "the pending bubble has a local id, got {:?}",
        pending.id
    );
    pending
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

#[test]
fn a_voice_send_shows_its_pending_bubble_at_once_and_becomes_the_server_message() {
    let h = UploadTestHarness::start();
    let open = Arc::new(AtomicBool::new(false));
    serve_held_upload(&h, &open, None);
    let mut live = h.live.clone();

    let pending = send_without_waiting(&mut live, h.bob_peer, voice_request());
    assert!(
        matches!(pending.media, Some(HistoryMedia::Voice(_))),
        "the bubble shows the voice message being sent: {:?}",
        pending.media
    );

    // The window keeps drawing while the part is held: the chat reads and the uploads bar
    // answer, the bubble is in the history with its media, and nothing is resolved yet.
    let started = Instant::now();
    let slice = HistoryBackend::load_slice_around(&live, h.bob_peer, MsgId::SHOW_AT_END, 50, 0);
    let uploads = HistoryBackend::pending_media_uploads(&live, h.bob_peer);
    let updates = HistoryBackend::drain_history_updates(&mut live, h.bob_peer);
    assert!(started.elapsed() < UI_CALL_LIMIT);
    let shown = slice
        .messages
        .iter()
        .find(|m| m.id == pending.id)
        .expect("the pending bubble is in the history");
    assert!(shown.is_sending);
    assert!(matches!(shown.media, Some(HistoryMedia::Voice(_))));
    assert_eq!(uploads.len(), 1, "the upload shows its progress");
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, HistoryLiveUpdate::LocalMessageSent { .. })),
        "nothing is sent while the upload is held: {updates:?}"
    );

    open.store(true, Ordering::Release);
    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| {
        all.iter()
            .any(|u| matches!(u, HistoryLiveUpdate::LocalMessageSent { .. }))
    });
    let sent = updates.iter().find_map(|u| match u {
        HistoryLiveUpdate::LocalMessageSent { local_id, message } => Some((*local_id, message)),
        _ => None,
    });
    let (local_id, message) = sent.expect("the pending bubble becomes the server message");
    assert_eq!(local_id, pending.id);
    assert_eq!(message.id, MsgId(SERVER_MSG_ID));
    assert!(!message.is_sending);
    assert!(
        HistoryBackend::pending_media_uploads(&live, h.bob_peer).is_empty(),
        "a finished upload leaves the uploads bar"
    );

    // The local message is gone from the history.
    let slice = HistoryBackend::load_slice_around(&live, h.bob_peer, MsgId::SHOW_AT_END, 50, 0);
    assert!(slice.messages.iter().all(|m| m.id != pending.id));

    h.shutdown();
}

#[test]
fn a_failed_media_send_marks_the_pending_bubble_failed() {
    let h = UploadTestHarness::start();
    let open = Arc::new(AtomicBool::new(true));
    serve_held_upload(&h, &open, Some("MEDIA_INVALID"));
    let mut live = h.live.clone();

    let tmp = tempfile::tempdir().expect("tempdir");
    let doc_path = tmp.path().join("notes.pdf");
    std::fs::write(&doc_path, b"%PDF-1.7 pending").expect("write pdf");
    let pending = send_without_waiting(
        &mut live,
        h.bob_peer,
        SendMessageExtended {
            media: Some(HistoryMedia::Document(DocumentAttachment {
                file_name: doc_path.to_string_lossy().into_owned(),
                size_bytes: 16,
                mime_type: "application/pdf".to_owned(),
                thumb_rgb: None,
            })),
            ..SendMessageExtended::default()
        },
    );

    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| {
        all.iter()
            .any(|u| matches!(u, HistoryLiveUpdate::LocalMessageFailed { .. }))
    });
    let failed = updates.iter().find_map(|u| match u {
        HistoryLiveUpdate::LocalMessageFailed { local_id, error } => Some((*local_id, error)),
        _ => None,
    });
    let (local_id, error) = failed.expect("the bubble is marked failed");
    assert_eq!(local_id, pending.id);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("MEDIA_INVALID")),
        "the error is shown: {error:?}"
    );

    // The failed bubble stays in the history with its media, as in tdesktop.
    let slice = HistoryBackend::load_slice_around(&live, h.bob_peer, MsgId::SHOW_AT_END, 50, 0);
    let shown = slice
        .messages
        .iter()
        .find(|m| m.id == pending.id)
        .expect("the failed bubble stays");
    assert!(shown.extra.send_failed);
    assert!(matches!(shown.media, Some(HistoryMedia::Document(_))));

    h.shutdown();
}

#[test]
fn a_cancelled_media_upload_removes_the_pending_bubble_without_an_error() {
    let h = UploadTestHarness::start();
    let open = Arc::new(AtomicBool::new(false));
    serve_held_upload(&h, &open, None);
    let mut live = h.live.clone();

    let pending = send_without_waiting(&mut live, h.bob_peer, voice_request());
    let uploads = HistoryBackend::pending_media_uploads(&live, h.bob_peer);
    assert_eq!(uploads.len(), 1);
    assert!(HistoryBackend::cancel_media_upload(
        &mut live,
        uploads[0].upload_id
    ));
    open.store(true, Ordering::Release);

    assert!(live.wait_media_sends(TEST_TIMEOUT));
    let updates = drain_until(&mut live, h.bob_peer, |all| {
        all.iter()
            .any(|u| matches!(u, HistoryLiveUpdate::MessagesDeleted(_)))
    });
    assert!(
        updates.iter().any(
            |u| matches!(u, HistoryLiveUpdate::MessagesDeleted(ids) if ids.contains(&pending.id))
        ),
        "the cancelled bubble is removed: {updates:?}"
    );
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, HistoryLiveUpdate::LocalMessageFailed { .. })),
        "a cancel is not a failure: {updates:?}"
    );
    assert!(
        h.fake
            .recorded_calls_of(tl_fns::messages::SendMedia::CONSTRUCTOR_ID)
            .is_empty(),
        "a cancelled upload is never sent"
    );

    h.shutdown();
}
