//! Integration and unit tests for live media upload, album sending, Terms of Service,
//! clipboard image copy, media overlay forwarding, and story upload (`stories.sendStory`).
//!
//! All tests run exclusively against synthetic users/media and an in-process `FakeRpcClient`.
//! No real Telegram network connections or credentials are ever used.

use std::collections::{BTreeMap, VecDeque};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use tg_api::rpc::RpcError;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_crypto::hash::md5_hex;
use tg_data::peer::GeoPoint;
use tg_data::{
    FullStoryId, Message, MessageContent, MessageFlags, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, PhotoData, PhotoId, ReactionId, TextWithEntities as DataText,
    UserId as DataUserId,
};
use tg_media::transfer::download::CancelToken;
use tg_media::transfer::upload::{
    BIG_FILE_THRESHOLD, PHOTO_UPLOAD_PART_SIZE, UPLOAD_PART_SIZE_32K, UPLOAD_PART_SIZE_64K,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stories::{
    RectF, StoriesStore, StoryArea, StoryLocation, StoryPrivacy, SuggestedReaction, UrlArea,
    WeatherArea,
};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::stories::{SendStoryRequest, StoryPrivacyInput, StoryUploadMedia};
use crate::upload::{
    AlbumUploadItem, LiveUploadConfig, OutgoingMedia, SendMediaOptions, UploadError,
    decode_image_for_clipboard, parse_missing_file_part,
};
use tg_app_core::history::backend::{
    AlbumItem, DocumentAttachment, HistoryBackend, HistoryMedia, PhotoAttachment,
    SendMessageExtended,
};
use tg_app_core::history::sections_model::ReplyQuote;
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::media::backend::{MediaBackend, MediaItemId};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_core::stories::backend::StoriesBackend;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const NOW_SECS: i32 = 1_750_000_000;

type RecordedCall = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn recorded_calls_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes.clone()));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };

        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn synthetic_tl_message(
    id: i32,
    peer_uid: i64,
    from_uid: i64,
    out: bool,
    text: &str,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: NOW_SECS,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

fn make_updates_with_messages(msgs: Vec<tl_enums::Message>) -> tl_enums::Updates {
    let updates = msgs
        .into_iter()
        .enumerate()
        .map(|(i, message)| {
            tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message,
                pts: 200 + i as i32,
                pts_count: 1,
            })
        })
        .collect();
    tl_enums::Updates::Updates(tl_types::Updates {
        updates,
        users: Vec::new(),
        chats: Vec::new(),
        date: NOW_SECS,
        seq: 1,
    })
}

struct UploadTestHarness {
    self_uid: i64,
    bob_uid: i64,
    carol_uid: i64,
    bob_peer: PeerId,
    carol_peer: PeerId,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    live: LiveSession,
}

impl UploadTestHarness {
    fn start() -> Self {
        let self_uid = 810_001i64;
        let bob_uid = 810_002i64;
        let carol_uid = 810_003i64;
        let self_user = synthetic_tl_user(self_uid, "Alice", "alice_u", true);
        let bob_user = synthetic_tl_user(bob_uid, "Bob", "bob_u", false);
        let carol_user = synthetic_tl_user(carol_uid, "Carol", "carol_u", false);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user)],
        );

        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: true,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                crate::ErasedRpcClient::new(fake.clone()),
                cfg,
            )
            .expect("start runtime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("ready");

        // Seed Bob and Carol into the session peer table.
        let session_arc = Arc::clone(runtime.session());
        runtime.tokio_handle().block_on(async move {
            let mut s = session_arc.write().await;
            tg_api::convert::apply_tl_users_and_chats(
                &mut s,
                &[
                    tl_enums::User::User(bob_user),
                    tl_enums::User::User(carol_user),
                ],
                &[],
            );
        });

        let live = LiveSession::new(Arc::clone(&runtime), 1);
        live.set_upload_config(LiveUploadConfig {
            max_flood_wait: Duration::from_millis(5),
            ..LiveUploadConfig::default()
        });

        Self {
            self_uid,
            bob_uid,
            carol_uid,
            bob_peer: PeerId::from_user(bob_uid as u64),
            carol_peer: PeerId::from_user(carol_uid as u64),
            fake,
            runtime,
            live,
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

fn synth_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed.wrapping_add((i.wrapping_mul(31) ^ (i >> 7)) as u8))
        .collect()
}

fn encode_test_png(w: u32, h: u32) -> Vec<u8> {
    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.put_pixel(x, y, Rgba([(x * 40) as u8, (y * 60) as u8, 200, 255]));
        }
    }
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

// =============================================================================
// 1. Part ordering, part sizes, small vs big (>10 MB), MD5 for small files
// =============================================================================

