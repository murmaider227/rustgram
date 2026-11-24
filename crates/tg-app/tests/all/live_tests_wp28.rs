//! Integration & unit tests for WP-28:
//! - TranslateBox modal (`messages.translateText` with tone + language switching)
//! - Compose with AI & AI Tones (`messages.composeMessageWithAI`, `aicompose.*`, diff parsing,
//!   `ComposeAiBoxState`, `CreateAiToneBoxState`, `PreviewAiToneBoxState`)
//! - Layer-233 Polls (option media/link attachments, `open_answers`, `messages.addPollAnswer`,
//!   `messages.deletePollAnswer`, `CreatePollBoxState`, `AddPollOptionBoxState`, `PollLinkBoxState`)
//! - Self-Destructing Media (`SelfDestructionBoxState`, `account.getAccountTTL`/`setAccountTTL`,
//!   `account.setAuthorizationTTL`, `messages.getDefaultHistoryTTL`/`setDefaultHistoryTTL`,
//!   `messages.readMessageContents`, `ViewOnceMediaModalState`, `AttachFilesBoxState` TTL)
//! - `StickerCreatorBox` (`stickers.suggestShortName`, `checkShortName`, `createStickerSet`,
//!   `addStickerToSet`, `deleteStickerSet`, image preparation & WebP encoding)
//! - Headless-safe real webcam round-video capture (`tg-voip::video_mf` + `tg-video` + `SyntheticVideoSource`)
//! - Telegram Passport polish (`account.sendVerifyPhoneCode`, `auth.resendCode`, `account.verifyPhone`,
//!   `account.sendVerifyEmailCode`, `account.verifyEmail`, `account.deleteSecureValue`,
//!   `reset_passport` via `account.updatePasswordSettings` + `deleteSecureValue`)
//! - `MockSession` WP-28 demo data verification.
//!
//! All tests run strictly against in-process `FakeRpcClient`, `SyntheticVideoSource`, and
//! fictional placeholder data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{PeerUpdateFlags, User, UserId as DataUserId};
use tg_passport::types::{FormRequest, ValueType};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_voip::{SyntheticVideoSource, VideoCaptureSource};
use tokio::sync::broadcast;

use tg_app::boxes::compose_ai::{
    AiComposeTone, AiComposeToneRef, ComposeAiBackend, ComposeAiBoxState, ComposeAiDiff,
    ComposeAiDiffEntity, ComposeAiDiffKind, ComposeAiMode, ComposeAiRequest, CreateAiToneBoxState,
    DiffDisplaySegment, PreviewAiToneBoxState, TranslateBoxState, build_diff_display,
};
use tg_app::boxes::self_destruct::{
    ACCOUNT_TTL_DAYS_OPTIONS, MediaTtlPreset, SESSIONS_TTL_DAYS_OPTIONS, SelfDestructBackend,
    SelfDestructionBoxState, SelfDestructionBoxType, VIEW_ONCE_TTL_SECONDS, ViewOnceMediaKind,
    ViewOnceMediaModalState, format_media_ttl_badge, nearest_ttl_option, self_destruct_days_label,
};
use tg_app::boxes::sticker_creator::{
    AddStickerToSetRequest, CreateStickerSetRequest, STICKER_SIDE, StickerCreatorBackend,
    StickerCreatorBoxState, StickerSetKind, StickerTargetMode, generate_sample_sticker_rgba,
    prepare_static_sticker_image, validate_sticker_set_meta,
};
use tg_app::history::backend::{HistoryBackend, HistoryMedia, SendMessageExtended};
use tg_app::history::compose_boxes::{
    AddPollOptionBoxState, AttachFilesAction, AttachFilesBoxState, CreatePollBoxState,
    PollLinkBoxState,
};
use tg_app::history::sections_model::{
    AddPollOptionRequest, CreatePollRequest, PollMessageExtras, PollOptionAttachment,
    validate_poll_link_url,
};
use tg_app::live::LiveSession;
use tg_app::live::compose_ai::{
    ROUND_VIDEO_SIDE_PX, RoundVideoWebcamCapture, available_round_video_cameras,
    frame_to_square_rgba,
};
use tg_app::payments::backend::PassportBackend;
use tg_app::payments::mock::MockPassportBackend;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::types::{MsgId, PeerId};

// =============================================================================
// FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);
type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
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

    #[allow(dead_code)]
    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn count_ctor(&self, ctor: u32) -> usize {
        self.recorded().iter().filter(|(c, _)| *c == ctor).count()
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

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, Ok(raw))) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got {ctor:#010x}, expected {expected:#010x}"
                );
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some((expected, Err(err))) => {
                assert_eq!(ctor, expected);
                Err(err)
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, PeerId) {
    let client = FakeRpcClient::new(2);
    let self_uid = DataUserId(5001);
    let peer_uid = DataUserId(5002);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = SessionRuntime::start(
        self_uid,
        tg_app::live::ErasedRpcClient::new(client.clone()),
        cfg,
    )
    .expect("start runtime");
    let session_lock = runtime.session();

    runtime.tokio_handle().block_on(async {
        let mut s = session_lock.write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Alex".to_owned();
        self_user.last_name = "River".to_owned();
        self_user.access_hash = 111_222;
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut peer_user = User::new(peer_uid, false);
        peer_user.first_name = "Sam".to_owned();
        peer_user.last_name = "Taylor".to_owned();
        peer_user.access_hash = 333_444;
        s.upsert_user(peer_user, PeerUpdateFlags::empty());
    });

    let live = LiveSession::new(Arc::new(runtime), 1);
    (live, client, PeerId::from_user(peer_uid.0))
}

