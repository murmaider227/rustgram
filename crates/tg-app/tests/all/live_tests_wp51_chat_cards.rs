//! WP-51 integration tests & headless `wgpu` visual snapshots:
//! - AI Message Summary (`messages.summarizeText`, `SummaryHeader`, `(peer_id, msg_id)` cache)
//! - Custom Video Cover Uploader (`upload.saveFilePart` + `messages.uploadMedia` + `InputMediaUploadedDocument::video_cover`)
//! - Round-Video Circular Seek Ring (`VideoMessageSeek` drag-to-seek)
//! - Interactive Birthday & Userpic Suggestion Cards (`account.updateBirthday`, `photos.updateProfilePhoto`)
//! - Profile Birthday Confetti Effect (`info_profile_birthday_effect`, reduced-motion gating)
//!
//! All tests run strictly against in-process `FakeRpcClient` and synthetic placeholder data.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Birthday as DataBirthday, Message, MessageFlags, MsgId as DataMsgId, PeerId as DataPeerId,
    PeerUpdateFlags, PhotoData, PhotoId, ServiceAction as DataServiceAction,
    TextWithEntities as DataText, User, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::history::backend::{
    HistoryBackend, HistoryMedia, HistoryMessage, MessageExtra, ServiceAction as UiServiceAction,
};
use tg_app::history::sections_model::RoundVideoAttachment;
use tg_app::history::video_cover::{
    VideoCoverPickerOutcome, encode_rgb_jpeg, prepare_cover_image_bytes, synthesize_cover_frame,
};
use tg_app::live::LiveSession;
use tg_app::live::upload::{OutgoingMedia, SendMediaOptions};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames};

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
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
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = request.to_bytes();
        let ctor = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, raw));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, outcome)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC ctor: got {ctor:#010x}, expected {expected:#010x}"
                );
                let bytes = outcome?;
                R::Return::from_bytes(&bytes)
                    .map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("NO_FAKE_RESPONSE_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, PeerId) {
    let fake = FakeRpcClient::new(2);
    let self_uid = DataUserId(10_501);
    let friend_uid = DataUserId(30_503);

    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            self_uid,
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start runtime"),
    );

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;

        let mut me = User::new(self_uid, true);
        me.first_name = "Tester".to_owned();
        me.flags.insert(UserFlags::SELF);
        s.upsert_user(me, PeerUpdateFlags::NAME);

        let mut friend = User::new(friend_uid, false);
        friend.first_name = "Mira".to_owned();
        friend.last_name = "Vance".to_owned();
        friend.access_hash = 951_003;
        s.upsert_user(friend, PeerUpdateFlags::NAME);

        let dp = DataPeerId::from_user(friend_uid);
        let h = s.ensure_history(dp);
        h.loaded_ranges.add_slice(
            std::iter::empty(),
            tg_data::MsgRange::new(DataMsgId(0), tg_data::SERVER_MAX_MSG_ID),
            Some(0),
        );
    });

    let live = LiveSession::new(runtime, 1);
    (live, fake, PeerId::from_user(friend_uid.0))
}

fn push_synthetic_wp51_messages(
    handle: &tg_app::stand_in::mock_session::SharedSessionHandle,
) -> (MsgId, MsgId, MsgId, MsgId) {
    let peer = session_peers::USER_MIRA;
    let base_ts = 1_795_341_600 + 20_000;

    let round_video_id = MsgId(9501);
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: round_video_id,
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: base_ts,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(HistoryMedia::RoundVideo(RoundVideoAttachment {
                duration_secs: 42,
                diameter_px: 176,
                accent_rgb: ([0x3B, 0x82, 0xC4], [0x1D, 0x4E, 0x89]),
            })),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );

    let bday_id = MsgId(9502);
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: bday_id,
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: base_ts + 60,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::default(),
            media: None,
            service: Some(UiServiceAction::SuggestBirthday {
                day: 21,
                month: 8,
                year: Some(1996),
            }),
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );

    let userpic_id = MsgId(9503);
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: userpic_id,
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: base_ts + 120,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::default(),
            media: None,
            service: Some(UiServiceAction::SuggestProfilePhoto {
                photo_id: 91234,
                access_hash: 445566,
                file_reference: vec![0xAA, 0xBB, 0xCC],
                is_video: false,
                stripped_thumb: None,
                fallback_rgb: ([0x5E, 0xA8, 0xE6], [0x2B, 0x62, 0x9C]),
            }),
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );

    let summary_id = MsgId(9504);
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: summary_id,
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: base_ts + 180,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "We finished profiling the shader cache and confirmed that pre-warming pipeline \
                 states eliminates frame drops during first-paint transitions across both desktop \
                 and compact mobile layouts. Let's roll this into the beta channel tomorrow morning \
                 after we verify the round-video seek accuracy.",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                summary_from_language: Some("en".to_owned()),
                ..MessageExtra::default()
            },
        },
    );

    (summary_id, round_video_id, bday_id, userpic_id)
}