#[test]
fn uploader_small_photo_and_document_part_sizes_ordering_and_md5() {
    let h = UploadTestHarness::start();

    h.fake.set_dynamic_handler(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    // Small photo: 70 KiB -> 3 parts of 32 KiB, 32 KiB, 6 KiB; `.jpg` suffix enforced; MD5 present.
    let photo_data = synth_bytes(70 * 1024, 0x11);
    let expected_md5 = md5_hex(&photo_data);
    let mut photo_cfg = h.live.upload_config();
    photo_cfg.is_photo = true;
    photo_cfg.explicit_file_id = Some(42_001);
    photo_cfg.max_sessions = 4;

    let mut progress_points = Vec::new();
    let res = h
        .live
        .upload_file_bytes(
            "sunset_frame",
            photo_data.clone(),
            photo_cfg,
            &CancelToken::new(),
            |p| progress_points.push((p.uploaded, p.total)),
        )
        .expect("photo upload");

    assert_eq!(res.plan.part_size, PHOTO_UPLOAD_PART_SIZE);
    assert_eq!(res.plan.total_parts, 3);
    assert!(!res.plan.is_big);
    match &res.input_file {
        tl_enums::InputFile::InputFile(f) => {
            assert_eq!(f.id, 42_001);
            assert_eq!(f.parts, 3);
            assert_eq!(f.name, "sunset_frame.jpg");
            assert_eq!(f.md5_checksum, expected_md5);
        }
        other => panic!("expected InputFile::InputFile, got {other:?}"),
    }

    let save_calls = h
        .fake
        .recorded_calls_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    assert_eq!(save_calls.len(), 3);
    let mut parts_by_idx = BTreeMap::new();
    for raw in &save_calls {
        let req = tl_fns::upload::SaveFilePart::from_bytes(raw).expect("decode SaveFilePart");
        assert_eq!(req.file_id, 42_001);
        parts_by_idx.insert(req.file_part, req.bytes);
    }
    assert_eq!(parts_by_idx[&0].len(), 32 * 1024);
    assert_eq!(parts_by_idx[&1].len(), 32 * 1024);
    assert_eq!(parts_by_idx[&2].len(), 6 * 1024);
    let reassembled: Vec<u8> = parts_by_idx.into_values().flatten().collect();
    assert_eq!(reassembled, photo_data);
    assert_eq!(progress_points.first().copied(), Some((0, 70 * 1024)));
    assert_eq!(
        progress_points.last().copied(),
        Some((70 * 1024, 70 * 1024))
    );

    // Medium small document: 1.5 MiB -> 64 KiB parts (24 parts), still `upload.saveFilePart` (<= 10 MiB).
    let doc_data = synth_bytes(1536 * 1024, 0x33);
    let expected_doc_md5 = md5_hex(&doc_data);
    let mut doc_cfg = h.live.upload_config();
    doc_cfg.is_photo = false;
    doc_cfg.explicit_file_id = Some(42_002);
    let doc_res = h
        .live
        .upload_file_bytes("report.pdf", doc_data, doc_cfg, &CancelToken::new(), |_| {})
        .expect("small 1.5 MiB document upload");

    assert_eq!(doc_res.plan.part_size, UPLOAD_PART_SIZE_64K);
    assert_eq!(doc_res.plan.total_parts, 24);
    assert!(!doc_res.plan.is_big);
    match doc_res.input_file {
        tl_enums::InputFile::InputFile(f) => {
            assert_eq!(f.id, 42_002);
            assert_eq!(f.parts, 24);
            assert_eq!(f.name, "report.pdf");
            assert_eq!(f.md5_checksum, expected_doc_md5);
        }
        other => panic!("expected InputFile::InputFile for 1.5 MiB doc, got {other:?}"),
    }

    h.shutdown();
}

#[test]
fn uploader_big_file_above_10mb_uses_save_big_file_part_without_md5() {
    let h = UploadTestHarness::start();

    h.fake.set_dynamic_handler(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::upload::SaveBigFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    // 10 MiB + 1 KiB strictly crosses BIG_FILE_THRESHOLD (10 MiB).
    let big_len = (BIG_FILE_THRESHOLD as usize) + 1024;
    let big_data = synth_bytes(big_len, 0x55);
    let mut cfg = h.live.upload_config();
    cfg.is_photo = false;
    cfg.explicit_file_id = Some(99_001);

    let res = h
        .live
        .upload_file_bytes("archive.bin", big_data, cfg, &CancelToken::new(), |_| {})
        .expect("big file upload");

    // For <= 32 MiB, part size is 64 KiB -> (10*1024*1024 + 1024) / 65536 = 160 full + 1 tail = 161 parts.
    assert!(res.plan.is_big);
    assert_eq!(res.plan.part_size, UPLOAD_PART_SIZE_64K);
    assert_eq!(res.plan.total_parts, 161);
    match res.input_file {
        tl_enums::InputFile::Big(f) => {
            assert_eq!(f.id, 99_001);
            assert_eq!(f.parts, 161);
            assert_eq!(f.name, "archive.bin");
        }
        other => panic!("expected InputFile::Big above 10 MiB, got {other:?}"),
    }

    assert!(
        h.fake
            .recorded_calls_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID)
            .is_empty(),
        "big files must not use upload.saveFilePart"
    );
    let big_calls = h
        .fake
        .recorded_calls_of(tl_fns::upload::SaveBigFilePart::CONSTRUCTOR_ID);
    assert_eq!(big_calls.len(), 161);
    let mut seen_parts = BTreeMap::new();
    for raw in &big_calls {
        let req = tl_fns::upload::SaveBigFilePart::from_bytes(raw).expect("decode SaveBigFilePart");
        assert_eq!(req.file_id, 99_001);
        assert_eq!(req.file_total_parts, 161);
        seen_parts.insert(req.file_part, req.bytes.len());
    }
    assert_eq!(seen_parts.len(), 161);
    assert_eq!(seen_parts[&0], UPLOAD_PART_SIZE_64K as usize);
    assert_eq!(seen_parts[&160], 1024);

    h.shutdown();
}

#[test]
fn uploader_retries_flood_wait_and_file_part_missing_and_honors_cancellation() {
    assert_eq!(parse_missing_file_part("FILE_PART_0_MISSING"), Some(0));
    assert_eq!(parse_missing_file_part("FILE_PART_17_MISSING"), Some(17));
    assert_eq!(parse_missing_file_part("FLOOD_WAIT_5"), None);

    let h = UploadTestHarness::start();
    let flood_injected = Arc::new(AtomicUsize::new(0));
    let missing_injected = Arc::new(AtomicUsize::new(0));
    let self_uid = h.self_uid;
    let bob_uid = h.bob_uid;

    let flood_flag = Arc::clone(&flood_injected);
    let missing_flag = Arc::clone(&missing_injected);
    let live_observer = h.live.clone();
    let bob_peer = h.bob_peer;
    let observed_media_progress: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = Arc::clone(&observed_media_progress);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            let uploads = HistoryBackend::pending_media_uploads(&live_observer, bob_peer);
            if let Some(u) = uploads.first() {
                progress_sink
                    .lock()
                    .expect("lock media progress")
                    .push(u.progress());
            }
            let req = tl_fns::upload::SaveFilePart::from_bytes(raw).expect("SaveFilePart");
            if req.file_part == 1 && flood_flag.fetch_add(1, Ordering::SeqCst) == 0 {
                return Some(Err(InvocationError::Rpc(RpcError::new(
                    420,
                    "FLOOD_WAIT_1",
                ))));
            }
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            let uploads = HistoryBackend::pending_media_uploads(&live_observer, bob_peer);
            if let Some(u) = uploads.first() {
                progress_sink
                    .lock()
                    .expect("lock media progress")
                    .push(u.progress());
            }
            if missing_flag.fetch_add(1, Ordering::SeqCst) == 0 {
                return Some(Err(InvocationError::Rpc(RpcError::new(
                    400,
                    "FILE_PART_0_MISSING",
                ))));
            }
            let upd = make_updates_with_messages(vec![synthetic_tl_message(
                301, bob_uid, self_uid, true, "Retried!",
            )]);
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));

    let mut live = h.live.clone();
    let mut seq_cfg = live.upload_config();
    seq_cfg.max_sessions = 1;
    live.set_upload_config(seq_cfg);
    let payload = synth_bytes(UPLOAD_PART_SIZE_32K as usize + 4096, 0x77);
    let msg = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Photo {
                bytes: payload,
                file_name: "retry_photo.jpg".to_owned(),
                spoiler: false,
                ttl_seconds: None,
            },
            SendMediaOptions {
                caption: TextWithEntities::simple("Retried!"),
                ..SendMediaOptions::default()
            },
        )
        .expect("upload with FLOOD_WAIT and FILE_PART_0_MISSING retries should succeed");

    assert_eq!(msg.id, MsgId(301));
    let media_progress = observed_media_progress
        .lock()
        .expect("lock media progress")
        .clone();
    assert!(
        media_progress.contains(&1.0),
        "expected HistoryBackend::pending_media_uploads to reach 1.0, got {media_progress:?}"
    );
    assert!(
        media_progress.iter().any(|&p| p < 1.0),
        "expected HistoryBackend::pending_media_uploads < 1.0 mid-upload, got {media_progress:?}"
    );
    // 2 initial parts + 1 FLOOD_WAIT retry on part 1 + 1 FILE_PART_0_MISSING re-upload on part 0 = 4.
    let save_calls = h
        .fake
        .recorded_calls_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    assert_eq!(save_calls.len(), 4);
    assert_eq!(
        h.fake
            .recorded_calls_of(tl_fns::messages::SendMedia::CONSTRUCTOR_ID)
            .len(),
        2
    );

    // Cancellation mid-upload aborts immediately.
    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled = h.live.upload_file_bytes(
        "cancelled.bin",
        synth_bytes(16 * 1024, 0x01),
        h.live.upload_config(),
        &cancel,
        |_| {},
    );
    assert_eq!(cancelled.unwrap_err(), UploadError::Cancelled);

    h.shutdown();
}