fn tl_text(s: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: s.to_owned(),
        entities: Vec::new(),
    })
}

// =============================================================================
// 1. TranslateBox & Compose with AI / AI Tones (FakeRpcClient)
// =============================================================================

#[test]
fn wp28_translate_box_live_rpc_with_tone_and_message_peer() {
    let (mut live, client, peer_id) = make_live_session();
    client.enqueue_ok(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![tl_text("Buenos dias, equipo.")],
        }),
    );

    let mut box_state = TranslateBoxState::new(
        Some(peer_id),
        Some(MsgId(42)),
        TextWithEntities::simple("Good morning, team."),
        "es",
    );
    box_state.tone = "formal".to_owned();
    box_state.run_translation(&mut live);

    assert!(box_state.error.is_none());
    assert_eq!(
        box_state.translated.as_ref().map(|t| t.text.as_str()),
        Some("Buenos dias, equipo.")
    );

    let calls = client.recorded();
    assert_eq!(calls.len(), 1);
    let sent = tl_fns::messages::TranslateText::from_bytes(&calls[0].1).expect("decode translate");
    assert_eq!(sent.to_lang, "es");
    assert_eq!(sent.tone.as_deref(), Some("formal"));
    assert_eq!(sent.id, Some(vec![42]));
    assert!(sent.peer.is_some());
}

#[test]
fn wp28_compose_with_ai_proofread_diff_and_custom_tones_live_rpc() {
    let (mut live, client, _) = make_live_session();

    // 1. aicompose.getTones
    let tones_resp = tl_enums::aicompose::Tones::Tones(tl_types::aicompose::Tones {
        hash: 777,
        tones: vec![
            tl_enums::AiComposeTone::Default(tl_types::AiComposeToneDefault {
                tone: "formal".to_owned(),
                emoji_id: 5368324170671202287,
                title: "Formal".to_owned(),
            }),
            tl_enums::AiComposeTone::AiComposeTone(tl_types::AiComposeTone {
                creator: true,
                id: 9001,
                access_hash: 88_001,
                slug: "release_style".to_owned(),
                title: "Release Note".to_owned(),
                emoji_id: Some(5368324170671202289),
                prompt: Some("Write as a release bullet.".to_owned()),
                installs_count: Some(15),
                author_id: Some(5001),
                example_english: Some(tl_enums::AiComposeToneExample::AiComposeToneExample(
                    tl_types::AiComposeToneExample {
                        from: tl_text("fixed a bug"),
                        to: tl_text("Resolved a minor stability issue."),
                    },
                )),
            }),
        ],
        users: Vec::new(),
    });
    client.enqueue_ok(tl_fns::aicompose::GetTones::CONSTRUCTOR_ID, &tones_resp);

    // 2. Second getTones returns NotModified (must preserve cached tones!)
    client.enqueue_ok(
        tl_fns::aicompose::GetTones::CONSTRUCTOR_ID,
        &tl_enums::aicompose::Tones::NotModified,
    );

    // 3. messages.composeMessageWithAI (proofread + diff entities)
    let diff_text = tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: "We received the update.".to_owned(),
        entities: vec![tl_enums::MessageEntity::DiffReplace(
            tl_types::MessageEntityDiffReplace {
                offset: 3,
                length: 8,
                old_text: "recieved".to_owned(),
            },
        )],
    });
    client.enqueue_ok(
        tl_fns::messages::ComposeMessageWithAI::CONSTRUCTOR_ID,
        &tl_enums::messages::ComposedMessageWithAI::ComposedMessageWithAI(
            tl_types::messages::ComposedMessageWithAI {
                result_text: tl_text("We received the update."),
                diff_text: Some(diff_text),
            },
        ),
    );

    // 4. aicompose.createTone -> updateTone -> getTone -> getToneExample -> saveTone -> deleteTone
    let created_tl = tl_enums::AiComposeTone::AiComposeTone(tl_types::AiComposeTone {
        creator: true,
        id: 9002,
        access_hash: 88_002,
        slug: "crisp_brief".to_owned(),
        title: "Crisp Brief".to_owned(),
        emoji_id: Some(5368324170671202290),
        prompt: Some("Keep it under two sentences.".to_owned()),
        installs_count: Some(1),
        author_id: Some(5001),
        example_english: None,
    });
    client.enqueue_ok(tl_fns::aicompose::CreateTone::CONSTRUCTOR_ID, &created_tl);
    let updated_tl = tl_enums::AiComposeTone::AiComposeTone(tl_types::AiComposeTone {
        creator: true,
        id: 9002,
        access_hash: 88_002,
        slug: "crisp_brief".to_owned(),
        title: "Crisp Brief v2".to_owned(),
        emoji_id: Some(5368324170671202290),
        prompt: Some("Keep it under one sentence.".to_owned()),
        installs_count: Some(1),
        author_id: Some(5001),
        example_english: None,
    });
    client.enqueue_ok(tl_fns::aicompose::UpdateTone::CONSTRUCTOR_ID, &updated_tl);
    client.enqueue_ok(
        tl_fns::aicompose::GetTone::CONSTRUCTOR_ID,
        &tl_enums::aicompose::Tones::Tones(tl_types::aicompose::Tones {
            hash: 0,
            tones: vec![updated_tl],
            users: Vec::new(),
        }),
    );
    client.enqueue_ok(
        tl_fns::aicompose::GetToneExample::CONSTRUCTOR_ID,
        &tl_enums::AiComposeToneExample::AiComposeToneExample(tl_types::AiComposeToneExample {
            from: tl_text("Long draft"),
            to: tl_text("Short brief."),
        }),
    );
    client.enqueue_ok(tl_fns::aicompose::SaveTone::CONSTRUCTOR_ID, &true);
    client.enqueue_ok(tl_fns::aicompose::DeleteTone::CONSTRUCTOR_ID, &true);

    let tones = live.list_ai_tones().expect("list tones");
    assert_eq!(tones.len(), 2);
    assert!(tones[0].is_default);
    assert_eq!(tones[1].slug, "release_style");

    let tones_cached = live.list_ai_tones().expect("cached tones");
    assert_eq!(tones_cached.len(), 2);

    let mut compose_box =
        ComposeAiBoxState::new(TextWithEntities::simple("We recieved the update."));
    compose_box.mode = ComposeAiMode::Fix;
    compose_box.tones = tones;
    compose_box.tones_loaded = true;
    compose_box.run_compose(&mut live);
    let res = compose_box.result.expect("proofread result");
    assert_eq!(res.result_text.text, "We received the update.");
    let diff = res.diff.expect("diff parsed");
    let segments = build_diff_display(&diff);
    assert_eq!(
        segments,
        vec![
            DiffDisplaySegment::Plain("We ".to_owned()),
            DiffDisplaySegment::Deleted("recieved".to_owned()),
            DiffDisplaySegment::Inserted("received".to_owned()),
            DiffDisplaySegment::Plain(" the update.".to_owned()),
        ]
    );

    let mut tone_box = CreateAiToneBoxState::new_create();
    tone_box.title = "Crisp Brief".to_owned();
    tone_box.prompt = "Keep it under two sentences.".to_owned();
    let created = tone_box.submit(&mut live).expect("create tone");
    assert_eq!(created.id, 9002);

    let mut edit_box = CreateAiToneBoxState::new_edit(created);
    edit_box.title = "Crisp Brief v2".to_owned();
    edit_box.prompt = "Keep it under one sentence.".to_owned();
    let updated = edit_box.submit(&mut live).expect("update tone");
    assert_eq!(updated.title, "Crisp Brief v2");

    let resolved = live.resolve_ai_tone("crisp_brief").expect("resolve tone");
    let mut preview_box = PreviewAiToneBoxState::new(resolved.clone(), false);
    preview_box.load_next_example(&mut live);
    assert_eq!(
        preview_box.example.as_ref().map(|e| e.to.text.as_str()),
        Some("Short brief.")
    );
    live.save_ai_tone(&resolved, false).expect("save tone");
    live.delete_ai_tone(&resolved).expect("delete tone");

    assert_eq!(
        client.count_ctor(tl_fns::aicompose::CreateTone::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::aicompose::UpdateTone::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::aicompose::SaveTone::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::aicompose::DeleteTone::CONSTRUCTOR_ID),
        1
    );
}

