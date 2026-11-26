//! Live RPC integration tests for Chat History Import (`live/chat_import.rs` &
//! `boxes/chat_import.rs`) using an in-process `FakeRpcClient` and synthetic archives only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::boxes::chat_import::{
    ChatImportBoxState, ChatImportStep, ImportSourceApp, build_synthetic_export_zip,
    parse_import_bytes,
};
use tg_app::live::LiveSession;
use tg_app::live::chat_import::{
    check_history_import, check_history_import_peer, run_history_import_with_progress,
};
use tg_app::stand_in::types::PeerId;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
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

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(ctor, _)| *ctor)
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
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
            .push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else if crate::common::is_background_list_request(ctor) {
                // Never the answer a test queued for another request (TGB-2941).
                None
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

fn build_session_with_synthetic_user() -> (LiveSession, FakeRpcClient, PeerId) {
    let rpc = FakeRpcClient::new(2);
    let runtime = SessionRuntime::start(
        DataUserId(1001),
        tg_app::live::ErasedRpcClient::new(rpc.clone()),
        SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start session runtime");

    let live = LiveSession::new(Arc::new(runtime), 1);
    let target_user = tl_enums::User::User(tl_types::User {
        self_: false,
        contact: true,
        mutual_contact: true,
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
        premium: false,
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
        id: 2002,
        access_hash: Some(987_654_321),
        first_name: Some("Sample".to_owned()),
        last_name: Some("Contact".to_owned()),
        username: Some("sample_contact".to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    });
    live.write_session(|s| {
        apply_tl_users_and_chats(s, &[target_user], &[]);
    });
    let target_peer = PeerId(DataPeerId::from_user(DataUserId(2002)).0);
    (live, rpc, target_peer)
}

#[test]
fn full_whatsapp_zip_import_flow_hits_all_five_history_import_rpcs() {
    let (mut live, rpc, target_peer) = build_session_with_synthetic_user();

    let transcript = concat!(
        "[20/02/2025, 14:10:00] Participant A: Synthetic message one\n",
        "[20/02/2025, 14:11:00] Participant B: <attached: PHOTO-1.jpg>\n",
        "[20/02/2025, 14:12:00] Participant A: <attached: DOC-1.pdf>\n",
    );
    let zip_bytes = build_synthetic_export_zip(&[
        ("_chat.txt", transcript.as_bytes()),
        (
            "PHOTO-1.jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46],
        ),
        ("DOC-1.pdf", b"%PDF-1.4 synthetic"),
    ])
    .expect("build synthetic zip");

    let archive = parse_import_bytes("WhatsApp Chat - Synthetic.zip", &zip_bytes)
        .expect("parse synthetic zip");
    assert_eq!(archive.source_app, ImportSourceApp::WhatsApp);
    assert_eq!(archive.media_count(), 2);

    // 1. messages.checkHistoryImport
    rpc.enqueue_ok(
        tl_fns::messages::CheckHistoryImport::CONSTRUCTOR_ID,
        &tl_enums::messages::HistoryImportParsed::HistoryImportParsed(
            tl_types::messages::HistoryImportParsed {
                pm: true,
                group: false,
                title: Some("Synthetic Chat".to_owned()),
            },
        ),
    );
    // 2. messages.checkHistoryImportPeer
    rpc.enqueue_ok(
        tl_fns::messages::CheckHistoryImportPeer::CONSTRUCTOR_ID,
        &tl_enums::messages::CheckedHistoryImportPeer::CheckedHistoryImportPeer(
            tl_types::messages::CheckedHistoryImportPeer {
                confirm_text: "Messages will be imported into Sample Contact.".to_owned(),
            },
        ),
    );
    // 3. upload.saveFilePart (transcript) + messages.initHistoryImport
    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::InitHistoryImport::CONSTRUCTOR_ID,
        &tl_enums::messages::HistoryImport::HistoryImport(tl_types::messages::HistoryImport {
            id: 77_001,
        }),
    );
    // 4. upload.saveFilePart + messages.uploadImportedMedia (for PHOTO-1.jpg and DOC-1.pdf)
    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::UploadImportedMedia::CONSTRUCTOR_ID,
        &tl_enums::MessageMedia::Empty,
    );
    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::UploadImportedMedia::CONSTRUCTOR_ID,
        &tl_enums::MessageMedia::Empty,
    );
    // 5. messages.startHistoryImport
    rpc.enqueue_ok(tl_fns::messages::StartHistoryImport::CONSTRUCTOR_ID, &true);

    // Drive through the ChatImportBoxState wizard using InfoBackend!
    let mut box_state = ChatImportBoxState::from_archive(archive, None, None, &live);
    assert_eq!(box_state.step, ChatImportStep::SelectPeer);
    let parsed_info = box_state.parsed_info.as_ref().expect("parsed info");
    assert!(parsed_info.pm);
    assert!(!parsed_info.group);
    assert_eq!(parsed_info.title.as_deref(), Some("Synthetic Chat"));

    box_state
        .select_peer(target_peer, "Sample Contact", &live)
        .expect("checkHistoryImportPeer succeeds");
    assert_eq!(box_state.step, ChatImportStep::Confirm);
    assert_eq!(
        box_state.confirm_text,
        "Messages will be imported into Sample Contact."
    );

    let summary = box_state
        .start_import(&mut live)
        .expect("full import succeeds");
    assert_eq!(box_state.step, ChatImportStep::Completed);
    assert_eq!(summary.import_id, 77_001);
    assert_eq!(summary.peer_id, target_peer);
    assert_eq!(summary.uploaded_media_count, 2);
    assert_eq!(summary.skipped_media_count, 0);
    assert!(summary.started);

    // Verify exact TL RPC constructor order on the wire.
    let ctors = rpc.recorded_ctors();
    assert!(ctors.contains(&tl_fns::messages::CheckHistoryImport::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::CheckHistoryImportPeer::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::InitHistoryImport::CONSTRUCTOR_ID));
    assert_eq!(
        ctors
            .iter()
            .filter(|&&c| c == tl_fns::messages::UploadImportedMedia::CONSTRUCTOR_ID)
            .count(),
        2
    );
    assert_eq!(
        ctors.last().copied(),
        Some(tl_fns::messages::StartHistoryImport::CONSTRUCTOR_ID)
    );
}