// =============================================================================
// 2. All single media types + caption entities + reply_to + silent + schedule
// =============================================================================

#[test]
fn send_media_supports_all_media_kinds_caption_entities_reply_silent_and_schedule() {
    let h = UploadTestHarness::start();
    let next_msg_id = Arc::new(AtomicUsize::new(400));
    let self_uid = h.self_uid;
    let bob_uid = h.bob_uid;

    let msg_counter = Arc::clone(&next_msg_id);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            let id = msg_counter.fetch_add(1, Ordering::SeqCst) as i32;
            let upd = make_updates_with_messages(vec![synthetic_tl_message(
                id, bob_uid, self_uid, true, "sent",
            )]);
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));

    let mut live = h.live.clone();

    // Rich caption with Bold, Italic, Code, CustomUrl, MentionName entities.
    let caption = TextWithEntities {
        text: "Hello Bob code link".to_owned(),
        entities: vec![
            EntityInText::new(EntityType::Bold, 0, 5),
            EntityInText::with_data(EntityType::MentionName, 6, 3, h.bob_uid.to_string()),
            EntityInText::new(EntityType::Code, 10, 4),
            EntityInText::with_data(EntityType::CustomUrl, 15, 4, "https://example.org"),
        ],
    };

    // 2a. Compressed photo via HistoryBackend::send_message_extended (from real temp file).
    let tmp = tempfile::tempdir().expect("tempdir");
    let photo_path = tmp.path().join("beach.png");
    std::fs::write(&photo_path, encode_test_png(8, 6)).expect("write png");

    let sent_photo = HistoryBackend::send_message_extended(
        &mut live,
        h.bob_peer,
        SendMessageExtended {
            text: caption.clone(),
            reply_to: Some(MsgId(10)),
            reply_quote: Some(ReplyQuote {
                text: "quoted snippet".to_owned(),
                entities: Vec::new(),
                offset_utf16: 0,
            }),
            silent: true,
            schedule_date: Some(NOW_SECS + 3600),
            media: Some(HistoryMedia::Photo(PhotoAttachment {
                width: 8,
                height: 6,
                stripped_thumb: None,
                fallback_rgb: ([0, 0, 0], [0, 0, 0]),
                file_name: photo_path.to_string_lossy().into_owned(),
                size_bytes: 100,
            })),
            ..SendMessageExtended::default()
        },
    )
    .expect("send photo via HistoryBackend::send_message_extended");
    assert_eq!(sent_photo.id, MsgId(400));

    // 2b. Document (force_file = true).
    let doc_path = tmp.path().join("spec.pdf");
    std::fs::write(&doc_path, b"%PDF-1.7 sample").expect("write pdf");
    let _ = HistoryBackend::send_message_extended(
        &mut live,
        h.bob_peer,
        SendMessageExtended {
            media: Some(HistoryMedia::Document(DocumentAttachment {
                file_name: doc_path.to_string_lossy().into_owned(),
                size_bytes: 15,
                mime_type: "application/pdf".to_owned(),
                thumb_rgb: None,
            })),
            ..SendMessageExtended::default()
        },
    )
    .expect("send document");

    // 2c. Video with attributes and thumbnail.
    let _ = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Video {
                bytes: synth_bytes(2048, 0x20),
                file_name: "clip.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
                width: 1280,
                height: 720,
                duration_secs: 12.5,
                supports_streaming: true,
                nosound: false,
                round_message: false,
                thumb_bytes: Some(encode_test_png(4, 4)),
                video_cover_bytes: None,
                spoiler: true,
                ttl_seconds: None,
            },
            SendMediaOptions::default(),
        )
        .expect("send video");

    // 2d. Voice note with waveform + duration.
    let _ = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Voice {
                bytes: synth_bytes(1024, 0x30),
                file_name: "voice.ogg".to_owned(),
                duration_secs: 7,
                waveform_5bit: vec![0xAA, 0x55, 0x0F],
            },
            SendMediaOptions::default(),
        )
        .expect("send voice");

    // 2e. Audio with title + performer + duration.
    let _ = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Audio {
                bytes: synth_bytes(1024, 0x40),
                file_name: "track.mp3".to_owned(),
                mime_type: "audio/mpeg".to_owned(),
                duration_secs: 215,
                title: Some("Nocturne".to_owned()),
                performer: Some("Synth Ensemble".to_owned()),
                thumb_bytes: None,
            },
            SendMediaOptions::default(),
        )
        .expect("send audio");

    // 2f. Sticker.
    let _ = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Sticker {
                bytes: synth_bytes(512, 0x50),
                file_name: "wave.webp".to_owned(),
                mime_type: "image/webp".to_owned(),
                width: 512,
                height: 512,
                alt_emoji: "\u{1F44B}".to_owned(),
            },
            SendMediaOptions::default(),
        )
        .expect("send sticker");

    // 2g. GIF.
    let _ = live
        .send_uploaded_media(
            h.bob_peer,
            OutgoingMedia::Gif {
                bytes: synth_bytes(1024, 0x60),
                file_name: "loop.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
                width: 320,
                height: 240,
                duration_secs: Some(3.0),
                thumb_bytes: None,
                spoiler: false,
            },
            SendMediaOptions::default(),
        )
        .expect("send gif");

    let send_calls = h
        .fake
        .recorded_calls_of(tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    assert_eq!(send_calls.len(), 7);

    // Inspect call 0: Photo + caption entities + reply_to + quote + silent + schedule_date.
    let photo_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[0]).expect("photo req");
    assert!(photo_req.silent);
    assert_eq!(photo_req.schedule_date, Some(NOW_SECS + 3600));
    assert_eq!(photo_req.message, "Hello Bob code link");
    match photo_req.reply_to {
        Some(tl_enums::InputReplyTo::Message(r)) => {
            assert_eq!(r.reply_to_msg_id, 10);
            assert_eq!(r.quote_text.as_deref(), Some("quoted snippet"));
        }
        other => panic!("expected InputReplyTo::Message, got {other:?}"),
    }
    let ents = photo_req.entities.expect("caption entities");
    assert_eq!(ents.len(), 4);
    assert!(matches!(ents[0], tl_enums::MessageEntity::Bold(_)));
    assert!(matches!(
        ents[1],
        tl_enums::MessageEntity::InputMessageEntityMentionName(_)
    ));
    assert!(matches!(ents[2], tl_enums::MessageEntity::Code(_)));
    assert!(matches!(ents[3], tl_enums::MessageEntity::TextUrl(_)));
    assert!(matches!(
        photo_req.media,
        tl_enums::InputMedia::UploadedPhoto(_)
    ));

    // Call 1: Document with force_file = true.
    let doc_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[1]).expect("doc req");
    match doc_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert!(d.force_file);
            assert_eq!(d.mime_type, "application/pdf");
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Filename(f) if f.file_name == "spec.pdf"
            )));
        }
        other => panic!("expected UploadedDocument, got {other:?}"),
    }

    // Call 2: Video with thumbnail and DocumentAttribute::Video.
    let vid_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[2]).expect("vid req");
    match vid_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert!(d.spoiler);
            assert!(d.thumb.is_some());
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Video(v)
                    if v.w == 1280 && v.h == 720 && v.supports_streaming && !v.nosound
            )));
        }
        other => panic!("expected Video UploadedDocument, got {other:?}"),
    }

    // Call 3: Voice note with waveform and voice = true.
    let voice_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[3]).expect("voice req");
    match voice_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert_eq!(d.mime_type, "audio/ogg");
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Audio(au)
                    if au.voice && au.duration == 7 && au.waveform.as_deref() == Some(&[0xAA, 0x55, 0x0F][..])
            )));
        }
        other => panic!("expected Voice UploadedDocument, got {other:?}"),
    }

    // Call 4: Audio with title & performer.
    let audio_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[4]).expect("audio req");
    match audio_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert_eq!(d.mime_type, "audio/mpeg");
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Audio(au)
                    if !au.voice
                        && au.duration == 215
                        && au.title.as_deref() == Some("Nocturne")
                        && au.performer.as_deref() == Some("Synth Ensemble")
            )));
        }
        other => panic!("expected Audio UploadedDocument, got {other:?}"),
    }

    // Call 5: Sticker.
    let sticker_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[5]).expect("sticker req");
    match sticker_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Sticker(st) if st.alt == "\u{1F44B}"
            )));
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::ImageSize(sz) if sz.w == 512 && sz.h == 512
            )));
        }
        other => panic!("expected Sticker UploadedDocument, got {other:?}"),
    }

    // Call 6: GIF.
    let gif_req = tl_fns::messages::SendMedia::from_bytes(&send_calls[6]).expect("gif req");
    match gif_req.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert!(d.nosound_video);
            assert!(
                d.attributes
                    .iter()
                    .any(|a| matches!(a, tl_enums::DocumentAttribute::Animated))
            );
        }
        other => panic!("expected Gif UploadedDocument, got {other:?}"),
    }

    // Active uploads list is empty after completion.
    assert!(HistoryBackend::pending_media_uploads(&live, h.bob_peer).is_empty());

    h.shutdown();
}