#[test]
fn wp28_compose_ai_diff_insert_delete_and_single_use_prompt() {
    let diff = ComposeAiDiff {
        text: TextWithEntities::simple("Hello wonderful world"),
        entities: vec![
            ComposeAiDiffEntity {
                kind: ComposeAiDiffKind::Insert,
                offset: 6,
                length: 10,
            },
            ComposeAiDiffEntity {
                kind: ComposeAiDiffKind::Delete,
                offset: 16,
                length: 5,
            },
        ],
    };
    let segs = build_diff_display(&diff);
    assert_eq!(
        segs,
        vec![
            DiffDisplaySegment::Plain("Hello ".to_owned()),
            DiffDisplaySegment::Inserted("wonderful ".to_owned()),
            DiffDisplaySegment::Deleted("world".to_owned()),
        ]
    );

    let (mut live, client, _) = make_live_session();
    client.enqueue_ok(
        tl_fns::messages::ComposeMessageWithAI::CONSTRUCTOR_ID,
        &tl_enums::messages::ComposedMessageWithAI::ComposedMessageWithAI(
            tl_types::messages::ComposedMessageWithAI {
                result_text: tl_text("Ahoy, shipping today! ⚓"),
                diff_text: None,
            },
        ),
    );
    let res = live
        .compose_with_ai(ComposeAiRequest {
            text: TextWithEntities::simple("Shipping today"),
            proofread: false,
            emojify: true,
            translate_to_lang: None,
            tone: Some(AiComposeToneRef::SingleUsePrompt(
                "Sound nautical".to_owned(),
            )),
        })
        .expect("single-use compose");
    assert_eq!(res.result_text.text, "Ahoy, shipping today! ⚓");
    let raw = &client.recorded()[0].1;
    let req = tl_fns::messages::ComposeMessageWithAI::from_bytes(raw).expect("decode compose");
    assert!(req.emojify);
    assert!(matches!(
        req.tone,
        Some(tl_enums::InputAiComposeTone::SingleUse(_))
    ));
}

// =============================================================================
// 2. Layer-233 Polls: Option Media/Link Attachments + Add/Delete Poll Option
// =============================================================================