// =============================================================================
// 1. Live FakeRpcClient TL Tests
// =============================================================================

#[test]
fn wp51_live_summarize_text_rpc_toggle_and_cache() {
    let (mut live, fake, peer_id) = make_live_session();
    let msg_id = MsgId(401);
    let friend_dp = DataPeerId::from_user(DataUserId(30_503));

    // Seed a summarizable message (`MessageFlags::CAN_BE_SUMMARIZED`) in session history.
    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;
        let mut msg = Message::new_text(
            friend_dp,
            DataMsgId(i64::from(msg_id.0)),
            Some(friend_dp),
            1_750_000_100,
            DataText::plain("Very long technical update paragraph."),
        );
        msg.flags.insert(MessageFlags::CAN_BE_SUMMARIZED);
        s.insert_message(msg);
    });

    // Verify conversion propagates `summary_from_language`.
    let slice = live.load_slice_around(peer_id, msg_id, 5, 5);
    let ui_msg = slice
        .messages
        .iter()
        .find(|m| m.id == msg_id)
        .expect("seeded message present");
    assert_eq!(ui_msg.extra.summary_from_language.as_deref(), Some("auto"));

    // Enqueue exact `messages.summarizeText` response.
    fake.enqueue_ok(
        tl_fns::messages::SummarizeText::CONSTRUCTOR_ID,
        &tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: "Shader pre-warming removes frame drops; ships tomorrow.".to_owned(),
            entities: Vec::new(),
        }),
    );

    // First toggle fetches via `messages.summarizeText` and shows summary.
    let first = live
        .toggle_message_summary(peer_id, msg_id, Some("en"))
        .expect("summarizeText succeeds");
    assert!(first.shown);
    assert!(!first.loading);
    assert_eq!(
        first.text.text,
        "Shader pre-warming removes frame drops; ships tomorrow."
    );

    let calls = fake.recorded_of(tl_fns::messages::SummarizeText::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 1, "must invoke messages.summarizeText once");
    let decoded =
        tl_fns::messages::SummarizeText::from_bytes(&calls[0]).expect("decode SummarizeText");
    assert_eq!(decoded.id, msg_id.0);
    assert_eq!(decoded.to_lang.as_deref(), Some("en"));
    assert!(decoded.tone.is_none());
    match decoded.peer {
        tl_enums::InputPeer::User(u) => {
            assert_eq!(u.user_id, 30_503);
            assert_eq!(u.access_hash, 951_003);
        }
        other => panic!("expected InputPeer::User, got {other:?}"),
    }

    // Second & third toggles hit the `(peer_id, msg_id)` cache without issuing another RPC.
    let second = live
        .toggle_message_summary(peer_id, msg_id, Some("en"))
        .expect("toggle off");
    assert!(!second.shown);
    let third = live
        .toggle_message_summary(peer_id, msg_id, Some("en"))
        .expect("toggle on from cache");
    assert!(third.shown);
    assert_eq!(
        fake.recorded_of(tl_fns::messages::SummarizeText::CONSTRUCTOR_ID)
            .len(),
        1,
        "cached summary must not re-invoke messages.summarizeText"
    );

    // Verify `SUMMARY_FLOOD_PREMIUM` surfaces cleanly and marks `premium_required`.
    let premium_msg_id = MsgId(402);
    fake.enqueue_err(
        tl_fns::messages::SummarizeText::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 420,
            name: "SUMMARY_FLOOD_PREMIUM".to_owned(),
            value: None,
        }),
    );
    let err = live
        .toggle_message_summary(peer_id, premium_msg_id, None)
        .expect_err("SUMMARY_FLOOD_PREMIUM must return Err");
    assert!(
        err.to_lowercase().contains("premium"),
        "expected premium hint, got: {err}"
    );
    let cached_err = live
        .message_summary(peer_id, premium_msg_id)
        .expect("premium state recorded");
    assert!(cached_err.premium_required);
    assert!(!cached_err.shown);
}