// =============================================================================
// 3. Album flow (`messages.uploadMedia` + `messages.sendMultiMedia`)
// =============================================================================

#[test]
fn album_upload_uses_upload_media_then_send_multi_media_with_caption_entities() {
    let h = UploadTestHarness::start();
    let self_uid = h.self_uid;
    let bob_uid = h.bob_uid;
    let upload_seq = Arc::new(AtomicUsize::new(1));

    let seq_clone = Arc::clone(&upload_seq);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::UploadMedia::CONSTRUCTOR_ID {
            let req = tl_fns::messages::UploadMedia::from_bytes(raw).expect("UploadMedia");
            let id = 7000 + seq_clone.fetch_add(1, Ordering::SeqCst) as i64;
            let resp = match req.media {
                tl_enums::InputMedia::UploadedPhoto(_) => {
                    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
                        spoiler: false,
                        live_photo: false,
                        photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                            has_stickers: false,
                            id,
                            access_hash: id * 11,
                            file_reference: vec![1, 2, 3],
                            date: NOW_SECS,
                            sizes: Vec::new(),
                            video_sizes: None,
                            dc_id: 2,
                        })),
                        ttl_seconds: None,
                        video: None,
                    })
                }
                tl_enums::InputMedia::UploadedDocument(_) => {
                    tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: true,
                        round: false,
                        voice: false,
                        document: Some(tl_enums::Document::Document(tl_types::Document {
                            id,
                            access_hash: id * 13,
                            file_reference: vec![4, 5, 6],
                            date: NOW_SECS,
                            mime_type: "video/mp4".to_owned(),
                            size: 2048,
                            thumbs: None,
                            video_thumbs: None,
                            dc_id: 2,
                            attributes: Vec::new(),
                        })),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    })
                }
                _ => panic!("unexpected UploadMedia input"),
            };
            return Some(Ok(resp.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID {
            let upd = make_updates_with_messages(vec![
                synthetic_tl_message(501, bob_uid, self_uid, true, "Album caption"),
                synthetic_tl_message(502, bob_uid, self_uid, true, ""),
            ]);
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));

    let mut live = h.live.clone();
    let album_caption = TextWithEntities {
        text: "Album caption".to_owned(),
        entities: vec![EntityInText::new(EntityType::Italic, 0, 5)],
    };

    let sent = live
        .send_album(
            h.bob_peer,
            vec![
                AlbumUploadItem {
                    media: OutgoingMedia::Photo {
                        bytes: encode_test_png(4, 4),
                        file_name: "p1.png".to_owned(),
                        spoiler: true,
                        ttl_seconds: None,
                    },
                    caption: None,
                },
                AlbumUploadItem {
                    media: OutgoingMedia::Video {
                        bytes: synth_bytes(2048, 0x99),
                        file_name: "v2.mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                        width: 640,
                        height: 480,
                        duration_secs: 4.0,
                        supports_streaming: true,
                        nosound: false,
                        round_message: false,
                        thumb_bytes: None,
                        video_cover_bytes: None,
                        spoiler: false,
                        ttl_seconds: None,
                    },
                    caption: None,
                },
            ],
            SendMediaOptions {
                caption: album_caption.clone(),
                reply_to: Some(MsgId(10)),
                silent: true,
                schedule_date: Some(NOW_SECS + 600),
                ..SendMediaOptions::default()
            },
        )
        .expect("send_album");

    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].id, MsgId(501));
    assert_eq!(sent[1].id, MsgId(502));

    let upload_media_calls = h
        .fake
        .recorded_calls_of(tl_fns::messages::UploadMedia::CONSTRUCTOR_ID);
    assert_eq!(upload_media_calls.len(), 2);

    let multi_calls = h
        .fake
        .recorded_calls_of(tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID);
    assert_eq!(multi_calls.len(), 1);
    let multi_req =
        tl_fns::messages::SendMultiMedia::from_bytes(&multi_calls[0]).expect("SendMultiMedia");
    assert!(multi_req.silent);
    assert_eq!(multi_req.schedule_date, Some(NOW_SECS + 600));
    assert!(matches!(
        multi_req.reply_to,
        Some(tl_enums::InputReplyTo::Message(ref r)) if r.reply_to_msg_id == 10
    ));
    assert_eq!(multi_req.multi_media.len(), 2);

    let tl_enums::InputSingleMedia::InputSingleMedia(ref item0) = multi_req.multi_media[0];
    let tl_enums::InputSingleMedia::InputSingleMedia(ref item1) = multi_req.multi_media[1];
    assert_ne!(item0.random_id, item1.random_id);
    assert_eq!(item0.message, "Album caption");
    assert_eq!(item0.entities.as_ref().map(Vec::len), Some(1));
    assert!(item1.message.is_empty());
    assert!(matches!(
        item0.media,
        tl_enums::InputMedia::Photo(ref p) if p.spoiler
    ));
    assert!(matches!(item1.media, tl_enums::InputMedia::Document(_)));

    // Also test HistoryBackend::send_message_extended with HistoryMedia::Album from staged bytes.
    live.stage_media_bytes("staged_a.jpg", encode_test_png(4, 4));
    live.stage_media_bytes("staged_b.jpg", encode_test_png(4, 4));
    let _ = HistoryBackend::send_message_extended(
        &mut live,
        h.bob_peer,
        SendMessageExtended {
            text: album_caption,
            media: Some(HistoryMedia::Album(vec![
                AlbumItem {
                    msg_id: MsgId(1),
                    width: 4,
                    height: 4,
                    stripped_thumb: None,
                    fallback_rgb: ([0, 0, 0], [0, 0, 0]),
                    video_duration_secs: None,
                    label: "staged_a.jpg".to_owned(),
                },
                AlbumItem {
                    msg_id: MsgId(2),
                    width: 4,
                    height: 4,
                    stripped_thumb: None,
                    fallback_rgb: ([0, 0, 0], [0, 0, 0]),
                    video_duration_secs: None,
                    label: "staged_b.jpg".to_owned(),
                },
            ])),
            ..SendMessageExtended::default()
        },
    )
    .expect("send album via HistoryBackend::send_message_extended");
    assert_eq!(
        h.fake
            .recorded_calls_of(tl_fns::messages::SendMultiMedia::CONSTRUCTOR_ID)
            .len(),
        2
    );

    h.shutdown();
}