#[test]
fn wp28_layer233_poll_option_attachments_and_add_delete_option_live_rpc() {
    let (mut live, client, peer_id) = make_live_session();

    // Expect: 1) upload.saveFilePart for media option, 2) messages.sendMedia(inputMediaPoll),
    // 3) messages.addPollAnswer (with link), 4) messages.deletePollAnswer.
    client.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    client.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
            out: true,
            id: 777,
            pts: 10,
            pts_count: 1,
            date: 1_795_300_000,
            media: None,
            entities: None,
            ttl_period: None,
        }),
    );
    client.enqueue_ok(
        tl_fns::messages::AddPollAnswer::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_300_010,
            seq: 1,
        }),
    );
    client.enqueue_ok(
        tl_fns::messages::DeletePollAnswer::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_300_020,
            seq: 2,
        }),
    );

    let mut poll_box = CreatePollBoxState::new();
    poll_box.question = "Which logo concept?".to_owned();
    poll_box.options[0] = "Concept A".to_owned();
    poll_box.options[1] = "Concept B".to_owned();
    poll_box.open_answers = true;
    poll_box.shuffle_answers = true;
    poll_box.hide_results_until_close = true;
    poll_box.set_option_attachment(
        0,
        Some(PollOptionAttachment::Media {
            file_name: "concept_a.jpg".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xD9],
        }),
    );
    let mut link_box = PollLinkBoxState::new(Some(1), "example.test/concept-b");
    let link_att = link_box.validate().expect("valid normalized link");
    poll_box.set_option_attachment(1, Some(link_att));

    let (req, extras) = poll_box.build_request_with_extras().expect("build poll");
    assert!(extras.open_answers);
    assert!(extras.shuffle_answers);
    assert!(extras.hide_results_until_close);

    let sent_msg = live
        .send_poll_with_extras(peer_id, None, req, extras)
        .expect("send Layer-233 poll");
    assert_eq!(sent_msg.id, MsgId(777));
    assert!(
        sent_msg
            .extra
            .poll_extras
            .as_ref()
            .is_some_and(|e| e.open_answers)
    );

    // Verify wire InputMediaPoll had open_answers + option media!
    let calls = client.recorded();
    let send_media_req =
        tl_fns::messages::SendMedia::from_bytes(&calls[1].1).expect("decode SendMedia");
    if let tl_enums::InputMedia::Poll(p) = send_media_req.media {
        let tl_enums::Poll::Poll(inner) = p.poll;
        assert!(inner.open_answers);
        assert!(inner.shuffle_answers);
        assert!(inner.hide_results_until_close);
        assert_eq!(inner.answers.len(), 2);
        assert!(matches!(
            inner.answers[0],
            tl_enums::PollAnswer::InputPollAnswer(tl_types::InputPollAnswer {
                media: Some(tl_enums::InputMedia::UploadedPhoto(_)),
                ..
            })
        ));
        assert!(matches!(
            inner.answers[1],
            tl_enums::PollAnswer::InputPollAnswer(tl_types::InputPollAnswer {
                media: Some(tl_enums::InputMedia::WebPage(_)),
                ..
            })
        ));
    } else {
        panic!("expected InputMedia::Poll");
    }

    // Add option via AddPollOptionBoxState + messages.addPollAnswer
    let mut add_box = AddPollOptionBoxState::new(MsgId(777), "Which logo concept?");
    add_box.text = "Concept C".to_owned();
    add_box.attachment = Some(PollOptionAttachment::Link {
        url: "https://example.test/concept-c".to_owned(),
        title: Some("Concept C".to_owned()),
    });
    let add_req = add_box.build_request().expect("valid add option");
    let _ = live
        .add_poll_option(peer_id, MsgId(777), add_req)
        .expect("addPollAnswer");
    assert_eq!(
        live.cached_poll_extras(peer_id, MsgId(777))
            .map(|e| e.option_bytes.len()),
        Some(3)
    );

    // Delete added option via messages.deletePollAnswer
    live.delete_poll_option(peer_id, MsgId(777), &[2])
        .expect("deletePollAnswer");
    assert_eq!(
        live.cached_poll_extras(peer_id, MsgId(777))
            .map(|e| e.option_bytes.len()),
        Some(2)
    );

    assert!(validate_poll_link_url("   ").is_err());
    assert_eq!(
        validate_poll_link_url("t.me/rustgram").expect("prepend https"),
        "https://t.me/rustgram"
    );
}

// =============================================================================
// 3. Self-Destructing Media & SelfDestructionBox (FakeRpcClient)
// =============================================================================