#[test]
fn wp51_live_video_cover_upload_populates_input_media_video_cover() {
    let (mut live, fake, peer_id) = make_live_session();
    let cover_img = synthesize_cover_frame(320, 180, 0.5, 9);
    let cover_jpeg = encode_rgb_jpeg(&cover_img).expect("encode JPEG cover");
    assert!(cover_jpeg.starts_with(&[0xFF, 0xD8]));

    // Exact RPC sequence:
    // 1. upload.saveFilePart (cover JPEG bytes)
    // 2. messages.uploadMedia (InputMediaUploadedPhoto -> MessageMediaPhoto)
    // 3. upload.saveFilePart (video bytes)
    // 4. messages.sendMedia (InputMediaUploadedDocument { video_cover: Some(InputPhoto) })
    fake.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::messages::UploadMedia::CONSTRUCTOR_ID,
        &tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: 777_001,
                access_hash: 888_002,
                file_reference: vec![1, 2, 3, 4],
                date: 1_750_000_200,
                sizes: Vec::new(),
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        }),
    );
    fake.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: tl_enums::Message::Message(tl_types::Message {
                    out: true,
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
                    guestchat_via_from: None,
                    id: 810,
                    from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 10_501 })),
                    from_boosts_applied: None,
                    from_rank: None,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 30_503 }),
                    saved_peer_id: None,
                    fwd_from: None,
                    via_bot_id: None,
                    via_business_bot_id: None,
                    reply_to: None,
                    date: 1_750_000_205,
                    message: "With custom cover".to_owned(),
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
                }),
                pts: 10,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_205,
            seq: 1,
        }),
    );

    let sent = live
        .send_uploaded_media(
            peer_id,
            OutgoingMedia::Video {
                bytes: vec![
                    0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2',
                ],
                file_name: "demo_clip.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
                duration_secs: 12.0,
                width: 1280,
                height: 720,
                supports_streaming: true,
                round_message: false,
                nosound: false,
                spoiler: false,
                ttl_seconds: None,
                thumb_bytes: None,
                video_cover_bytes: Some(cover_jpeg),
            },
            SendMediaOptions {
                caption: TextWithEntities::simple("With custom cover"),
                ..SendMediaOptions::default()
            },
        )
        .expect("send_uploaded_media with video_cover succeeds");
    assert_eq!(sent.id, MsgId(810));

    let part_calls = fake.recorded_of(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID);
    assert_eq!(
        part_calls.len(),
        2,
        "must upload both video and custom cover via upload.saveFilePart"
    );

    let upload_media_calls = fake.recorded_of(tl_fns::messages::UploadMedia::CONSTRUCTOR_ID);
    assert_eq!(upload_media_calls.len(), 1);
    let upload_req = tl_fns::messages::UploadMedia::from_bytes(&upload_media_calls[0])
        .expect("decode UploadMedia");
    assert!(
        matches!(upload_req.media, tl_enums::InputMedia::UploadedPhoto(_)),
        "video cover must be uploaded via InputMediaUploadedPhoto"
    );

    let send_calls = fake.recorded_of(tl_fns::messages::SendMedia::CONSTRUCTOR_ID);
    assert_eq!(send_calls.len(), 1);
    let send_req =
        tl_fns::messages::SendMedia::from_bytes(&send_calls[0]).expect("decode SendMedia");
    match send_req.media {
        tl_enums::InputMedia::UploadedDocument(doc) => match doc.video_cover {
            Some(tl_enums::InputPhoto::InputPhoto(p)) => {
                assert_eq!(p.id, 777_001);
                assert_eq!(p.access_hash, 888_002);
                assert_eq!(p.file_reference, vec![1, 2, 3, 4]);
            }
            other => panic!("expected InputMediaUploadedDocument::video_cover, got {other:?}"),
        },
        other => panic!("expected InputMedia::UploadedDocument, got {other:?}"),
    }
}