// =============================================================================
// 4. Terms of Service (`help.getTermsOfServiceUpdate` + `help.acceptTermsOfService`)
// =============================================================================

#[test]
fn terms_of_service_fetches_real_body_and_accepts_revision() {
    let h = UploadTestHarness::start();
    let tos_id_json = r#"{"country":"XX","min_age":false,"terms_key":"terms_v2"}"#;
    let tos_body = "You must be at least 16 years old and follow the community rules.";

    h.fake.enqueue_ok(
        tl_fns::help::GetTermsOfServiceUpdate::CONSTRUCTOR_ID,
        &tl_enums::help::TermsOfServiceUpdate::TermsOfServiceUpdate(
            tl_types::help::TermsOfServiceUpdate {
                expires: NOW_SECS + 3600,
                terms_of_service: tl_enums::help::TermsOfService::TermsOfService(
                    tl_types::help::TermsOfService {
                        popup: true,
                        id: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                            data: tos_id_json.to_owned(),
                        }),
                        text: tos_body.to_owned(),
                        entities: Vec::new(),
                        min_age_confirm: Some(16),
                    },
                ),
            },
        ),
    );

    let mut live = h.live.clone();
    let tos = InfoBackend::terms_of_service(&live);
    assert_eq!(tos.id, tos_id_json);
    assert_eq!(tos.body, tos_body);
    assert_eq!(tos.min_age_confirm, Some(16));
    assert!(!tos.accepted);

    // Subsequent call within expiry uses cache (no duplicate RPC).
    let tos_cached = InfoBackend::terms_of_service(&live);
    assert_eq!(tos_cached.body, tos_body);
    assert_eq!(
        h.fake
            .recorded_calls_of(tl_fns::help::GetTermsOfServiceUpdate::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Accept the ToS revision.
    h.fake
        .enqueue_ok(tl_fns::help::AcceptTermsOfService::CONSTRUCTOR_ID, &true);
    InfoBackend::accept_terms(&mut live, &tos.id).expect("accept_terms");

    let accept_calls = h
        .fake
        .recorded_calls_of(tl_fns::help::AcceptTermsOfService::CONSTRUCTOR_ID);
    assert_eq!(accept_calls.len(), 1);
    let accept_req =
        tl_fns::help::AcceptTermsOfService::from_bytes(&accept_calls[0]).expect("decode accept");
    let tl_enums::DataJSON::DataJSON(sent_id) = accept_req.id;
    assert_eq!(sent_id.data, tos_id_json);

    // Next ToS fetch after acceptance sees Empty update -> accepted = true, no NOT_AVAILABLE_YET.
    h.fake.enqueue_ok(
        tl_fns::help::GetTermsOfServiceUpdate::CONSTRUCTOR_ID,
        &tl_enums::help::TermsOfServiceUpdate::Empty(tl_types::help::TermsOfServiceUpdateEmpty {
            expires: NOW_SECS + 7200,
        }),
    );
    let tos_after = InfoBackend::terms_of_service(&live);
    assert!(tos_after.accepted);
    assert!(tos_after.body.is_empty());

    h.shutdown();
}

// =============================================================================
// 5. Copy media to clipboard & Forward from media overlay
// =============================================================================

#[test]
fn copy_media_to_clipboard_and_forward_from_media_overlay() {
    let h = UploadTestHarness::start();
    let photo_id = 91_234u64;
    let png_bytes = encode_test_png(6, 4);

    // Decode helper sanity check (including 90-degree rotation).
    let (w, ht, rgba) = decode_image_for_clipboard(&png_bytes, 0).expect("decode 0 deg");
    assert_eq!((w, ht), (6, 4));
    assert_eq!(rgba.len(), 6 * 4 * 4);
    let (rw, rh, _) = decode_image_for_clipboard(&png_bytes, 90).expect("decode 90 deg");
    assert_eq!((rw, rh), (4, 6));

    // Seed a photo message in Bob's history inside tg_data::Session.
    let bob_data_peer = DataPeerId::from_user(DataUserId::from_tl(h.bob_uid));
    let png_len = png_bytes.len() as i64;
    let session_arc = Arc::clone(h.runtime.session());
    h.runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        let mut msg = Message::new_text(
            bob_data_peer,
            DataMsgId(77),
            Some(bob_data_peer),
            NOW_SECS,
            DataText::plain("Photo to copy & forward"),
        );
        msg.flags = MessageFlags::empty();
        msg.content = MessageContent::Regular {
            text: DataText::plain("Photo to copy & forward"),
            media: MessageMedia::Photo(Box::new(PhotoData {
                id: PhotoId(photo_id),
                access_hash: 555_111,
                file_reference: vec![7, 8, 9],
                date: NOW_SECS,
                has_stickers: false,
                dc_id: 2,
                sizes: vec![tg_data::message::PhotoSize {
                    kind: "y".to_owned(),
                    width: 6,
                    height: 4,
                    size: png_len,
                }],
                stripped_thumb: None,
                spoiler: false,
                ttl_seconds: None,
                live_photo_video_id: None,
            })),
        };
        s.insert_message(msg);
    });

    // Serve the PNG bytes over `upload.getFile` and capture clipboard sink.
    let png_for_rpc = Arc::new(png_bytes);
    let carol_uid = h.carol_uid;
    let self_uid = h.self_uid;
    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePng,
                mtime: NOW_SECS,
                bytes: (*png_for_rpc).clone(),
            })
            .to_bytes()));
        }
        if ctor == tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID {
            let upd = make_updates_with_messages(vec![synthetic_tl_message(
                801,
                carol_uid,
                self_uid,
                true,
                "Photo to copy & forward",
            )]);
            return Some(Ok(upd.to_bytes()));
        }
        None
    }));

    let copied_capture: Arc<Mutex<Option<(u32, u32, usize)>>> = Arc::new(Mutex::new(None));
    let capture_clone = Arc::clone(&copied_capture);
    h.live
        .set_clipboard_test_sink(Some(Arc::new(move |cw, ch, bytes| {
            *capture_clone.lock().expect("lock capture") = Some((cw, ch, bytes.len()));
            Ok(())
        })));

    MediaBackend::copy_item(&h.live, MediaItemId(photo_id)).expect("copy_item");
    assert_eq!(
        *copied_capture.lock().expect("lock capture"),
        Some((6, 4, 6 * 4 * 4))
    );

    // Forward from media overlay to Carol via chosen target + `forward_item` AND via `forward_item_to`.
    h.live.set_media_forward_target(Some(h.carol_peer));
    MediaBackend::forward_item(&h.live, MediaItemId(photo_id)).expect("forward_item");
    MediaBackend::forward_item_to(&h.live, MediaItemId(photo_id), h.carol_peer.0)
        .expect("forward_item_to");

    let fwd_calls = h
        .fake
        .recorded_calls_of(tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID);
    assert_eq!(fwd_calls.len(), 2);
    for raw in &fwd_calls {
        let req =
            tl_fns::messages::ForwardMessages::from_bytes(raw).expect("decode ForwardMessages");
        assert_eq!(req.id, vec![77]);
        assert_eq!(req.random_id.len(), 1);
        assert_ne!(req.random_id[0], 0);
        assert!(matches!(
            req.from_peer,
            tl_enums::InputPeer::User(ref u) if u.user_id == h.bob_uid
        ));
        assert!(matches!(
            req.to_peer,
            tl_enums::InputPeer::User(ref u) if u.user_id == h.carol_uid
        ));
    }

    // Restricted (NO_FORWARDS) media rejects both copy and forward without issuing RPCs.
    let protected_photo_id = 91_235u64;
    let session_arc = Arc::clone(h.runtime.session());
    h.runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        let mut msg = Message::new_text(
            bob_data_peer,
            DataMsgId(78),
            Some(bob_data_peer),
            NOW_SECS,
            DataText::plain("Protected photo"),
        );
        msg.flags = MessageFlags::NO_FORWARDS;
        msg.content = MessageContent::Regular {
            text: DataText::plain("Protected photo"),
            media: MessageMedia::Photo(Box::new(PhotoData {
                id: PhotoId(protected_photo_id),
                access_hash: 555_222,
                file_reference: vec![9, 8, 7],
                date: NOW_SECS,
                has_stickers: false,
                dc_id: 2,
                sizes: vec![tg_data::message::PhotoSize {
                    kind: "y".to_owned(),
                    width: 6,
                    height: 4,
                    size: png_len,
                }],
                stripped_thumb: None,
                spoiler: false,
                ttl_seconds: None,
                live_photo_video_id: None,
            })),
        };
        s.insert_message(msg);
    });
    assert!(MediaBackend::copy_item(&h.live, MediaItemId(protected_photo_id)).is_err());
    assert!(MediaBackend::forward_item(&h.live, MediaItemId(protected_photo_id)).is_err());
    assert_eq!(
        h.fake
            .recorded_calls_of(tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID)
            .len(),
        2
    );

    h.shutdown();
}