#[test]
fn wp28_self_destruct_box_api_and_view_once_media_upload_live_rpc() {
    let (mut live, client, peer_id) = make_live_session();

    // 1. reload_self_destruct -> account.getAccountTTL + messages.getDefaultHistoryTTL
    client.enqueue_ok(
        tl_fns::account::GetAccountTTL::CONSTRUCTOR_ID,
        &tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days: 180 }),
    );
    client.enqueue_ok(
        tl_fns::messages::GetDefaultHistoryTTL::CONSTRUCTOR_ID,
        &tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(tl_types::DefaultHistoryTTL {
            period: 86_400,
        }),
    );
    // 2. updateAccountTTL, updateSessionsTTL, updateDefaultHistoryTTL, mark_ttl_media_viewed
    client.enqueue_ok(tl_fns::account::SetAccountTTL::CONSTRUCTOR_ID, &true);
    client.enqueue_ok(tl_fns::account::SetAuthorizationTTL::CONSTRUCTOR_ID, &true);
    client.enqueue_ok(
        tl_fns::messages::SetDefaultHistoryTTL::CONSTRUCTOR_ID,
        &true,
    );
    client.enqueue_ok(
        tl_fns::messages::ReadMessageContents::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 12,
                pts_count: 1,
            },
        ),
    );

    let snap = live.reload_self_destruct().expect("reload self destruct");
    assert_eq!(snap.account_ttl_days, 180);
    assert_eq!(snap.default_history_ttl_secs, 86_400);

    let mut acct_box = SelfDestructionBoxState::new(SelfDestructionBoxType::Account, 180);
    acct_box.loaded = true;
    acct_box.selected = 365;
    assert_eq!(acct_box.save(&mut live).expect("save account ttl"), 365);

    let mut sess_box = SelfDestructionBoxState::new(SelfDestructionBoxType::Sessions, 180);
    sess_box.loaded = true;
    sess_box.selected = 30;
    assert_eq!(sess_box.save(&mut live).expect("save sessions ttl"), 30);

    let mut hist_box = SelfDestructionBoxState::new(SelfDestructionBoxType::DefaultHistory, 0);
    hist_box.loaded = true;
    hist_box.selected = 604_800;
    assert_eq!(
        hist_box.save(&mut live).expect("save default history ttl"),
        604_800
    );

    SelfDestructBackend::mark_ttl_media_viewed(&mut live, peer_id, MsgId(555))
        .expect("readMessageContents");
    assert!(live.is_ttl_media_viewed(peer_id, MsgId(555)));

    // Verify helper math & AttachFilesBoxState view-once TTL propagation
    assert_eq!(self_destruct_days_label(7), "1 week");
    assert_eq!(self_destruct_days_label(180), "6 months");
    assert_eq!(nearest_ttl_option(175, ACCOUNT_TTL_DAYS_OPTIONS), 180);
    assert_eq!(nearest_ttl_option(10, SESSIONS_TTL_DAYS_OPTIONS), 7);
    assert_eq!(
        format_media_ttl_badge(VIEW_ONCE_TTL_SECONDS, false),
        "1 · View Once"
    );
    assert_eq!(
        format_media_ttl_badge(VIEW_ONCE_TTL_SECONDS, true),
        "Expired"
    );

    let mut attach = AttachFilesBoxState::new(false, false);
    attach.ttl_preset = MediaTtlPreset::ViewOnce;
    let AttachFilesAction::Send(sub) = attach.build_action_for_send() else {
        panic!("expected Send action");
    };
    assert_eq!(sub.ttl_seconds, Some(VIEW_ONCE_TTL_SECONDS));

    let modal = ViewOnceMediaModalState::open(
        peer_id,
        MsgId(555),
        "Sam Taylor",
        ViewOnceMediaKind::Photo,
        10,
        "Secret diagram",
        ([30, 50, 80], [90, 150, 220]),
        100.0,
    );
    assert_eq!(modal.remaining_secs(104.0), Some(6.0));
    assert_eq!(modal.remaining_secs(115.0), Some(0.0));
}

// =============================================================================
// 4. StickerCreatorBox (`api_stickers_creator`, `stickers.createStickerSet` / `addStickerToSet`)
// =============================================================================