#[test]
fn wp51_live_suggestion_cards_birthday_and_userpic_rpcs() {
    let (mut live, fake, peer_id) = make_live_session();
    let friend_dp = DataPeerId::from_user(DataUserId(30_503));

    // Seed both `SuggestBirthday` and `SuggestProfilePhoto` service messages in `tg-data`.
    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;
        let bday_msg = Message::new_service(
            friend_dp,
            DataMsgId(501),
            Some(friend_dp),
            1_750_000_300,
            DataServiceAction::SuggestBirthday {
                birthday: DataBirthday {
                    day: 14,
                    month: 5,
                    year: 1994,
                },
            },
        );
        let photo_msg = Message::new_service(
            friend_dp,
            DataMsgId(502),
            Some(friend_dp),
            1_750_000_310,
            DataServiceAction::SuggestProfilePhoto {
                photo: Some(Box::new(PhotoData {
                    id: PhotoId(555_111),
                    access_hash: 666_222,
                    file_reference: vec![9, 8, 7],
                    date: 1_750_000_310,
                    dc_id: 2,
                    sizes: Vec::new(),
                    stripped_thumb: None,
                    has_stickers: false,
                    spoiler: false,
                    ttl_seconds: None,
                    live_photo_video_id: None,
                })),
            },
        );
        s.insert_message(bday_msg);
        s.insert_message(photo_msg);
    });

    // Verify `convert.rs` maps both to UI `ServiceAction` variants.
    let slice = live.load_slice_around(peer_id, MsgId(502), 5, 5);
    let bday_ui = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(501))
        .expect("birthday service msg");
    assert_eq!(
        bday_ui.service,
        Some(UiServiceAction::SuggestBirthday {
            day: 14,
            month: 5,
            year: Some(1994),
        })
    );
    let photo_ui = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(502))
        .expect("userpic service msg");
    match &photo_ui.service {
        Some(UiServiceAction::SuggestProfilePhoto {
            photo_id,
            access_hash,
            file_reference,
            is_video,
            ..
        }) => {
            assert_eq!(*photo_id, 555_111);
            assert_eq!(*access_hash, 666_222);
            assert_eq!(file_reference, &vec![9, 8, 7]);
            assert!(!*is_video);
        }
        other => panic!("expected SuggestProfilePhoto, got {other:?}"),
    }

    // Apply suggested birthday -> exact `account.updateBirthday` RPC.
    fake.enqueue_ok(tl_fns::account::UpdateBirthday::CONSTRUCTOR_ID, &true);
    live.apply_suggested_birthday(14, 5, Some(1994))
        .expect("apply_suggested_birthday succeeds");
    let bday_calls = fake.recorded_of(tl_fns::account::UpdateBirthday::CONSTRUCTOR_ID);
    assert_eq!(bday_calls.len(), 1);
    let bday_req =
        tl_fns::account::UpdateBirthday::from_bytes(&bday_calls[0]).expect("decode UpdateBirthday");
    match bday_req.birthday {
        Some(tl_enums::Birthday::Birthday(b)) => {
            assert_eq!(b.day, 14);
            assert_eq!(b.month, 5);
            assert_eq!(b.year, Some(1994));
        }
        other => panic!("expected Birthday, got {other:?}"),
    }

    // Apply suggested profile photo -> exact `photos.updateProfilePhoto` RPC.
    fake.enqueue_ok(
        tl_fns::photos::UpdateProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 555_111 }),
            users: Vec::new(),
        }),
    );
    live.apply_suggested_profile_photo(peer_id, MsgId(502), 555_111, 666_222, &[9, 8, 7], false)
        .expect("apply_suggested_profile_photo succeeds");
    let photo_calls = fake.recorded_of(tl_fns::photos::UpdateProfilePhoto::CONSTRUCTOR_ID);
    assert_eq!(photo_calls.len(), 1);
    let photo_req = tl_fns::photos::UpdateProfilePhoto::from_bytes(&photo_calls[0])
        .expect("decode UpdateProfilePhoto");
    match photo_req.id {
        tl_enums::InputPhoto::InputPhoto(p) => {
            assert_eq!(p.id, 555_111);
            assert_eq!(p.access_hash, 666_222);
            assert_eq!(p.file_reference, vec![9, 8, 7]);
        }
        other => panic!("expected InputPhoto, got {other:?}"),
    }
}

// =============================================================================
// 2. Headless egui_kittest UI Interaction Tests
// =============================================================================

