//! TGB-3015: a recorded voice message is sent. The recorder's OGG/Opus file reaches
//! `upload.saveFilePart`, `messages.sendMedia` carries tdesktop's single voice attribute with the
//! duration and waveform, and the sent message replaces the recording. Before the fix the
//! recorder dropped the encoded bytes, the live backend found no "voice.ogg" and the widget
//! swallowed the error: no request ever left the client.
//!
//! The recorder runs on its synthetic capture source: unit tests never open the microphone.
//! The server side is a `FakeRpcClient` answering with the layer-233 shapes the real server
//! sends (no network).

use std::sync::{Arc, Mutex};

use egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_api::rpc::RpcError;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::PeerId;
use tg_app_live::convert::from_data_peer;
use tg_app_live::session::LiveSession;
use tg_data::{PeerId as DataPeerId, PeerUpdateFlags, User, UserId as DataUserId};
use tg_media::audio::waveform_decode_5bit;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tokio::sync::broadcast;

use crate::history::HistoryWidget;
use crate::history::backend::HistoryMedia;
use crate::history::compose_extras::VoiceRecorderController;

const SELF_ID: i64 = 100;
const PEER_ID: i64 = 301;
const SENT_ID: i32 = 4_001;
const NOW: i32 = 1_795_341_600;

type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;
type Responder = Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    recorded: RecordedCalls,
    responder: Arc<Mutex<Option<Responder>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            recorded: Arc::new(Mutex::new(Vec::new())),
            responder: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn respond_with(&self, responder: Responder) {
        *self.responder.lock().expect("lock responder") = Some(responder);
    }

    fn recorded_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .filter_map(|(_, bytes)| R::from_bytes(bytes).ok())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
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
        let responder = self.responder.lock().expect("lock responder").clone();
        if let Some(responder) = responder
            && let Some(result) = responder(ctor, &bytes)
        {
            let raw = result?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }
        // Everything else (history pages, typing) goes unanswered.
        Err(InvocationError::Dropped)
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

struct VoiceChat {
    widget: HistoryWidget,
    live: LiveSession,
    peer: PeerId,
    palette: Palette,
    _rt: tokio::runtime::Runtime,
}

fn start_chat(fake: &FakeRpcClient) -> Harness<'static, VoiceChat> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let runtime = SessionRuntime::start_on(
        DataUserId(SELF_ID as u64),
        ErasedRpcClient::new(fake.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            fetch_self_user_on_start: false,
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    rt.block_on(runtime.wait_ready())
        .expect("SessionRuntime ready");
    let live = LiveSession::new(Arc::new(runtime), 1);
    let peer_data = DataPeerId::from_user(DataUserId(PEER_ID as u64));
    live.write_session(|s| {
        let mut me = User::new(DataUserId(SELF_ID as u64), true);
        me.first_name = "Account A".to_owned();
        me.common.name = "Account A".to_owned();
        me.common.loaded_status = tg_data::LoadedStatus::Full;
        s.upsert_user(me, PeerUpdateFlags::empty());
        let mut peer = User::new(DataUserId(PEER_ID as u64), false);
        peer.first_name = "Account B".to_owned();
        peer.common.name = "Account B".to_owned();
        peer.common.loaded_status = tg_data::LoadedStatus::Full;
        peer.access_hash = 11_111;
        s.upsert_user(peer, PeerUpdateFlags::empty());
        s.set_history_unread_count(peer_data, 0);
    });

    let state = VoiceChat {
        widget: HistoryWidget::new(),
        live,
        peer: from_data_peer(peer_data),
        palette: Palette::day(),
        _rt: rt,
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(720.0, 520.0))
        .with_step_dt(0.1)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut VoiceChat| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let _ = st.widget.show_with_backend(
                            ui,
                            st.peer,
                            &mut st.live,
                            true,
                            false,
                            false,
                            &st.palette,
                            &EnglishStrings,
                        );
                    });
            },
            state,
        );
    for _ in 0..3 {
        harness.step();
    }
    harness
}