#[test]
fn wp28_sticker_creator_create_and_add_live_rpc() {
    let (mut live, client, _) = make_live_session();

    // Dynamic handler supporting upload.saveFilePart, messages.uploadMedia,
    // stickers.suggestShortName, stickers.checkShortName, stickers.createStickerSet,
    // stickers.addStickerToSet, and stickers.deleteStickerSet.
    client.set_dynamic_handler(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::UploadMedia::CONSTRUCTOR_ID {
            let media = tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(tl_enums::Document::Document(tl_types::Document {
                    id: 66_001,
                    access_hash: 66_999,
                    file_reference: vec![1, 2, 3],
                    date: 1_795_300_000,
                    mime_type: "image/webp".to_owned(),
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
            });
            return Some(Ok(media.to_bytes()));
        }
        if ctor == tl_fns::stickers::SuggestShortName::CONSTRUCTOR_ID {
            let s = tl_enums::stickers::SuggestedShortName::SuggestedShortName(
                tl_types::stickers::SuggestedShortName {
                    short_name: "rust_mascots_pack".to_owned(),
                },
            );
            return Some(Ok(s.to_bytes()));
        }
        if ctor == tl_fns::stickers::CheckShortName::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::stickers::CreateStickerSet::CONSTRUCTOR_ID
            || ctor == tl_fns::stickers::AddStickerToSet::CONSTRUCTOR_ID
        {
            let count = if ctor == tl_fns::stickers::CreateStickerSet::CONSTRUCTOR_ID {
                1
            } else {
                2
            };
            let set = tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
                set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                    archived: false,
                    official: false,
                    masks: false,
                    emojis: false,
                    text_color: false,
                    channel_emoji_status: false,
                    creator: true,
                    installed_date: Some(1_795_300_000),
                    id: 77_001,
                    access_hash: 77_999,
                    title: "Rust Mascots".to_owned(),
                    short_name: "rust_mascots_pack".to_owned(),
                    thumbs: None,
                    thumb_dc_id: None,
                    thumb_version: None,
                    thumb_document_id: None,
                    count,
                    hash: 42,
                }),
                packs: Vec::new(),
                keywords: Vec::new(),
                documents: Vec::new(),
            });
            return Some(Ok(set.to_bytes()));
        }
        if ctor == tl_fns::stickers::DeleteStickerSet::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    // Prepare a real 512x512 WebP sticker asset via pure-Rust `image` pipeline.
    let rgba = generate_sample_sticker_rgba(7, 256);
    let asset = prepare_static_sticker_image(&rgba, StickerSetKind::Regular)
        .expect("prepare 512px sticker");
    assert_eq!(asset.width, STICKER_SIDE);
    assert_eq!(asset.height, STICKER_SIDE);
    assert!(!asset.bytes.is_empty());

    assert!(validate_sticker_set_meta("Rust Mascots", "rust_mascots_pack").is_ok());
    assert!(validate_sticker_set_meta("Bad", "123_bad").is_err());

    let suggested = live
        .suggest_sticker_short_name("Rust Mascots")
        .expect("suggestShortName");
    assert_eq!(suggested, "rust_mascots_pack");
    assert!(
        live.check_sticker_short_name(&suggested)
            .expect("checkShortName")
    );

    let created = live
        .create_sticker_set(CreateStickerSetRequest {
            title: "Rust Mascots".to_owned(),
            short_name: suggested.clone(),
            kind: StickerSetKind::Regular,
            asset: asset.clone(),
            emoji: "🦀✨".to_owned(),
            keywords: "rust,crab".to_owned(),
        })
        .expect("createStickerSet");
    assert_eq!(created.id, 77_001);
    assert_eq!(created.count, 1);

    let updated = live
        .add_sticker_to_set(AddStickerToSetRequest {
            set_id: created.id,
            set_access_hash: created.access_hash,
            set_short_name: created.short_name.clone(),
            kind: StickerSetKind::Regular,
            asset,
            emoji: "🔥".to_owned(),
            keywords: "fire".to_owned(),
        })
        .expect("addStickerToSet");
    assert_eq!(updated.count, 2);

    live.delete_sticker_set(updated.id, updated.access_hash, &updated.short_name)
        .expect("deleteStickerSet");

    assert_eq!(
        client.count_ctor(tl_fns::stickers::CreateStickerSet::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::stickers::AddStickerToSet::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::messages::UploadMedia::CONSTRUCTOR_ID),
        2
    );
}

// =============================================================================
// 5. Real Webcam Round-Video Capture (headless-safe + SyntheticVideoSource + tg-video)
// =============================================================================

#[test]
fn wp28_round_video_capture_headless_safe_and_square_crop_export() {
    // 1. Headless safety: never panics even when RUSTGRAM_HEADLESS_AUDIO=1 or no webcam exists.
    let _cams = available_round_video_cameras();
    let _maybe_cam = RoundVideoWebcamCapture::try_start_default().expect("headless-safe try_start");

    // 2. Deterministic end-to-end capture using SyntheticVideoSource -> square crop -> tg-video MP4!
    let mut synth = SyntheticVideoSource::new(640, 480, 30);
    let sample_frame = synth
        .next_frame()
        .expect("next_frame")
        .expect("synthetic frame");
    let preview =
        frame_to_square_rgba(&sample_frame, ROUND_VIDEO_SIDE_PX).expect("square crop RGBA");
    assert_eq!(preview.side, ROUND_VIDEO_SIDE_PX);
    assert_eq!(
        preview.rgba.len(),
        (ROUND_VIDEO_SIDE_PX * ROUND_VIDEO_SIDE_PX * 4) as usize
    );

    let mut session = RoundVideoWebcamCapture::start_with_source(Box::new(synth), false);
    std::thread::sleep(Duration::from_millis(60));
    assert!(session.frame_count() >= 1);
    assert!(session.latest_preview().is_some());

    let tmp = tempfile::tempdir().expect("tempdir");
    let captured = session.finish(tmp.path(), Some(2));
    assert_eq!(captured.duration_secs, 2);
    assert!(captured.frame_count >= 1);
    if tg_video::encode::ffmpeg_runtime_available() {
        let mp4 = captured.mp4_path.expect("tg-video exported round MP4");
        assert!(mp4.is_file());
        assert!(std::fs::metadata(&mp4).expect("meta").len() > 256);
    }
}

// =============================================================================
// 6. Passport Polish: Phone/Email Verification Codes + Delete/Reset (FakeRpcClient)
// =============================================================================