#[test]
fn wp51_ui_summary_pill_toggle_and_cache_kittest() {
    let mut state = AppTestState::main_workspace();
    let handle = state.controller.session_handle();
    let (summary_id, _, _, _) = push_synthetic_wp51_messages(&handle);
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), summary_id);

    let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
    settle_layout(&mut h);
    step_frames(&mut h, 3);

    // Click the collapsed AI summary pill.
    h.get_by_label_contains("AI summary").click();
    step_frames(&mut h, 3);

    let after_expand = handle
        .message_summary(session_peers::USER_MIRA, summary_id)
        .expect("summary created on click");
    assert!(after_expand.shown, "summary header should be expanded");
    assert!(
        after_expand.text.text.starts_with("Summary:"),
        "summary text should be populated, got: {}",
        after_expand.text.text
    );

    // Click the expanded summary header to toggle back to original text.
    h.get_by_label_contains("AI summary:").click();
    step_frames(&mut h, 3);

    let after_collapse = handle
        .message_summary(session_peers::USER_MIRA, summary_id)
        .expect("summary remains cached");
    assert!(
        !after_collapse.shown,
        "summary should toggle back to original"
    );
}

#[test]
fn wp51_ui_video_cover_picker_flow_kittest() {
    // Reject invalid cover bytes with `lng_choose_cover_bad`.
    let bad = prepare_cover_image_bytes(b"not an image", 1280, 720);
    assert!(bad.is_err());

    let mut state = AppTestState::main_workspace();
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
    {
        let hist = state.controller.main_widget_mut().history_mut();
        hist.open_attach_box(false);
        if let Some(box_st) = hist.attach_box_mut() {
            box_st.add_sample_video("demo_clip.mp4", 1280, 720, 2_450_000);
        }
    }

    let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
    settle_layout(&mut h);
    step_frames(&mut h, 3);

    // Click "Cover" on the video row to open `VideoCoverPickerState`.
    h.get_by_label("Cover").click();
    step_frames(&mut h, 3);

    {
        let hist = h.state_mut().controller.main_widget_mut().history_mut();
        let attach = hist.attach_box_mut().expect("attach box open");
        let picker = attach
            .active_cover_picker
            .as_mut()
            .expect("cover picker open");
        picker.selected_ms = 4_200;
        assert!(picker.apply_custom_image_bytes(b"corrupt").is_err());
        assert!(picker.error_message.is_some());
    }
    step_frames(&mut h, 2);

    // Confirm custom cover via "Use selected cover" button.
    h.get_by_label("Use selected cover").click();
    step_frames(&mut h, 3);

    {
        let hist = h.state_mut().controller.main_widget_mut().history_mut();
        let attach = hist.attach_box_mut().expect("attach box still open");
        assert!(attach.active_cover_picker.is_none());
        let cover_bytes = attach
            .video_covers
            .get("demo_clip.mp4")
            .expect("custom video cover stored on AttachFilesBoxState");
        assert!(
            cover_bytes.starts_with(&[0xFF, 0xD8]),
            "stored video cover must be valid JPEG bytes"
        );
    }

    // Smoke-check `VideoCoverPickerOutcome` enum stays reachable.
    let _ = VideoCoverPickerOutcome::Cancelled;
}