/// Presses the mic button, drags a few pixels (starts the hold), holds for `hold_secs` of
/// frames and releases: `VoiceRecordBar`'s hold-to-record gesture.
fn hold_mic(harness: &mut Harness<'_, VoiceChat>, hold_secs: f64) {
    let mic = harness.get_by_label("Record voice message").rect().center();
    let press = |pos: Pos2, pressed: bool| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    harness.input_mut().events.push(Event::PointerMoved(mic));
    harness.step();
    harness.input_mut().events.push(press(mic, true));
    harness.step();
    let held = mic + Vec2::new(10.0, 0.0);
    harness.input_mut().events.push(Event::PointerMoved(held));
    harness.step();
    assert!(
        harness.state().widget.compose.voice_recorder.is_active(),
        "the hold starts a recording"
    );
    assert!(
        !harness
            .state()
            .widget
            .compose
            .voice_recorder
            .uses_hardware_capture(),
        "tests record from the synthetic source, never the microphone"
    );
    let frames = (hold_secs / 0.1).round() as usize;
    for _ in 0..frames {
        harness.step();
    }
    harness.input_mut().events.push(press(held, false));
    harness.step();
}

fn sent_voice_updates(send: &tl_fns::messages::SendMedia) -> tl_enums::Updates {
    let tl_enums::InputMedia::UploadedDocument(doc) = &send.media else {
        panic!("voice goes out as an uploaded document: {:?}", send.media);
    };
    let media = tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: true,
        document: Some(tl_enums::Document::Document(tl_types::Document {
            id: 77_001,
            access_hash: 77_002,
            file_reference: vec![1, 2, 3],
            date: NOW,
            mime_type: doc.mime_type.clone(),
            size: 4_096,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: doc.attributes.clone(),
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    });
    let message = tl_enums::Message::Message(tl_types::Message {
        out: true,
        mentioned: false,
        media_unread: true,
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
        id: SENT_ID,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SELF_ID,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: PEER_ID }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: NOW,
        message: String::new(),
        media: Some(media),
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
    });
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                id: SENT_ID,
                random_id: send.random_id,
            }),
            tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message,
                pts: 10,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: NOW,
        seq: 0,
    })
}

/// The server: every file part is accepted; `messages.sendMedia` answers with the new message.
fn accept_uploads_and_sends(fake: &FakeRpcClient) {
    fake.respond_with(Arc::new(|ctor, raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            let send = tl_fns::messages::SendMedia::from_bytes(raw).ok()?;
            return Some(Ok(sent_voice_updates(&send).to_bytes()));
        }
        None
    }));
}

#[test]
fn holding_the_mic_sends_the_recorded_ogg_with_duration_and_waveform() {
    let fake = FakeRpcClient::new();
    accept_uploads_and_sends(&fake);
    let mut harness = start_chat(&fake);

    hold_mic(&mut harness, 2.5);

    // The recorded file itself was uploaded: an Ogg stream with an Opus head.
    let parts = fake.recorded_of::<tl_fns::upload::SaveFilePart>();
    assert!(
        !parts.is_empty(),
        "the old code never uploaded the recording"
    );
    let mut file = Vec::new();
    for part in &parts {
        file.extend_from_slice(&part.bytes);
    }
    assert!(file.starts_with(b"OggS"), "uploaded bytes are an Ogg file");
    assert!(
        file.windows(8).any(|w| w == b"OpusHead"),
        "the Ogg stream carries Opus"
    );

    // `messages.sendMedia` with tdesktop's voice document.
    let sends = fake.recorded_of::<tl_fns::messages::SendMedia>();
    assert_eq!(sends.len(), 1, "exactly one voice message goes out");
    let send = &sends[0];
    assert!(matches!(
        send.peer,
        tl_enums::InputPeer::User(ref u) if u.user_id == PEER_ID
    ));
    assert!(send.message.is_empty(), "a voice message has no caption");
    let tl_enums::InputMedia::UploadedDocument(doc) = &send.media else {
        panic!("expected an uploaded document, got {:?}", send.media);
    };
    assert_eq!(doc.mime_type, "audio/ogg");
    assert!(!doc.force_file);
    assert_eq!(
        doc.attributes.len(),
        1,
        "FileLoadTask keeps only the audio attribute for a voice note: {:?}",
        doc.attributes
    );
    let tl_enums::DocumentAttribute::Audio(audio) = &doc.attributes[0] else {
        panic!(
            "expected documentAttributeAudio, got {:?}",
            doc.attributes[0]
        );
    };
    assert!(audio.voice);
    assert!(
        (2..=3).contains(&audio.duration),
        "about 2.5 s were recorded, got {} s",
        audio.duration
    );
    let waveform = audio.waveform.as_deref().expect("waveform attached");
    let bars = waveform_decode_5bit(waveform);
    assert_eq!(bars.len(), 100, "tdesktop's 100-sample waveform");
    assert!(
        bars.iter().any(|&b| b > 0),
        "the waveform follows the recorded signal"
    );

    // The confirmed message is in the chat as a voice message, with no error shown.
    let st = harness.state();
    assert_eq!(st.widget.active_toast(), None);
    assert!(!st.widget.compose.voice_recorder.is_active());
    let sent = st
        .widget
        .inner()
        .messages()
        .iter()
        .find(|m| m.id.0 == SENT_ID)
        .expect("the sent voice message is in the chat");
    match &sent.media {
        Some(HistoryMedia::Voice(voice)) => {
            assert_eq!(voice.duration_secs, audio.duration as u32);
            assert!(!voice.waveform_5bit.is_empty());
        }
        other => panic!("expected a voice bubble, got {other:?}"),
    }
}