#[test]
fn wp28_passport_phone_email_verification_delete_and_reset_live_rpc() {
    let (mut live, client, _) = make_live_session();
    live.configure_form_request(FormRequest {
        bot_id: DataUserId(5002),
        scope: "phone,email".to_owned(),
        callback_url: "https://example.test/cb".to_owned(),
        public_key: "-----BEGIN PUBLIC KEY-----\nTEST\n-----END PUBLIC KEY-----".to_owned(),
        nonce: "nonce_wp28".to_owned(),
    });

    // Dynamic handler for Passport form load, phone/email verification, save, delete, and reset!
    client.set_dynamic_handler(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID {
            let af = tl_enums::account::AuthorizationForm::AuthorizationForm(
                tl_types::account::AuthorizationForm {
                    required_types: vec![
                        tl_enums::SecureRequiredType::SecureRequiredType(
                            tl_types::SecureRequiredType {
                                native_names: false,
                                selfie_required: false,
                                translation_required: false,
                                r#type: tl_enums::SecureValueType::Phone,
                            },
                        ),
                        tl_enums::SecureRequiredType::SecureRequiredType(
                            tl_types::SecureRequiredType {
                                native_names: false,
                                selfie_required: false,
                                translation_required: false,
                                r#type: tl_enums::SecureValueType::Email,
                            },
                        ),
                    ],
                    values: Vec::new(),
                    errors: Vec::new(),
                    users: Vec::new(),
                    privacy_policy_url: None,
                },
            );
            return Some(Ok(af.to_bytes()));
        }
        if ctor == tl_fns::account::GetPassword::CONSTRUCTOR_ID {
            let pwd = tl_enums::account::Password::Password(tl_types::account::Password {
                has_recovery: false,
                has_secure_values: true,
                has_password: false,
                current_algo: None,
                srp_b: None,
                srp_id: None,
                hint: None,
                email_unconfirmed_pattern: None,
                new_algo: tl_enums::PasswordKdfAlgo::Unknown,
                new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
                secure_random: vec![0u8; 256],
                pending_reset_date: None,
                login_email_pattern: None,
            });
            return Some(Ok(pwd.to_bytes()));
        }
        if ctor == tl_fns::account::SendVerifyPhoneCode::CONSTRUCTOR_ID
            || ctor == tl_fns::auth::ResendCode::CONSTRUCTOR_ID
        {
            let sent = tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms {
                    length: 5,
                }),
                phone_code_hash: "hash_phone_wp28".to_owned(),
                next_type: None,
                timeout: Some(60),
            });
            return Some(Ok(sent.to_bytes()));
        }
        if ctor == tl_fns::account::VerifyPhone::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::account::SendVerifyEmailCode::CONSTRUCTOR_ID {
            let sent =
                tl_enums::account::SentEmailCode::SentEmailCode(tl_types::account::SentEmailCode {
                    email_pattern: "a***@example.test".to_owned(),
                    length: 6,
                });
            return Some(Ok(sent.to_bytes()));
        }
        if ctor == tl_fns::account::VerifyEmail::CONSTRUCTOR_ID {
            let ok =
                tl_enums::account::EmailVerified::EmailVerified(tl_types::account::EmailVerified {
                    email: "alex.river@example.test".to_owned(),
                });
            return Some(Ok(ok.to_bytes()));
        }
        if ctor == tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID {
            let saved = tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                r#type: tl_enums::SecureValueType::Phone,
                data: None,
                front_side: None,
                reverse_side: None,
                selfie: None,
                translation: None,
                files: None,
                plain_data: Some(tl_enums::SecurePlainData::SecurePlainPhone(
                    tl_types::SecurePlainPhone {
                        phone: "9996610001".to_owned(),
                    },
                )),
                hash: vec![9, 8, 7],
            });
            return Some(Ok(saved.to_bytes()));
        }
        if ctor == tl_fns::account::DeleteSecureValue::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    let _bundle = live
        .load_authorization_form("vericheck_demo_bot")
        .expect("load passport form");

    // Phone verification flow: send -> resend -> verify + save
    let phone_ch = live
        .send_contact_verification_code(ValueType::Phone, "9996610001")
        .expect("sendVerifyPhoneCode");
    assert_eq!(phone_ch.code_length, 5);
    let resent = live
        .resend_phone_verification_code(
            "9996610001",
            phone_ch.phone_code_hash.as_deref().unwrap_or_default(),
        )
        .expect("auth.resendCode");
    let _form_phone = live
        .confirm_contact_verification_code(
            ValueType::Phone,
            "9996610001",
            resent.phone_code_hash.as_deref(),
            "12345",
        )
        .expect("verifyPhone + saveSecureValue");

    // Email verification flow: send -> verify + save
    let email_ch = live
        .send_contact_verification_code(ValueType::Email, "alex.river@example.test")
        .expect("sendVerifyEmailCode");
    assert_eq!(email_ch.code_length, 6);
    assert_eq!(email_ch.email_pattern.as_deref(), Some("a***@example.test"));
    let _form_email = live
        .confirm_contact_verification_code(
            ValueType::Email,
            "alex.river@example.test",
            None,
            "123456",
        )
        .expect("verifyEmail + saveSecureValue");

    // Delete single value & full reset
    let _after_del = live
        .delete_passport_values(&[ValueType::Phone])
        .expect("deleteSecureValue");
    let reset_res = live.reset_passport("").expect("reset_passport");
    assert!(reset_res.notice.contains("reset"));

    assert_eq!(
        client.count_ctor(tl_fns::account::SendVerifyPhoneCode::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::auth::ResendCode::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::account::VerifyPhone::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::account::SendVerifyEmailCode::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::account::VerifyEmail::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        client.count_ctor(tl_fns::account::DeleteSecureValue::CONSTRUCTOR_ID),
        2
    );
}

// =============================================================================
// 7. MockSession Demo Data & Offline Backends
// =============================================================================