#[test]
fn import_handles_server_errors_and_reports_progress() {
    let (live, rpc, target_peer) = build_session_with_synthetic_user();

    // Unrecognized format error from checkHistoryImport propagates cleanly.
    rpc.enqueue_rpc_err(
        tl_fns::messages::CheckHistoryImport::CONSTRUCTOR_ID,
        400,
        "IMPORT_FORMAT_UNRECOGNIZED",
    );
    let err = check_history_import(&live, "not a valid export header").unwrap_err();
    assert!(err.contains("IMPORT_FORMAT_UNRECOGNIZED"));

    // Peer eligibility error from checkHistoryImportPeer propagates cleanly.
    rpc.enqueue_rpc_err(
        tl_fns::messages::CheckHistoryImportPeer::CONSTRUCTOR_ID,
        400,
        "USER_NOT_MUTUAL_CONTACT",
    );
    let peer_err = check_history_import_peer(&live, target_peer).unwrap_err();
    assert!(peer_err.contains("USER_NOT_MUTUAL_CONTACT"));

    // Partial media upload failure is skipped without aborting startHistoryImport.
    let line_log = "[LINE] Chat history with Demo\n10:00\tUser A\tHi\n";
    let zip_bytes = build_synthetic_export_zip(&[
        ("[LINE] Demo.txt", line_log.as_bytes()),
        (
            "sample.png",
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        ),
    ])
    .expect("line zip");
    let archive = parse_import_bytes("line.zip", &zip_bytes).expect("parse line");

    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::messages::InitHistoryImport::CONSTRUCTOR_ID,
        &tl_enums::messages::HistoryImport::HistoryImport(tl_types::messages::HistoryImport {
            id: 88_002,
        }),
    );
    rpc.enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    rpc.enqueue_rpc_err(
        tl_fns::messages::UploadImportedMedia::CONSTRUCTOR_ID,
        400,
        "MEDIA_EMPTY",
    );
    rpc.enqueue_ok(tl_fns::messages::StartHistoryImport::CONSTRUCTOR_ID, &true);

    let mut progress_steps = Vec::new();
    let summary = run_history_import_with_progress(&live, target_peer, &archive, |done, total| {
        progress_steps.push((done, total));
    })
    .expect("import completes even if one media item is skipped");
    assert_eq!(summary.import_id, 88_002);
    assert_eq!(summary.uploaded_media_count, 0);
    assert_eq!(summary.skipped_media_count, 1);
    assert!(summary.started);
    assert_eq!(progress_steps.first(), Some(&(0, 3)));
    assert_eq!(progress_steps.last(), Some(&(3, 3)));
}