#[test]
fn wp51_ui_round_video_seek_ring_drag_kittest() {
    let mut state = AppTestState::main_workspace();
    let handle = state.controller.session_handle();
    let (_, round_video_id, _, _) = push_synthetic_wp51_messages(&handle);
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(
            session_peers::USER_MIRA,
            SectionShow::default(),
            round_video_id,
        );

    let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
    settle_layout(&mut h);
    step_frames(&mut h, 4);

    let bubble_rect = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .last_painted_bubble_rect(round_video_id)
        .expect("round video bubble painted");

    // Drag along the circular seek ring rim: start near 12 o'clock, drag to 3 o'clock (0.25), then 6 o'clock (0.50).
    let center = bubble_rect.center();
    let rim_r = (bubble_rect.width().min(bubble_rect.height()) * 0.5) - 6.0;
    let p_top = Pos2::new(center.x, center.y - rim_r);
    let p_right = Pos2::new(center.x + rim_r, center.y);
    let p_bottom = Pos2::new(center.x, center.y + rim_r);

    h.input_mut().events.push(Event::PointerMoved(p_top));
    step_frames(&mut h, 1);
    h.input_mut().events.push(Event::PointerButton {
        pos: p_top,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    step_frames(&mut h, 1);
    h.input_mut().events.push(Event::PointerMoved(p_right));
    step_frames(&mut h, 2);

    let progress_q1 = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .media_runtime()
        .round_video_seek_progress(round_video_id)
        .expect("seek progress recorded at 3 o'clock");
    assert!(
        (progress_q1 - 0.25).abs() < 0.08,
        "expected ~0.25 at 3 o'clock, got {progress_q1}"
    );

    h.input_mut().events.push(Event::PointerMoved(p_bottom));
    step_frames(&mut h, 2);
    h.input_mut().events.push(Event::PointerButton {
        pos: p_bottom,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    step_frames(&mut h, 1);

    let progress_q2 = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .media_runtime()
        .round_video_seek_progress(round_video_id)
        .expect("seek progress recorded at 6 o'clock");
    assert!(
        (progress_q2 - 0.50).abs() < 0.08,
        "expected ~0.50 at 6 o'clock, got {progress_q2}"
    );
}

#[test]
fn wp51_ui_suggestion_cards_click_actions_kittest() {
    let mut state = AppTestState::main_workspace();
    let handle = state.controller.session_handle();
    let (_, _, _bday_id, userpic_id) = push_synthetic_wp51_messages(&handle);
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), userpic_id);

    let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
    settle_layout(&mut h);
    step_frames(&mut h, 4);

    // Click Birthday Suggestion card CTA -> updates self birthday via `apply_suggested_birthday`.
    h.get_by_label_contains("Add suggested birthday").click();
    step_frames(&mut h, 3);
    assert_eq!(
        handle.self_birthday(),
        Some((21, 8, Some(1996))),
        "clicking birthday suggestion card must apply suggested birthday"
    );

    // Click Userpic Suggestion card CTA -> updates profile photo via `apply_suggested_profile_photo`.
    h.get_by_label("Set Photo").click();
    step_frames(&mut h, 3);
    assert_eq!(
        handle.applied_userpic_photo_id(),
        Some(91234),
        "clicking userpic suggestion card must apply suggested photo_id"
    );
}

#[test]
fn wp51_ui_profile_birthday_confetti_and_reduced_motion_kittest() {
    // 1. Active confetti when today matches Mira Vance's birthday (May 14, 1994).
    {
        let mut state = AppTestState::main_workspace();
        state
            .controller
            .session_mut()
            .expect("session")
            .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
        let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
        settle_layout(&mut h);
        h.state_mut()
            .controller
            .main_widget_mut()
            .third_info_mut()
            .set_birthday_today_override(Some((2026, 5, 14)));
        h.state_mut()
            .controller
            .session_mut()
            .expect("session")
            .show_peer_info(session_peers::USER_MIRA, SectionShow::default());
        step_frames(&mut h, 10);

        let info = h.state().controller.main_widget().third_info();
        assert!(
            info.is_birthday_effect_active(),
            "birthday confetti effect should be active on Mira's birthday"
        );
        assert!(
            info.birthday_effect_frames_rendered() >= 5,
            "confetti effect should have rendered multiple frames, got {}",
            info.birthday_effect_frames_rendered()
        );
    }

    // 2. Reduced motion (`chat_effects_paused = true`) suppresses confetti completely.
    {
        let mut state = AppTestState::main_workspace();
        state
            .controller
            .session_mut()
            .expect("session")
            .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
        let mut h = build_wgpu_harness(state, Vec2::new(1440.0, 900.0));
        settle_layout(&mut h);
        {
            let info = h.state_mut().controller.main_widget_mut().third_info_mut();
            info.set_birthday_today_override(Some((2026, 5, 14)));
            info.chat_effects_paused = true;
        }
        h.state_mut()
            .controller
            .session_mut()
            .expect("session")
            .show_peer_info(session_peers::USER_MIRA, SectionShow::default());
        step_frames(&mut h, 8);

        let info = h.state().controller.main_widget().third_info();
        assert!(
            !info.is_birthday_effect_active(),
            "reduced motion must suppress birthday confetti"
        );
        assert_eq!(info.birthday_effect_frames_rendered(), 0);
    }
}

// =============================================================================
// 3. Headless wgpu Before & After Visual Snapshot Captures
// =============================================================================

fn save_harness_png(harness: &mut egui_kittest::Harness<'_, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap parent dir");
    }
    let img = harness.render().expect("render wgpu snapshot");
    img.save(path).expect("save png snapshot");
}

#[test]
#[ignore = "Manual visual snapshot capture (writes before PNGs to RUSTGRAM_SNAPSHOT_DIR or temp dir)"]
fn capture_wp51_before_snapshots() {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp51"));
    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        {
            let mut state = AppTestState::main_workspace();
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            step_frames(&mut h, 5);
            save_harness_png(
                &mut h,
                &dir.join(format!("history_{size_label}_before.png")),
            );
        }

        {
            let mut state = AppTestState::main_workspace();
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
            {
                let hist = state.controller.main_widget_mut().history_mut();
                hist.open_attach_box(false);
                if let Some(box_st) = hist.attach_box_mut() {
                    box_st.add_sample_video("demo_clip.mp4", 1280, 720, 2_450_000);
                }
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            step_frames(&mut h, 5);
            save_harness_png(
                &mut h,
                &dir.join(format!("attach_video_cover_{size_label}_before.png")),
            );
        }

        {
            let mut state = AppTestState::main_workspace();
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .session_mut()
                .expect("session")
                .show_peer_info(session_peers::USER_MIRA, SectionShow::default());
            step_frames(&mut h, 5);
            save_harness_png(
                &mut h,
                &dir.join(format!("profile_birthday_{size_label}_before.png")),
            );
        }
    }
}

#[test]
#[ignore = "Manual visual snapshot capture (writes after PNGs to RUSTGRAM_SNAPSHOT_DIR or temp dir)"]
fn capture_wp51_after_snapshots() {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp51"));
    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // 1. History workspace with AI Summary Header (expanded), Round-Video Seek Ring,
        //    Birthday Suggestion card, and Userpic Suggestion card.
        {
            let mut state = AppTestState::main_workspace();
            let mut handle = state.controller.session_handle();
            let (summary_id, round_video_id, _, _) = push_synthetic_wp51_messages(&handle);
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(
                    session_peers::USER_MIRA,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            // Expand the AI Summary header and reveal the circular seek ring at 62% progress.
            let _ = handle.toggle_message_summary(session_peers::USER_MIRA, summary_id, Some("en"));
            {
                let hist = h.state_mut().controller.main_widget_mut().history_mut();
                hist.inner_mut().invalidate_message_layout(summary_id);
                let rt = hist.inner_mut().media_runtime_mut();
                rt.seek_round_video(
                    session_peers::USER_MIRA,
                    round_video_id,
                    "Mira Vance",
                    42,
                    0.62,
                );
                rt.round_seek_mut(round_video_id).set_grabbed(true, 0.0);
                // Keep all 4 WP-51 items cleanly visible in frame at 1440x900 without clipping the bottom CTA.
                hist.inner_mut().request_scroll_to_bottom();
            }
            step_frames(&mut h, 10);
            save_harness_png(&mut h, &dir.join(format!("history_{size_label}_after.png")));
        }

        // 2. Attach Files box with video candidate + Custom Video Cover Picker open
        //    (verifies both desktop 1440px and compact mobile 390px layout with zero clipping).
        {
            let mut state = AppTestState::main_workspace();
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
            {
                let hist = state.controller.main_widget_mut().history_mut();
                hist.open_attach_box(false);
                if let Some(box_st) = hist.attach_box_mut() {
                    box_st.add_sample_video("demo_clip.mp4", 1280, 720, 2_450_000);
                }
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            step_frames(&mut h, 3);
            h.get_by_label("Cover").click();
            step_frames(&mut h, 4);
            if let Some(picker) = h
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .attach_box_mut()
                .and_then(|b| b.active_cover_picker.as_mut())
            {
                picker.selected_ms = 5_200;
            }
            step_frames(&mut h, 4);
            save_harness_png(
                &mut h,
                &dir.join(format!("attach_video_cover_{size_label}_after.png")),
            );
        }

        // 3. Profile Info pane for Mira Vance on her birthday (May 14, 1994 -> age 32)
        //    with confetti burst + cascading age digits active on both 1440px and 390px.
        {
            let mut state = AppTestState::main_workspace();
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            {
                let mw = h.state_mut().controller.main_widget_mut();
                mw.main_info_mut()
                    .set_birthday_today_override(Some((2026, 5, 14)));
                mw.third_info_mut()
                    .set_birthday_today_override(Some((2026, 5, 14)));
            }
            h.state_mut()
                .controller
                .session_mut()
                .expect("session")
                .show_peer_info(session_peers::USER_MIRA, SectionShow::default());
            step_frames(&mut h, 14);
            save_harness_png(
                &mut h,
                &dir.join(format!("profile_birthday_{size_label}_after.png")),
            );
        }
    }
}