#[test]
fn wp28_mock_session_demo_data_end_to_end() {
    let mut session = MockSession::new(true);

    // 1. Demo AI tones & Compose with AI
    {
        let ai = session
            .as_compose_ai_backend()
            .expect("MockSession exposes ComposeAiBackend");
        let tones = ai.list_ai_tones().expect("demo tones");
        assert!(tones.len() >= 5);
        assert!(tones.iter().any(|t| !t.is_default && t.creator));
        let proof = ai
            .compose_with_ai(ComposeAiRequest {
                text: TextWithEntities::simple("teh build is green"),
                proofread: true,
                emojify: false,
                translate_to_lang: None,
                tone: None,
            })
            .expect("demo proofread");
        assert_eq!(proof.result_text.text, "The build is green.");
        assert!(proof.diff.is_some());
    }

    // 2. Demo StickerCreatorBox
    {
        let sc = session
            .as_sticker_creator_backend()
            .expect("MockSession exposes StickerCreatorBackend");
        let mut box_state = StickerCreatorBoxState::new(StickerSetKind::Regular);
        box_state.target_mode = StickerTargetMode::AddToExisting;
        let saved = box_state.submit(sc).expect("add demo sticker to owned set");
        assert_eq!(saved.count, 9);
    }

    // 3. Demo Self-Destruct backend & view-once demo message in USER_MIRA
    let slice = session.load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0);
    let view_once_msg = slice
        .messages
        .iter()
        .find(|m| m.extra.ttl_seconds == Some(VIEW_ONCE_TTL_SECONDS))
        .expect("MockSession includes a demo view-once photo bubble");
    assert!(!view_once_msg.extra.ttl_viewed);
    let view_once_id = view_once_msg.id;
    session
        .mark_ttl_media_viewed(session_peers::USER_MIRA, view_once_id)
        .expect("mark view-once viewed");
    let slice_after =
        session.load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0);
    assert!(
        slice_after
            .messages
            .iter()
            .find(|m| m.id == view_once_id)
            .expect("msg exists")
            .extra
            .ttl_viewed
    );

    // 4. Demo Layer-233 open-answers poll with option link attachment in USER_MIRA
    let poll_msg = slice_after
        .messages
        .iter()
        .find(|m| {
            matches!(m.media, Some(HistoryMedia::Poll(_)))
                && m.extra.poll_extras.as_ref().is_some_and(|e| e.open_answers)
        })
        .expect("MockSession includes demo open-answers poll");
    let poll_id = poll_msg.id;
    let updated_poll = session
        .add_poll_option(
            session_peers::USER_MIRA,
            poll_id,
            AddPollOptionRequest {
                text: "Solarized Slate".to_owned(),
                attachment: Some(PollOptionAttachment::Link {
                    url: "https://example.test/themes/solarized".to_owned(),
                    title: Some("Solarized Preview".to_owned()),
                }),
            },
        )
        .expect("add poll option in MockSession");
    if let Some(HistoryMedia::Poll(p)) = updated_poll.media {
        assert_eq!(p.options.len(), 3);
    } else {
        panic!("expected poll media");
    }

    // 5. Demo MockPassportBackend verification + delete + reset
    let mut passport = MockPassportBackend::new();
    let ch = passport
        .send_contact_verification_code(ValueType::Phone, "15550109999")
        .expect("mock phone code");
    assert!(
        passport
            .confirm_contact_verification_code(
                ValueType::Phone,
                "15550109999",
                ch.phone_code_hash.as_deref(),
                "00000"
            )
            .is_err()
    );
    let verified = passport
        .confirm_contact_verification_code(
            ValueType::Phone,
            "15550109999",
            ch.phone_code_hash.as_deref(),
            "12345",
        )
        .expect("mock phone verify");
    assert_eq!(
        verified
            .values
            .get(&ValueType::Phone)
            .map(|v| v.data.parsed.text("value")),
        Some("15550109999")
    );
    let after_del = passport
        .delete_passport_values(&[ValueType::Email])
        .expect("mock delete email");
    assert!(
        after_del.values.get(&ValueType::Email).is_some_and(|v| v
            .data
            .parsed
            .text("value")
            .is_empty())
    );
    let after_reset = passport.reset_passport("demo1234").expect("mock reset");
    assert!(
        after_reset
            .form
            .values
            .get(&ValueType::PersonalDetails)
            .is_some_and(|v| v.data.parsed.text("first_name").is_empty())
    );

    // 6. SendMessageExtended carries ttl_seconds & round_video_path cleanly
    let ext = SendMessageExtended {
        ttl_seconds: Some(15),
        round_video_path: Some("demo_round.mp4".to_owned()),
        ..SendMessageExtended::default()
    };
    assert_eq!(ext.ttl_seconds, Some(15));
    assert_eq!(ext.round_video_path.as_deref(), Some("demo_round.mp4"));

    // Keep unused import check clean across all types
    let _sample_tone = AiComposeTone {
        id: 1,
        access_hash: 2,
        slug: "s".to_owned(),
        title: "T".to_owned(),
        emoji_id: 0,
        emoji_label: "✨".to_owned(),
        prompt: "P".to_owned(),
        installs_count: 0,
        author_id: 0,
        creator: true,
        is_default: false,
        default_type: String::new(),
        first_example: None,
    };
    let _ = CreatePollRequest {
        question: "Q".to_owned(),
        options: vec!["A".to_owned(), "B".to_owned()],
        is_anonymous: true,
        multiple_choice: false,
        is_quiz: false,
        correct_option: None,
        explanation: None,
        silent: false,
        schedule_date: None,
    };
    let _ = PollMessageExtras::default();
}