// =============================================================================
// 6. Story upload (`stories.sendStory` + real `pending_uploads()` progress)
// =============================================================================

#[test]
fn story_upload_sends_photo_and_video_with_all_fields_and_tracks_pending_uploads() {
    let h = UploadTestHarness::start();
    let self_data_peer = DataPeerId::from_user(DataUserId::from_tl(h.self_uid));
    let bob_data_peer = DataPeerId::from_user(DataUserId::from_tl(h.bob_uid));
    let self_uid = h.self_uid;

    let observed_mid_progress: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let live_for_observer = h.live.clone();
    let progress_log = Arc::clone(&observed_mid_progress);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            let uploads = StoriesBackend::pending_uploads(&live_for_observer);
            if let Some(first) = uploads.first() {
                progress_log
                    .lock()
                    .expect("lock progress")
                    .push(first.progress);
            }
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::stories::SendStory::CONSTRUCTOR_ID {
            let uploads = StoriesBackend::pending_uploads(&live_for_observer);
            if let Some(first) = uploads.first() {
                progress_log
                    .lock()
                    .expect("lock progress")
                    .push(first.progress);
            }
            let story_item = tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
                pinned: true,
                public: true,
                close_friends: false,
                min: false,
                noforwards: true,
                edited: false,
                contacts: false,
                selected_contacts: false,
                out: true,
                id: 901,
                date: NOW_SECS,
                from_id: None,
                fwd_from: None,
                expire_date: NOW_SECS + 43_200,
                caption: Some("Hello Story".to_owned()),
                entities: None,
                media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
                    spoiler: false,
                    live_photo: false,
                    photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                        has_stickers: false,
                        id: 55_901,
                        access_hash: 123_456,
                        file_reference: vec![1, 2],
                        date: NOW_SECS,
                        sizes: Vec::new(),
                        video_sizes: None,
                        dc_id: 2,
                    })),
                    ttl_seconds: None,
                    video: None,
                })),
                media_areas: None,
                privacy: None,
                views: None,
                sent_reaction: None,
                albums: None,
                music: None,
            });
            let updates = tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: self_uid }),
                    story: story_item,
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 1,
            });
            return Some(Ok(updates.to_bytes()));
        }
        None
    }));

    // Force sequential single-session upload so part 1 observes progress after part 0 completes.
    let mut cfg = h.live.upload_config();
    cfg.max_sessions = 1;
    h.live.set_upload_config(cfg);

    let mut store = StoriesStore::new(self_data_peer);
    let mut live = h.live.clone();

    // 6a. Photo story with caption + entities, privacy rules (Public minus Bob), period 12h,
    // pinned = true, noforwards = true, album [3], and interactive media areas.
    let area = StoryArea {
        geometry: RectF::new(0.25, 0.25, 0.5, 0.2),
        rotation: 15.0,
        radius: 8.0,
    };
    let story_id = live
        .send_story(
            Some(&mut store),
            SendStoryRequest {
                peer_id: self_data_peer,
                media: StoryUploadMedia::Photo {
                    bytes: synth_bytes(72 * 1024, 0x42), // 3 parts of 32/32/8 KiB
                    file_name: "morning_view".to_owned(),
                },
                caption: TextWithEntities {
                    text: "Hello Story".to_owned(),
                    entities: vec![EntityInText::new(EntityType::Bold, 0, 5)],
                },
                privacy: StoryPrivacyInput {
                    privacy: StoryPrivacy::Public,
                    allow_peers: Vec::new(),
                    disallow_peers: vec![bob_data_peer],
                },
                period: Some(43_200),
                pinned: true,
                noforwards: true,
                albums: vec![3],
                locations: vec![StoryLocation {
                    area,
                    point: GeoPoint {
                        lat: 47.3769,
                        lon: 8.5417,
                        accuracy_radius: Some(50),
                    },
                    title: "Central Station".to_owned(),
                    address: "Bahnhofplatz".to_owned(),
                    country_iso2: "CH".to_owned(),
                    state: String::new(),
                    city: "Zurich".to_owned(),
                    street: String::new(),
                    provider: "foursquare".to_owned(),
                    venue_id: "venue_1".to_owned(),
                    venue_type: "transit".to_owned(),
                }],
                suggested_reactions: vec![SuggestedReaction {
                    area,
                    reaction: ReactionId::Emoji("\u{1F525}".to_owned()),
                    count: 0,
                    flipped: true,
                    dark: true,
                }],
                channel_posts: Vec::new(),
                url_areas: vec![UrlArea {
                    area,
                    url: "https://example.org/story".to_owned(),
                }],
                weather_areas: vec![WeatherArea {
                    area,
                    emoji: "\u{2600}".to_owned(),
                    color: 0xFF_FF_AA_00,
                    millicelsius: 22_500,
                }],
            },
            NOW_SECS,
        )
        .expect("send photo story");

    assert_eq!(story_id, 901);
    assert!(store.lookup(FullStoryId::new(self_data_peer, 901)).is_ok());
    // Pending uploads cleared once finished.
    assert!(StoriesBackend::pending_uploads(&live).is_empty());
    // Real upload progress was reported during part upload and reached 1.0 before SendStory.
    let progress_samples = observed_mid_progress.lock().expect("lock progress").clone();
    assert!(
        progress_samples.contains(&1.0),
        "expected progress to reach 1.0 during stories.sendStory, got {progress_samples:?}"
    );
    assert!(
        progress_samples.iter().any(|&p| p < 1.0),
        "expected progress < 1.0 while parts were uploading, got {progress_samples:?}"
    );

    // 6b. Video story with thumbnail and CloseFriends privacy.
    let _ = live
        .send_story(
            None,
            SendStoryRequest {
                peer_id: self_data_peer,
                media: StoryUploadMedia::Video {
                    bytes: synth_bytes(40 * 1024, 0x88),
                    file_name: "clip.mp4".to_owned(),
                    width: 720,
                    height: 1280,
                    duration_secs: 6.5,
                    has_audio: false,
                    thumb_bytes: Some(encode_test_png(4, 4)),
                },
                caption: TextWithEntities::default(),
                privacy: StoryPrivacyInput {
                    privacy: StoryPrivacy::CloseFriends,
                    allow_peers: Vec::new(),
                    disallow_peers: Vec::new(),
                },
                period: Some(86_400),
                pinned: false,
                noforwards: false,
                albums: Vec::new(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            NOW_SECS,
        )
        .expect("send video story");

    let story_calls = h
        .fake
        .recorded_calls_of(tl_fns::stories::SendStory::CONSTRUCTOR_ID);
    assert_eq!(story_calls.len(), 2);

    // Verify photo stories.sendStory fields.
    let photo_story =
        tl_fns::stories::SendStory::from_bytes(&story_calls[0]).expect("decode photo SendStory");
    assert!(photo_story.pinned);
    assert!(photo_story.noforwards);
    assert_eq!(photo_story.period, Some(43_200));
    assert_eq!(photo_story.caption.as_deref(), Some("Hello Story"));
    assert_eq!(photo_story.entities.as_ref().map(Vec::len), Some(1));
    assert_eq!(photo_story.albums, Some(vec![3]));
    assert_ne!(photo_story.random_id, 0);
    assert!(matches!(
        photo_story.media,
        tl_enums::InputMedia::UploadedPhoto(_)
    ));
    assert!(
        photo_story
            .privacy_rules
            .iter()
            .any(|r| matches!(r, tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll))
    );
    assert!(photo_story.privacy_rules.iter().any(|r| matches!(
        r,
        tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(u) if u.users.len() == 1
    )));
    let areas = photo_story.media_areas.expect("media_areas");
    assert_eq!(areas.len(), 4);
    assert!(
        areas
            .iter()
            .any(|a| matches!(a, tl_enums::MediaArea::Venue(_)))
    );
    assert!(
        areas
            .iter()
            .any(|a| matches!(a, tl_enums::MediaArea::SuggestedReaction(_)))
    );
    assert!(
        areas
            .iter()
            .any(|a| matches!(a, tl_enums::MediaArea::Url(_)))
    );
    assert!(
        areas
            .iter()
            .any(|a| matches!(a, tl_enums::MediaArea::Weather(_)))
    );

    // Verify video stories.sendStory fields.
    let video_story =
        tl_fns::stories::SendStory::from_bytes(&story_calls[1]).expect("decode video SendStory");
    assert!(!video_story.pinned);
    assert_eq!(video_story.period, Some(86_400));
    assert!(video_story.caption.is_none());
    assert!(matches!(
        video_story.privacy_rules.as_slice(),
        [tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends]
    ));
    match video_story.media {
        tl_enums::InputMedia::UploadedDocument(d) => {
            assert!(d.nosound_video);
            assert!(d.thumb.is_some());
            assert_eq!(d.mime_type, "video/mp4");
            assert!(d.attributes.iter().any(|a| matches!(
                a,
                tl_enums::DocumentAttribute::Video(v) if v.w == 720 && v.h == 1280 && v.nosound
            )));
        }
        other => panic!("expected video story UploadedDocument, got {other:?}"),
    }

    // 6c. Failed story upload remains visible in `pending_uploads()` with `failed = true`
    // until dismissed.
    h.fake.set_dynamic_handler(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::stories::SendStory::CONSTRUCTOR_ID {
            return Some(Err(InvocationError::Rpc(RpcError::new(
                400,
                "STORIES_TOO_MUCH",
            ))));
        }
        None
    }));
    let err = live
        .send_story(
            None,
            SendStoryRequest {
                peer_id: self_data_peer,
                media: StoryUploadMedia::Photo {
                    bytes: synth_bytes(4096, 0x05),
                    file_name: "fail.jpg".to_owned(),
                },
                caption: TextWithEntities::default(),
                privacy: StoryPrivacyInput {
                    privacy: StoryPrivacy::Public,
                    allow_peers: Vec::new(),
                    disallow_peers: Vec::new(),
                },
                period: None,
                pinned: false,
                noforwards: false,
                albums: Vec::new(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            NOW_SECS,
        )
        .expect_err("expected STORIES_TOO_MUCH error");
    assert!(err.contains("STORIES_TOO_MUCH"), "unexpected err: {err}");
    let failed_uploads = StoriesBackend::pending_uploads(&live);
    assert_eq!(failed_uploads.len(), 1);
    assert!(failed_uploads[0].failed);
    live.dismiss_failed_story_uploads(self_data_peer);
    assert!(StoriesBackend::pending_uploads(&live).is_empty());

    h.shutdown();
}