#[test]
fn a_failed_voice_send_is_shown_not_swallowed() {
    let fake = FakeRpcClient::new();
    fake.respond_with(Arc::new(|ctor, _raw| {
        if ctor == tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        if ctor == tl_fns::messages::SendMedia::CONSTRUCTOR_ID {
            return Some(Err(InvocationError::Rpc(RpcError::new(
                400,
                "VOICE_MESSAGES_FORBIDDEN",
            ))));
        }
        None
    }));
    let mut harness = start_chat(&fake);

    hold_mic(&mut harness, 2.0);

    assert_eq!(
        fake.recorded_of::<tl_fns::messages::SendMedia>().len(),
        1,
        "the voice message was sent"
    );
    // `sendMessageFail`'s fallback shows the error type.
    assert_eq!(
        harness.state().widget.active_toast(),
        Some("VOICE_MESSAGES_FORBIDDEN")
    );
}

#[test]
fn a_hold_shorter_than_a_second_sends_nothing() {
    // tdesktop's capture returns no bytes under one second (`kCaptureFrequency` samples) and
    // `VoiceRecordBar::stopRecording` discards the empty result.
    let fake = FakeRpcClient::new();
    accept_uploads_and_sends(&fake);
    let mut harness = start_chat(&fake);

    hold_mic(&mut harness, 0.3);

    assert!(
        fake.recorded_of::<tl_fns::upload::SaveFilePart>()
            .is_empty()
    );
    assert!(fake.recorded_of::<tl_fns::messages::SendMedia>().is_empty());
    assert!(!harness.state().widget.compose.voice_recorder.is_active());
}

#[test]
fn the_synthetic_source_records_wall_time_even_with_sparse_frames() {
    // An idle window repaints a few times a second; the old source fed a fixed 80 ms per
    // frame, so a 3 s recording at 4 fps came out as about 1 s.
    let mut rec = VoiceRecorderController::new();
    rec.start_locked(10.0);
    assert!(!rec.uses_hardware_capture());
    for frame in 1..=12 {
        rec.tick(10.0 + f64::from(frame) * 0.25);
    }
    let voice = rec.finish(13.0).expect("three seconds are recorded");
    assert!(voice.ogg.starts_with(b"OggS"));
    assert_eq!(voice.attachment.duration_secs, 3);
    assert_eq!(voice.attachment.size_bytes, voice.ogg.len() as u64);
    assert_eq!(
        waveform_decode_5bit(&voice.attachment.waveform_5bit).len(),
        100
    );
}

#[test]
fn a_paused_span_is_not_recorded() {
    let mut rec = VoiceRecorderController::new();
    rec.start_locked(0.0);
    for frame in 1..=10 {
        rec.tick(f64::from(frame) * 0.1);
    }
    rec.toggle_pause(1.0);
    for frame in 1..=50 {
        rec.tick(1.0 + f64::from(frame) * 0.1);
    }
    rec.toggle_pause(6.0);
    for frame in 1..=10 {
        rec.tick(6.0 + f64::from(frame) * 0.1);
    }
    let voice = rec.finish(7.0).expect("two seconds are recorded");
    assert_eq!(
        voice.attachment.duration_secs, 2,
        "the 5 s pause is not in the file"
    );
}
